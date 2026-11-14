use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// Format unix millis as a ClickHouse DateTime64(3) literal (UTC).
pub fn ch_time(ms: i64) -> String {
    let dt = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let f = time::macros::format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"
    );
    dt.format(&f).unwrap_or_default()
}

pub fn iso_time(ms: i64) -> String {
    let dt = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    dt.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

/// Parse a timestamp given as RFC 3339 string, unix seconds (float) or unix millis.
pub fn parse_timestamp(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::String(s) => {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                .ok()
                .map(|d| (d.unix_timestamp_nanos() / 1_000_000) as i64)
        }
        serde_json::Value::Number(n) => {
            let f = n.as_f64()?;
            // Heuristic: values above 1e11 are millis, otherwise seconds.
            Some(if f > 1e11 { f as i64 } else { (f * 1000.0) as i64 })
        }
        _ => None,
    }
}

/// Parse a relative or absolute time expression used by query APIs: "24h", "7d", "30m", RFC 3339, or millis.
pub fn parse_time_expr(s: &str, now: i64) -> Option<i64> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix('m').and_then(|n| n.parse::<i64>().ok()) {
        return Some(now - num * 60_000);
    }
    if let Some(num) = s.strip_suffix('h').and_then(|n| n.parse::<i64>().ok()) {
        return Some(now - num * 3_600_000);
    }
    if let Some(num) = s.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
        return Some(now - num * 86_400_000);
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    parse_timestamp(&serde_json::Value::String(s.to_string()))
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_exprs() {
        assert_eq!(parse_time_expr("1h", 10_000_000), Some(10_000_000 - 3_600_000));
        assert_eq!(parse_time_expr("2d", 200_000_000), Some(200_000_000 - 172_800_000));
        assert!(parse_time_expr("2026-01-01T00:00:00Z", 0).is_some());
        assert_eq!(ch_time(0), "1970-01-01 00:00:00.000");
    }
}

/// Make model-written text safe to forward to Slack, email or the UI: the model reads crash and
/// feedback text that anyone can send, so it could be steered into emitting phishing links.
/// Markdown links keep their label, bare URLs are dropped, and control characters go.
pub fn defang_llm_text(s: &str) -> String {
    static MD_LINK: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"\[([^\]\n]{0,200})\]\([^)\s]*\)").unwrap()
    });
    static URL: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"(?i)\b(?:https?://|www\.)\S+").unwrap());
    let s = MD_LINK.replace_all(s, "$1");
    let s = URL.replace_all(&s, "[link removed]");
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}

#[cfg(test)]
mod defang_tests {
    #[test]
    fn strips_links() {
        assert_eq!(
            super::defang_llm_text(
                "See [here](https://evil.example/x) or https://evil.example/y now"
            ),
            "See here or [link removed] now"
        );
        assert_eq!(
            super::defang_llm_text("Errors up 20%.\nWEB-1 is new."),
            "Errors up 20%.\nWEB-1 is new."
        );
    }
}
