//! Tolerant JSON accessors and timestamp normalization utilities.

use serde_json::{json, Value};

/// First key in `keys` whose value is a non-empty trimmed string.
pub fn jstr<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(s) = v.get(*key).and_then(Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// Owned version of [`jstr`].
pub fn jstring(v: &Value, keys: &[&str]) -> Option<String> {
    jstr(v, keys).map(str::to_string)
}

/// Numeric field tolerating numeric strings ("42").
pub fn jnum(v: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(found) = v.get(key) {
            if let Some(n) = num_of(found) {
                return Some(n);
            }
        }
    }
    None
}

fn num_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Boolean field tolerating numbers and common truthy strings.
pub fn jbool(v: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        match v.get(*key) {
            Some(Value::Bool(b)) => return Some(*b),
            Some(Value::Number(n)) => return Some(n.as_i64().unwrap_or(0) != 0),
            Some(Value::String(s)) => match s.trim().to_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => return Some(true),
                "0" | "false" | "no" | "off" => return Some(false),
                _ => {}
            },
            _ => {}
        }
    }
    None
}

/// Current unix timestamp in seconds.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build one normalized quota window object.
pub fn window(
    name: &str,
    used_percent: Option<f64>,
    resets_at: Option<String>,
    caption: Option<String>,
) -> Value {
    json!({
        "name": name,
        "used_percent": used_percent,
        "resets_at": resets_at,
        "caption": caption,
    })
}

/// Normalize heterogeneous reset markers into an ISO-8601 UTC string.
///
/// Accepts: epoch seconds/ms, "seconds until reset" durations (< 1 year),
/// pure-digit strings, and passes ISO-like strings through unchanged.
pub fn normalize_reset(v: Option<&Value>) -> Option<String> {
    let v = v?;
    match v {
        Value::Number(_) => iso_from_number(num_of(v)?),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Some(n) = num_of(v) {
                iso_from_number(n)
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

const SECS_IN_YEAR: f64 = 365.0 * 24.0 * 3600.0;

fn iso_from_number(n: f64) -> Option<String> {
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    if n < SECS_IN_YEAR {
        // Treat small values as "seconds until reset" relative to now.
        iso_from_unix(unix_now().saturating_add(n as u64))
    } else {
        let secs = if n > 1e12 { n / 1000.0 } else { n };
        iso_from_unix(secs as u64)
    }
}

fn iso_from_unix(secs: u64) -> Option<String> {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    ))
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Round a percentage to one decimal place.
pub fn pct1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Format an amount without noisy trailing zeros.
pub fn fmt_amount(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as u64)
    } else {
        format!("{x}")
    }
}
