//! Minimal single-page dashboard for CLIProxyAPI OAuth account quotas.
//!
//! Users authenticate with any valid CLIProxyAPI API key; quota data is fetched
//! server-side with the management key and never exposed to the browser.

mod cliproxy;
mod helpers;
mod providers;
mod quota;
mod session;

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

use crate::session::SessionSigner;

/// How long an authenticated session cookie stays valid.
const SESSION_TTL_SECS: u64 = 24 * 60 * 60;
/// Server-side TTL for aggregated /api/quota responses.
const QUOTA_CACHE_TTL: Duration = Duration::from_secs(20);
/// Login rate limiting: failures allowed per IP within one window before lockout.
const LOGIN_MAX_FAILURES: u32 = 8;
const LOGIN_WINDOW: Duration = Duration::from_secs(5 * 60);

struct AppState {
    cli: cliproxy::Cli,
    signer: SessionSigner,
    quota_key_secret: Vec<u8>,
    quota_cache: Mutex<Option<(Instant, Value)>>,
    login_failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl AppState {
    async fn cached_quota(&self) -> Option<Value> {
        let guard = self.quota_cache.lock().await;
        let (at, value) = guard.as_ref()?;
        (at.elapsed() < QUOTA_CACHE_TTL).then(|| value.clone())
    }

    async fn store_quota(&self, value: Value) {
        *self.quota_cache.lock().await = Some((Instant::now(), value));
    }

    async fn login_locked(&self, ip: IpAddr) -> bool {
        let map = self.login_failures.lock().await;
        match map.get(&ip) {
            Some((count, start)) if *count >= LOGIN_MAX_FAILURES => start.elapsed() < LOGIN_WINDOW,
            _ => false,
        }
    }

    async fn record_login_failure(&self, ip: IpAddr) {
        let mut map = self.login_failures.lock().await;
        let now = Instant::now();
        map.retain(|_, (_, start)| now.duration_since(*start) < LOGIN_WINDOW * 2);
        map.entry(ip)
            .and_modify(|(count, start)| {
                if now.duration_since(*start) >= LOGIN_WINDOW {
                    *start = now;
                    *count = 0;
                }
                *count += 1;
            })
            .or_insert_with(|| (1, now));
    }

    async fn clear_login_failures(&self, ip: IpAddr) {
        self.login_failures.lock().await.remove(&ip);
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let base_url = match std::env::var("CLIPROXY_BASE_URL") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!("error: CLIPROXY_BASE_URL is required (e.g. http://127.0.0.1:8317)");
            std::process::exit(1);
        }
    };
    let management_key = match std::env::var("CLIPROXY_MANAGEMENT_KEY") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!("error: CLIPROXY_MANAGEMENT_KEY is required");
            std::process::exit(1);
        }
    };
    let secret = match std::env::var("SESSION_SECRET") {
        Ok(s) if s.len() >= 32 => s.into_bytes(),
        Ok(_) => {
            eprintln!("warning: SESSION_SECRET is shorter than 32 chars; using an ephemeral random secret instead");
            rand::random::<[u8; 32]>().to_vec()
        }
        Err(_) => {
            eprintln!("warning: SESSION_SECRET not set; sessions will not survive restarts");
            rand::random::<[u8; 32]>().to_vec()
        }
    };

    let cli = cliproxy::Cli::new(base_url, management_key).expect("failed to init http client");
    let signer = SessionSigner::new(secret.clone());
    let state = Arc::new(AppState {
        cli,
        signer,
        quota_key_secret: secret,
        quota_cache: Mutex::new(None),
        login_failures: Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/api/session", get(session_status))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/quota", get(quota))
        .fallback(not_found)
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind port");
    tracing::info!("listening on http://{addr}");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("server error");
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn session_status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Json<Value> {
    Json(json!({ "authenticated": is_authenticated(&state, &headers).await }))
}

async fn login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    let ip = addr.ip();
    if state.login_locked(ip).await {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "尝试次数过多，请稍后再试");
    }
    let Some(api_key) = body
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return api_error(StatusCode::BAD_REQUEST, "缺少 api_key 字段");
    };

    match state.cli.validate_api_key(api_key).await {
        Ok(true) => {
            state.clear_login_failures(ip).await;
            let cookie = state.signer.issue(SESSION_TTL_SECS);
            let set_cookie = format!(
                "session={cookie}; HttpOnly; SameSite=Lax; Path=/; Max-Age={SESSION_TTL_SECS}"
            );
            (
                [(axum::http::header::SET_COOKIE, set_cookie)],
                Json(json!({"ok": true})),
            )
                .into_response()
        }
        Ok(false) => {
            state.record_login_failure(ip).await;
            api_error(StatusCode::UNAUTHORIZED, "无效的 API Key")
        }
        Err(e) => {
            tracing::warn!("login validation failed: {e}");
            api_error(StatusCode::BAD_GATEWAY, "无法连接 CLIProxyAPI，请稍后再试")
        }
    }
}

async fn logout() -> Response {
    let set_cookie = "session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0";
    (
        [(axum::http::header::SET_COOKIE, set_cookie)],
        Json(json!({"ok": true})),
    )
        .into_response()
}

async fn quota(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !is_authenticated(&state, &headers).await {
        return api_error(StatusCode::UNAUTHORIZED, "未登录或会话已过期");
    }
    if let Some(cached) = state.cached_quota().await {
        return (StatusCode::OK, Json(cached)).into_response();
    }
    match quota::aggregate(&state.cli, &state.quota_key_secret).await {
        Ok(payload) => {
            state.store_quota(payload.clone()).await;
            (StatusCode::OK, Json(payload)).into_response()
        }
        Err(msg) => api_error(StatusCode::BAD_GATEWAY, &msg),
    }
}

async fn not_found() -> Response {
    api_error(StatusCode::NOT_FOUND, "not found")
}

async fn is_authenticated(state: &AppState, headers: &HeaderMap) -> bool {
    extract_session(headers)
        .map(|token| state.signer.verify(&token))
        .unwrap_or(false)
}

fn extract_session(headers: &HeaderMap) -> Option<String> {
    for header in headers.get_all(axum::http::header::COOKIE) {
        let Ok(raw) = header.to_str() else { continue };
        for pair in raw.split(';') {
            if let Some((name, value)) = pair.trim().split_once('=') {
                if name == "session" && !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
