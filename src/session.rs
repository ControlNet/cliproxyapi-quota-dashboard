//! Stateless HMAC-SHA256 signed session cookies.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::Hmac;
use hmac::Mac;
use serde_json::Value;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

pub struct SessionSigner {
    key: Vec<u8>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl SessionSigner {
    pub fn new(key: Vec<u8>) -> Self {
        Self { key }
    }

    /// Issue a signed cookie value: `<payload_b64url>.<sig_hex>`.
    pub fn issue(&self, ttl_secs: u64) -> String {
        let exp = unix_now() + ttl_secs;
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        format!("{payload}.{}", self.sign(payload.as_bytes()))
    }

    /// Constant-time signature verification plus expiry check.
    pub fn verify(&self, token: &str) -> bool {
        let Some((payload, sig_hex)) = token.split_once('.') else {
            return false;
        };
        let Ok(sig) = decode_hex(sig_hex) else {
            return false;
        };
        if payload.len() > 128 {
            return false;
        }
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.key) else {
            return false;
        };
        mac.update(payload.as_bytes());
        if mac.verify_slice(&sig).is_err() {
            return false;
        }
        let Ok(payload_bytes) = URL_SAFE_NO_PAD.decode(payload) else {
            return false;
        };
        let Ok(parsed) = serde_json::from_slice::<Value>(&payload_bytes) else {
            return false;
        };
        parsed
            .get("exp")
            .and_then(Value::as_u64)
            .is_some_and(|exp| exp > unix_now())
    }

    fn sign(&self, data: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("hmac accepts any key length");
        mac.update(data);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}
