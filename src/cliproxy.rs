//! Thin HTTP client for the CLIProxyAPI service and management APIs.

use serde_json::{json, Map, Value};
use std::time::Duration;

use crate::helpers::jnum;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Envelope returned by `POST /v0/management/api-call`.
pub struct ApiCall {
    pub status: u16,
    pub body: Value,
}

pub struct Cli {
    http: reqwest::Client,
    base_url: String,
    management_key: String,
}

impl Cli {
    pub fn new(mut base_url: String, management_key: String) -> reqwest::Result<Self> {
        while base_url.ends_with('/') {
            base_url.pop();
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            base_url,
            management_key,
        })
    }

    /// Validate a user API key against `GET /v1/models`.
    pub async fn validate_api_key(&self, key: &str) -> Result<bool, String> {
        let resp = self
            .http
            .get(format!("{}/v1/models", self.base_url))
            .header("Authorization", format!("Bearer {key}"))
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        Ok(resp.status().is_success())
    }

    /// List configured credential files via the management API.
    pub async fn auth_files(&self) -> Result<Vec<Value>, String> {
        let resp = self
            .http
            .get(format!("{}/v0/management/auth-files", self.base_url))
            .header("Authorization", format!("Bearer {}", self.management_key))
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("management api returned status {}", resp.status()));
        }
        let parsed: Value = resp
            .json()
            .await
            .map_err(|e| format!("invalid json: {e}"))?;
        let arr = parsed
            .get("files")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| parsed.as_array().cloned())
            .unwrap_or_default();
        Ok(arr.into_iter().filter(Value::is_object).collect())
    }

    /// Proxy a request to an upstream provider through the management API;
    /// CLIProxyAPI substitutes `$TOKEN$` with the selected credential's access token.
    ///
    /// The POST request-body field is named `data`, matching CLIProxyAPI's contract.
    pub async fn api_call(
        &self,
        auth_index: &str,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        data: Option<String>,
    ) -> Result<ApiCall, String> {
        let mut header_obj = Map::new();
        for (k, v) in headers {
            header_obj.insert((*k).to_string(), Value::String((*v).to_string()));
        }
        let mut payload = json!({
            "auth_index": auth_index,
            "method": method,
            "url": url,
            "header": Value::Object(header_obj),
        });
        if let Some(d) = data {
            payload["data"] = Value::String(d);
        }

        let resp = self
            .http
            .post(format!("{}/v0/management/api-call", self.base_url))
            .header("Authorization", format!("Bearer {}", self.management_key))
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("api-call returned status {}", resp.status()));
        }
        let envelope: Value = resp
            .json()
            .await
            .map_err(|e| format!("invalid json: {e}"))?;

        let status = jnum(&envelope, &["status_code", "statusCode"]).unwrap_or(500.0) as u16;
        let body = match envelope.get("body") {
            Some(Value::String(s)) => {
                serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
            }
            Some(other) => other.clone(),
            None => Value::Null,
        };
        Ok(ApiCall { status, body })
    }
}
