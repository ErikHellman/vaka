//! Server-side scrubbing of secrets and common PII patterns in error payloads.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

const FILTERED: &str = "[Filtered]";

static DEFAULT_KEYS: &[&str] = &[
    "password", "passwd", "pwd", "secret", "token", "api_key", "apikey", "access_token",
    "refresh_token", "authorization", "auth", "cookie", "set-cookie", "session", "credit_card",
    "card_number", "cvv", "ssn", "private_key",
];

static EMAIL: Lazy<Regex> = Lazy::new(|| Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap());
static CARD: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(?:\d[ -]?){13,19}\b").unwrap());
static BEARER: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}").unwrap());
static KV_SECRET: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(password|passwd|pwd|secret|token|api_key|apikey|access_token)=([^&\s]+)").unwrap()
});

pub struct Scrubber {
    keys: Vec<String>,
}

impl Scrubber {
    pub fn new(extra_fields: &str) -> Self {
        let mut keys: Vec<String> = DEFAULT_KEYS.iter().map(|s| s.to_string()).collect();
        keys.extend(
            extra_fields
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty()),
        );
        Self { keys }
    }

    fn sensitive_key(&self, key: &str) -> bool {
        let k = key.to_lowercase();
        self.keys.iter().any(|s| k.contains(s.as_str()))
    }

    pub fn scrub_str(&self, s: &str) -> String {
        let s = BEARER.replace_all(s, "$1 [Filtered]");
        let s = KV_SECRET.replace_all(&s, "$1=[Filtered]");
        let s = EMAIL.replace_all(&s, "[email]");
        CARD.replace_all(&s, "[card]").to_string()
    }

    pub fn scrub_value(&self, v: &mut Value) {
        match v {
            Value::String(s) => *s = self.scrub_str(s),
            Value::Array(a) => a.iter_mut().for_each(|x| self.scrub_value(x)),
            Value::Object(o) => {
                for (k, val) in o.iter_mut() {
                    if self.sensitive_key(k) {
                        *val = Value::String(FILTERED.into());
                    } else {
                        self.scrub_value(val);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scrubs_keys_and_patterns() {
        let s = Scrubber::new("ssn_extra");
        let mut v = json!({
            "password": "hunter2",
            "msg": "user bob@example.com paid with 4111 1111 1111 1111",
            "url": "https://x.io/?token=abc123&page=2",
            "nested": {"Authorization": "Bearer abcdefghijk", "ssn_extra": "1"}
        });
        s.scrub_value(&mut v);
        assert_eq!(v["password"], "[Filtered]");
        assert_eq!(v["msg"], "user [email] paid with [card]");
        assert_eq!(v["url"], "https://x.io/?token=[Filtered]&page=2");
        assert_eq!(v["nested"]["Authorization"], "[Filtered]");
        assert_eq!(v["nested"]["ssn_extra"], "[Filtered]");
    }
}
