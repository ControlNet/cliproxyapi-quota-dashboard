//! Provider-specific quota fetching and parsing.
//!
//! Each provider recipe mirrors the upstream request CLIProxyAPI's own CLI clients
//! make, proxied through `POST /v0/management/api-call` so `$TOKEN$` is substituted
//! server-side by CLIProxyAPI itself.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Map, Value};

use crate::cliproxy::Cli;
use crate::helpers::{fmt_amount, jbool, jnum, jstr, jstring, normalize_reset, pct1, window};

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const GEMINI_QUOTA_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";
const GEMINI_ASSIST_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const KIMI_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";

const CLAUDE_WINDOWS: &[(&str, &str)] = &[
    ("five_hour", "5h"),
    ("seven_day", "7天"),
    ("seven_day_oauth_apps", "7天·Apps"),
    ("seven_day_opus", "7天·Opus"),
    ("seven_day_sonnet", "7天·Sonnet"),
    ("seven_day_cowork", "7天·Cowork"),
];

const CODEX_FIVE_HOUR_SECS: f64 = 18_000.0;
const CODEX_WEEK_SECS: f64 = 604_800.0;

/// Parsed quota data for a single account, merged into the unified payload later.
pub struct AccountData {
    pub plan: Option<String>,
    pub windows: Vec<Value>,
    pub extra: Map<String, Value>,
}

impl AccountData {
    fn new() -> Self {
        Self {
            plan: None,
            windows: Vec::new(),
            extra: Map::new(),
        }
    }
}

fn ok_status(status: u16) -> bool {
    (200..300).contains(&status)
}

fn require_obj<'a>(body: &'a Value, provider: &str) -> Result<&'a Value, String> {
    body.is_object()
        .then_some(body)
        .ok_or_else(|| format!("{provider} 配额响应格式异常"))
}

/// Fetch and parse quota for one credential. `provider` is already normalized.
pub async fn fetch(cli: &Cli, provider: &str, file: &Value) -> Result<AccountData, String> {
    let idx = jstr(file, &["auth_index", "authIndex", "index", "id"])
        .ok_or("凭据缺少 auth_index，无法查询配额")?
        .to_string();
    match provider {
        "claude" => claude(cli, &idx).await,
        "codex" => codex(cli, &idx, file).await,
        "gemini" => gemini(cli, &idx, file).await,
        "kimi" => kimi(cli, &idx).await,
        _ => Err("该账号类型暂不支持配额查询".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Claude
// ---------------------------------------------------------------------------

async fn claude(cli: &Cli, idx: &str) -> Result<AccountData, String> {
    let headers = [
        ("Authorization", "Bearer $TOKEN$"),
        ("Content-Type", "application/json"),
        ("anthropic-beta", "oauth-2025-04-20"),
    ];
    let usage = cli
        .api_call(idx, "GET", CLAUDE_USAGE_URL, &headers, None)
        .await?;
    if !ok_status(usage.status) {
        return Err(format!("Claude 配额接口返回状态 {}", usage.status));
    }
    let payload = require_obj(&usage.body, "Claude")?;

    let mut out = AccountData::new();
    for &(key, label) in CLAUDE_WINDOWS {
        let Some(w) = payload.get(key).filter(|v| v.is_object()) else {
            continue;
        };
        out.windows.push(window(
            label,
            jnum(w, &["utilization"]).map(pct1),
            normalize_reset(w.get("resets_at")),
            None,
        ));
    }

    // Profile is optional: only used to badge Pro/Max plans.
    if let Ok(profile) = cli
        .api_call(idx, "GET", CLAUDE_PROFILE_URL, &headers, None)
        .await
    {
        if ok_status(profile.status) {
            if let Some(account) = profile.body.get("account") {
                if jbool(account, &["has_claude_max"]).unwrap_or(false) {
                    out.plan = Some("Max".to_string());
                } else if jbool(account, &["has_claude_pro"]).unwrap_or(false) {
                    out.plan = Some("Pro".to_string());
                }
            }
        }
    }

    if let Some(extra_usage) = payload.get("extra_usage").filter(|v| v.is_object()) {
        if let (Some(limit), Some(used)) = (
            jnum(extra_usage, &["monthly_limit"]),
            jnum(extra_usage, &["used_credits"]),
        ) {
            out.extra.insert(
                "credit_summary".into(),
                json!(format!(
                    "额外用量 ${:.2} / ${:.2}",
                    used / 100.0,
                    limit / 100.0
                )),
            );
        }
    }

    if out.windows.is_empty() && out.plan.is_none() && out.extra.is_empty() {
        return Err("Claude 未返回可展示的配额数据".to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Codex (ChatGPT)
// ---------------------------------------------------------------------------

async fn codex(cli: &Cli, idx: &str, file: &Value) -> Result<AccountData, String> {
    let account_id = codex_account_id(file).ok_or("缺少 ChatGPT 账号标识，无法查询配额")?;
    let ua = "codex_cli_rs/0.76.0 (Debian 13.0.0; x86_64) WindowsTerminal";
    let headers = [
        ("Authorization", "Bearer $TOKEN$"),
        ("Content-Type", "application/json"),
        ("User-Agent", ua),
        ("Chatgpt-Account-Id", account_id.as_str()),
    ];

    let res = cli
        .api_call(idx, "GET", CODEX_USAGE_URL, &headers, None)
        .await?;
    if !ok_status(res.status) {
        return Err(format!("Codex 配额接口返回状态 {}", res.status));
    }
    let payload = require_obj(&res.body, "Codex")?;

    let mut out = AccountData::new();
    if let Some(rl) = obj_field(payload, &["rate_limit", "rateLimit"]) {
        push_codex_windows(&mut out, rl, "5h", "7天");
    }
    if let Some(cr) = obj_field(payload, &["code_review_rate_limit", "codeReviewRateLimit"]) {
        push_codex_windows(&mut out, cr, "CR·5h", "CR·7天");
    }

    let plan_raw = jstring(payload, &["plan_type", "planType"]).or_else(|| codex_file_plan(file));
    out.plan = plan_raw.map(|p| {
        let lower = p.to_lowercase();
        match lower.as_str() {
            "plus" => "Plus".to_string(),
            "pro" => "Pro".to_string(),
            "team" => "Team".to_string(),
            "free" => "Free".to_string(),
            _ => lower,
        }
    });

    if out.windows.is_empty() {
        return Err("Codex 未返回配额窗口".to_string());
    }
    Ok(out)
}

fn push_codex_windows(out: &mut AccountData, rl: &Value, short_label: &str, weekly_label: &str) {
    let primary = obj_field(rl, &["primary_window", "primaryWindow"]);
    let secondary = obj_field(rl, &["secondary_window", "secondaryWindow"]);

    let mut five: Option<&Value> = None;
    let mut week: Option<&Value> = None;
    for cand in [primary, secondary].into_iter().flatten() {
        match jnum(cand, &["limit_window_seconds", "limitWindowSeconds"]) {
            Some(s) if (s - CODEX_FIVE_HOUR_SECS).abs() < f64::EPSILON && five.is_none() => {
                five = Some(cand)
            }
            Some(s) if (s - CODEX_WEEK_SECS).abs() < f64::EPSILON && week.is_none() => {
                week = Some(cand)
            }
            _ => {}
        }
    }
    if five.is_none() && primary.is_some() && !same_ref(primary, week) {
        five = primary;
    }
    if week.is_none() && secondary.is_some() && !same_ref(secondary, five) {
        week = secondary;
    }

    let reached = jbool(rl, &["limit_reached", "limitReached"]).unwrap_or(false);
    let allowed = jbool(rl, &["allowed"]).unwrap_or(true);

    for (w, label) in [(five, short_label), (week, weekly_label)] {
        let Some(w) = w else { continue };
        let used = jnum(w, &["used_percent", "usedPercent"])
            .map(pct1)
            .or_else(|| (reached || !allowed).then_some(100.0));
        let resets = [
            "reset_after_seconds",
            "resetAfterSeconds",
            "reset_at",
            "resetAt",
        ]
        .iter()
        .find_map(|k| normalize_reset(w.get(k)));
        out.windows.push(window(label, used, resets, None));
    }
}

fn codex_account_id(file: &Value) -> Option<String> {
    for holder in [Some(file), file.get("metadata"), file.get("attributes")]
        .into_iter()
        .flatten()
    {
        if let Some(token) = holder.get("id_token") {
            if let Some(id) = token_field(token, &["chatgpt_account_id", "chatgptAccountId"]) {
                return Some(id);
            }
        }
    }
    None
}

fn codex_file_plan(file: &Value) -> Option<String> {
    if let Some(p) = jstring(file, &["plan_type", "planType"]) {
        return Some(p);
    }
    for holder in [
        file.get("metadata"),
        file.get("attributes"),
        file.get("id_token"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(p) = jstring(
            holder,
            &[
                "chatgpt_plan_type",
                "chatgptPlanType",
                "plan_type",
                "planType",
            ],
        ) {
            return Some(p);
        }
    }
    None
}

/// Extract a field from an id_token that may be an object, a JSON string, or a JWT.
fn token_field(token: &Value, keys: &[&str]) -> Option<String> {
    match token {
        Value::Object(_) => jstring(token, keys),
        Value::String(s) => {
            if let Ok(v) = serde_json::from_str::<Value>(s) {
                return jstring(&v, keys);
            }
            let parts: Vec<&str> = s.split('.').collect();
            if parts.len() >= 2 {
                if let Ok(bytes) = URL_SAFE_NO_PAD.decode(parts[1].trim_end_matches('=')) {
                    if let Ok(text) = String::from_utf8(bytes) {
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            return jstring(&v, keys);
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn same_ref(a: Option<&Value>, b: Option<&Value>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if std::ptr::eq(x, y))
}

fn obj_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|k| v.get(*k))
        .filter(|f| f.is_object())
}

// ---------------------------------------------------------------------------
// Gemini CLI
// ---------------------------------------------------------------------------

async fn gemini(cli: &Cli, idx: &str, file: &Value) -> Result<AccountData, String> {
    let project = gemini_project_id(file).ok_or("缺少 Google 项目标识，无法查询配额")?;
    let headers = [
        ("Authorization", "Bearer $TOKEN$"),
        ("Content-Type", "application/json"),
    ];

    let quota = cli
        .api_call(
            idx,
            "POST",
            GEMINI_QUOTA_URL,
            &headers,
            Some(json!({ "project": project }).to_string()),
        )
        .await?;
    if !ok_status(quota.status) {
        return Err(format!("Gemini 配额接口返回状态 {}", quota.status));
    }
    let payload = require_obj(&quota.body, "Gemini")?;

    let mut out = AccountData::new();
    if let Some(buckets) = payload.get("buckets").and_then(Value::as_array) {
        for b in buckets {
            if !b.is_object() {
                continue;
            }
            let Some(model) = jstring(b, &["modelId", "model_id"]) else {
                continue;
            };
            let label = match jstring(b, &["tokenType", "token_type"]) {
                Some(t) => format!("{model} · {t}"),
                None => model,
            };
            let used = jnum(b, &["remainingFraction", "remaining_fraction"])
                .map(|rf| pct1((1.0 - rf.clamp(0.0, 1.0)) * 100.0));
            let caption = jnum(b, &["remainingAmount", "remaining_amount"])
                .map(|a| format!("剩余 {}", fmt_amount(a)));
            let resets = ["resetTime", "reset_time"]
                .iter()
                .find_map(|k| normalize_reset(b.get(k)));
            out.windows.push(window(&label, used, resets, caption));
        }
    }

    // Best-effort tier + Google One AI credits.
    let assist_body = json!({
        "cloudaicompanionProject": project,
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
            "duetProject": project,
        }
    });
    if let Ok(assist) = cli
        .api_call(
            idx,
            "POST",
            GEMINI_ASSIST_URL,
            &headers,
            Some(assist_body.to_string()),
        )
        .await
    {
        if ok_status(assist.status) && assist.body.is_object() {
            let tier = ["paidTier", "paid_tier", "currentTier", "current_tier"]
                .iter()
                .find_map(|k| assist.body.get(k))
                .filter(|t| t.is_object());
            if let Some(tier) = tier {
                let tier_id = jstr(tier, &["id"]).unwrap_or("").to_lowercase();
                out.plan = match tier_id.as_str() {
                    "free-tier" => Some("免费层级".to_string()),
                    "legacy-tier" => Some("Legacy 层级".to_string()),
                    "standard-tier" => Some("标准层级".to_string()),
                    "g1-pro-tier" => Some("Pro 层级".to_string()),
                    "g1-ultra-tier" => Some("Ultra 层级".to_string()),
                    "" => None,
                    other => Some(other.to_string()),
                };
                if let Some(credits) = obj_arr(tier, &["availableCredits", "available_credits"]) {
                    let g1: f64 = credits
                        .iter()
                        .filter(|c| {
                            jstring(c, &["creditType", "credit_type"]).as_deref()
                                == Some("GOOGLE_ONE_AI")
                        })
                        .filter_map(|c| jnum(c, &["creditAmount", "credit_amount"]))
                        .sum();
                    if g1 > 0.0 {
                        out.extra.insert(
                            "g1_credits".into(),
                            json!(format!("Google One AI 剩余额度 {}", fmt_amount(g1))),
                        );
                    }
                }
            }
        }
    }

    if out.windows.is_empty() {
        return Err("Gemini 未返回配额桶数据".to_string());
    }
    Ok(out)
}

fn gemini_project_id(file: &Value) -> Option<String> {
    for holder in [Some(file), file.get("metadata"), file.get("attributes")]
        .into_iter()
        .flatten()
    {
        let Some(acc) = jstring(holder, &["account"]) else {
            continue;
        };
        // Account strings look like "user@gmail.com (my-project-id)" — take the last parenthesized group.
        if let Some(open) = acc.rfind('(') {
            if let Some(close) = acc.rfind(')') {
                if close > open + 1 {
                    let pid = acc[open + 1..close].trim();
                    if !pid.is_empty() {
                        return Some(pid.to_string());
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Kimi
// ---------------------------------------------------------------------------

async fn kimi(cli: &Cli, idx: &str) -> Result<AccountData, String> {
    let headers = [("Authorization", "Bearer $TOKEN$")];
    let res = cli
        .api_call(idx, "GET", KIMI_USAGE_URL, &headers, None)
        .await?;
    if !ok_status(res.status) {
        return Err(format!("Kimi 配额接口返回状态 {}", res.status));
    }
    let payload = require_obj(&res.body, "Kimi")?;
    let limits = payload
        .get("limits")
        .and_then(Value::as_array)
        .ok_or("Kimi 未返回额度列表")?;

    let mut out = AccountData::new();
    for l in limits {
        if !l.is_object() {
            continue;
        }
        let detail = l.get("detail").filter(|d| d.is_object()).unwrap_or(l);
        let Some(name) =
            jstring(l, &["title", "name", "scope"]).or_else(|| jstring(detail, &["title", "name"]))
        else {
            continue;
        };
        let used = jnum(detail, &["used"]).or_else(|| jnum(l, &["used"]));
        let limit_v = jnum(detail, &["limit"]).or_else(|| jnum(l, &["limit"]));
        let remaining = jnum(detail, &["remaining"]).or_else(|| jnum(l, &["remaining"]));

        let used_pct = match (remaining, limit_v) {
            (Some(r), Some(lim)) if lim > 0.0 => {
                Some(pct1((1.0 - (r / lim).clamp(0.0, 1.0)) * 100.0))
            }
            _ => match (used, limit_v) {
                (Some(u), Some(lim)) if lim > 0.0 => Some(pct1((u / lim).clamp(0.0, 1.0) * 100.0)),
                _ => None,
            },
        };
        let caption = match (used, limit_v) {
            (Some(u), Some(lim)) => Some(format!("{} / {}", fmt_amount(u), fmt_amount(lim))),
            _ => remaining.map(|r| format!("剩余 {}", fmt_amount(r))),
        };
        let mut resets = None;
        for k in [
            "resetAt",
            "reset_at",
            "resetTime",
            "reset_time",
            "resetIn",
            "reset_in",
            "ttl",
        ] {
            if let Some(r) = normalize_reset(detail.get(k)).or_else(|| normalize_reset(l.get(k))) {
                resets = Some(r);
                break;
            }
        }
        out.windows.push(window(&name, used_pct, resets, caption));
    }

    if out.windows.is_empty() {
        return Err("Kimi 未返回可用额度".to_string());
    }
    Ok(out)
}

fn obj_arr<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    keys.iter()
        .find_map(|k| v.get(*k))
        .and_then(Value::as_array)
}
