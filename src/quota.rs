//! Aggregate every OAuth credential's quota into the unified dashboard payload.

use futures::StreamExt;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::cliproxy::Cli;
use crate::helpers::{jbool, jstring, unix_now};
use crate::providers;

/// Max concurrent upstream quota fetches per aggregation round.
const FETCH_CONCURRENCY: usize = 4;
type HmacSha256 = Hmac<Sha256>;

pub async fn aggregate(cli: &Cli, key_secret: &[u8]) -> Result<Value, String> {
    let files = cli
        .auth_files()
        .await
        .map_err(|e| format!("无法读取账号列表：{e}"))?;

    let accounts: Vec<Result<Value, String>> = futures::stream::iter(
        files
            .into_iter()
            .map(|file| async move { build_account(cli, &file, key_secret).await }),
    )
    .buffer_unordered(FETCH_CONCURRENCY)
    .collect()
    .await;

    let mut accounts: Vec<Value> = accounts.into_iter().collect::<Result<_, _>>()?;
    accounts.sort_by(compare_accounts);

    Ok(json!({
        "fetched_at": unix_now(),
        "accounts": accounts,
    }))
}

async fn build_account(cli: &Cli, file: &Value, key_secret: &[u8]) -> Result<Value, String> {
    let raw_provider = jstring(file, &["provider", "type"])
        .unwrap_or_else(|| "other".to_string())
        .to_lowercase();
    let provider = normalize_provider(&raw_provider);
    // Privacy: raw upstream identity fields stay server-side. Numeric indices are safe to expose;
    // all other credential identities are reduced to a process-secret HMAC pseudonym.
    let auth_index = jstring(file, &["auth_index", "authIndex", "index"])
        .filter(|tag| !tag.is_empty() && tag.chars().all(|ch| ch.is_ascii_digit()));
    let label = auth_index.as_deref().map_or_else(
        || provider_display(provider).to_string(),
        |tag| format!("{} #{}", provider_display(provider), tag),
    );
    let key = account_key(provider, file, auth_index.as_deref(), key_secret)?;
    let disabled = jbool(file, &["disabled"]).unwrap_or(false);

    let mut account = json!({
        "key": key,
        "label": label,
        "provider": provider,
        "plan": null,
        "disabled": disabled,
        "windows": [],
        "extra": {},
        "error": null,
    });

    if provider == "other" {
        account["error"] = json!("该账号类型暂不支持配额查询");
        return Ok(account);
    }
    if disabled {
        // Skip upstream calls for disabled credentials; UI renders them dimmed.
        return Ok(account);
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
    Ok(account)
}

fn account_key(
    provider: &str,
    file: &Value,
    numeric_index: Option<&str>,
    key_secret: &[u8],
) -> Result<String, String> {
    if let Some(index) = numeric_index {
        return Ok(format!("{provider}:{index}"));
    }

    let identity = jstring(
        file,
        &[
            "id",
            "auth_index",
            "authIndex",
            "index",
            "name",
            "filename",
            "file",
        ],
    )
    .unwrap_or_else(|| file.to_string());
    let mut mac =
        HmacSha256::new_from_slice(key_secret).map_err(|_| "无法生成账号匿名标识".to_string())?;
    mac.update(provider.as_bytes());
    mac.update(&[0]);
    mac.update(identity.as_bytes());
    let digest = mac.finalize().into_bytes();
    let short_hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("{provider}:h:{short_hex}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY_SECRET: &[u8] = b"test-only-account-key-secret";

    fn test_cli() -> Cli {
        Cli::new(
            "http://127.0.0.1".to_string(),
            "test-management-key".to_string(),
        )
        .expect("test client should be valid")
    }

    #[tokio::test]
    async fn account_label_is_anonymous_when_upstream_label_contains_identity() {
        for identity in [
            "alice@example.com",
            "alice-work-account",
            "claude-alice.json",
        ] {
            let file = json!({
                "id": "credential-file.json",
                "auth_index": "7",
                "provider": "claude",
                "label": identity,
                "disabled": true,
            });

            let account = build_account(&test_cli(), &file, TEST_KEY_SECRET)
                .await
                .expect("account should build");

            assert_eq!(account["key"], "claude:7");
            assert_eq!(account["label"], "Claude #7");
        }
    }

    #[tokio::test]
    async fn account_payload_omits_unused_upstream_identity_fields() {
        let file = json!({
            "id": "credential-file.json",
            "auth_index": "7",
            "provider": "claude",
            "label": "alice@example.com",
            "email": "email-sentinel@example.com",
            "account": "username-sentinel",
            "name": "credential-name-sentinel.json",
            "status": "identity-status-sentinel",
            "disabled": true,
        });

        let account = build_account(&test_cli(), &file, TEST_KEY_SECRET)
            .await
            .expect("account should build");
        let serialized = account.to_string();

        for identity in [
            "credential-file.json",
            "alice@example.com",
            "email-sentinel@example.com",
            "username-sentinel",
            "credential-name-sentinel.json",
            "identity-status-sentinel",
        ] {
            assert!(!serialized.contains(identity));
        }
    }

    #[tokio::test]
    async fn account_label_omits_nonnumeric_upstream_index() {
        let file = json!({
            "id": "credential-file.json",
            "auth_index": "alice@example.com",
            "provider": "claude",
            "disabled": true,
        });

        let account = build_account(&test_cli(), &file, TEST_KEY_SECRET)
            .await
            .expect("account should build");
        let account_again = build_account(&test_cli(), &file, TEST_KEY_SECRET)
            .await
            .expect("account should build");

        assert_eq!(account["key"], account_again["key"]);
        assert!(account["key"]
            .as_str()
            .is_some_and(|key| key.starts_with("claude:h:")));
        assert!(!account.to_string().contains("alice@example.com"));
        assert!(!account.to_string().contains("credential-file.json"));
        assert_eq!(account["label"], "Claude");
    }
}
