//! Aggregate every OAuth credential's quota into the unified dashboard payload.

use futures::StreamExt;
use serde_json::{json, Value};

use crate::cliproxy::Cli;
use crate::helpers::{jbool, jstring, unix_now};
use crate::providers;

/// Max concurrent upstream quota fetches per aggregation round.
const FETCH_CONCURRENCY: usize = 4;

pub async fn aggregate(cli: &Cli) -> Result<Value, String> {
    let files = cli
        .auth_files()
        .await
        .map_err(|e| format!("无法读取账号列表：{e}"))?;

    let accounts: Vec<Value> = futures::stream::iter(
        files
            .into_iter()
            .map(|file| async move { build_account(cli, &file).await }),
    )
    .buffer_unordered(FETCH_CONCURRENCY)
    .collect()
    .await;

    let mut accounts = accounts;
    accounts.sort_by(compare_accounts);

    Ok(json!({
        "fetched_at": unix_now(),
        "accounts": accounts,
    }))
}

async fn build_account(cli: &Cli, file: &Value) -> Value {
    let id = jstring(file, &["id"]).unwrap_or_default();
    let raw_provider = jstring(file, &["provider", "type"])
        .unwrap_or_else(|| "other".to_string())
        .to_lowercase();
    let provider = normalize_provider(&raw_provider);
    // Privacy: upstream identity (email/account/filename) must never reach the browser.
    // Show the operator's own label if set; otherwise an anonymous per-provider tag.
    let label = match jstring(file, &["label"]).filter(|s| !s.is_empty()) {
        Some(custom) => custom,
        None => {
            let tag = jstring(file, &["auth_index", "authIndex", "index", "id"])
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone());
            format!("{} #{}", provider_display(provider), tag)
        }
    };
    let disabled = jbool(file, &["disabled"]).unwrap_or(false);

    let mut account = json!({
        "id": id,
        "label": label,
        "provider": provider,
        "plan": null,
        "disabled": disabled,
        "status": jstring(file, &["status"]),
        "windows": [],
        "extra": {},
        "error": null,
    });

    if provider == "other" {
        account["error"] = json!("该账号类型暂不支持配额查询");
        return account;
    }
    if disabled {
        // Skip upstream calls for disabled credentials; UI renders them dimmed.
        return account;
    }

    match providers::fetch(cli, provider, file).await {
        Ok(data) => {
            account["plan"] = json!(data.plan);
            account["windows"] = Value::Array(data.windows);
            account["extra"] = Value::Object(data.extra);
        }
        Err(msg) => {
            account["error"] = json!(msg);
        }
    }
    account
}

fn provider_display(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "gemini" => "Gemini",
        "kimi" => "Kimi",
        _ => "其他",
    }
}

fn normalize_provider(raw: &str) -> &'static str {
    match raw {
        "claude" | "anthropic" | "claude-pro" | "claude-max" => "claude",
        "codex" | "openai" | "chatgpt" => "codex",
        "gemini-cli" | "gemini" => "gemini",
        "kimi" | "moonshot" => "kimi",
        _ => "other",
    }
}

fn rank(provider: &str) -> u8 {
    match provider {
        "claude" => 0,
        "codex" => 1,
        "gemini" => 2,
        "kimi" => 3,
        _ => 4,
    }
}

fn compare_accounts(a: &Value, b: &Value) -> std::cmp::Ordering {
    let pa = a["provider"].as_str().unwrap_or("other");
    let pb = b["provider"].as_str().unwrap_or("other");
    rank(pa).cmp(&rank(pb)).then_with(|| {
        a["label"]
            .as_str()
            .unwrap_or("")
            .cmp(b["label"].as_str().unwrap_or(""))
    })
}
