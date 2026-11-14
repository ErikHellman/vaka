//! SMS alert channel. One provider per server, chosen with `VAKA_SMS_PROVIDER`:
//! 46elks (default), Twilio or Sinch. Rule targets are comma-separated E.164 numbers.
//! Messages are kept to a single SMS segment, and a server-wide daily cap
//! (`VAKA_SMS_MAX_PER_DAY`) bounds cost.

use crate::db::RowExt;
use crate::state::AppState;
use crate::util::now_ms;
use serde_json::Value;
use std::time::Duration;

/// Supported providers, in order of preference.
pub const PROVIDERS: &[&str] = &["46elks", "twilio", "sinch"];

#[derive(Clone, Debug, Default)]
pub struct SmsConfig {
    /// `46elks`, `twilio` or `sinch`.
    pub provider: String,
    /// Sender: a phone number, or (46elks, Sinch in some countries) an alphanumeric id of up to 11 characters.
    pub from: String,
    /// 46elks API username, Twilio Account SID or Sinch service plan id.
    pub user: Option<String>,
    /// 46elks API password, Twilio auth token or Sinch API token.
    pub secret: Option<String>,
    /// API base URL; defaults to the provider's public API (Sinch: the `VAKA_SINCH_REGION` host).
    pub api_url: String,
    /// SMS deliveries allowed per 24 hours across the server (0 = unlimited).
    pub max_per_day: i64,
}

impl SmsConfig {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let provider = var("VAKA_SMS_PROVIDER").map(|p| p.to_lowercase().replace(['-', '_', ' '], "")).unwrap_or_else(|| "46elks".into());
        let provider = match provider.as_str() {
            "elks" | "46elks" => "46elks".to_string(),
            other => other.to_string(),
        };
        let (user, secret, default_url) = match provider.as_str() {
            "twilio" => (var("VAKA_TWILIO_ACCOUNT_SID"), var("VAKA_TWILIO_AUTH_TOKEN"), "https://api.twilio.com".to_string()),
            "sinch" => {
                let region = var("VAKA_SINCH_REGION").unwrap_or_else(|| "us".into());
                (var("VAKA_SINCH_SERVICE_PLAN_ID"), var("VAKA_SINCH_API_TOKEN"), format!("https://{region}.sms.api.sinch.com"))
            }
            _ => (var("VAKA_46ELKS_USERNAME"), var("VAKA_46ELKS_PASSWORD"), "https://api.46elks.com".to_string()),
        };
        SmsConfig {
            from: var("VAKA_SMS_FROM").unwrap_or_else(|| "Vaka".into()),
            api_url: var("VAKA_SMS_API_URL").unwrap_or(default_url).trim_end_matches('/').to_string(),
            max_per_day: var("VAKA_SMS_MAX_PER_DAY").and_then(|v| v.parse().ok()).unwrap_or(100),
            provider,
            user,
            secret,
        }
    }

    /// Why SMS cannot be sent, or None when it is configured.
    pub fn missing(&self) -> Option<String> {
        let (u, s) = match self.provider.as_str() {
            "46elks" => ("VAKA_46ELKS_USERNAME", "VAKA_46ELKS_PASSWORD"),
            "twilio" => ("VAKA_TWILIO_ACCOUNT_SID", "VAKA_TWILIO_AUTH_TOKEN"),
            "sinch" => ("VAKA_SINCH_SERVICE_PLAN_ID", "VAKA_SINCH_API_TOKEN"),
            other => return Some(format!("SMS alerts need VAKA_SMS_PROVIDER to be one of {} (got {other})", PROVIDERS.join(", "))),
        };
        if self.user.is_none() || self.secret.is_none() {
            return Some(format!("SMS alerts need {u} and {s} (VAKA_SMS_PROVIDER={})", self.provider));
        }
        None
    }
}

/// Marker in errors that must not be retried (configuration or cap problems).
pub const NO_RETRY: &str = "[sms: not retried]";

/// Parse and validate a comma-separated list of E.164 numbers (`+46701234567`).
pub fn parse_numbers(target: &str) -> Result<Vec<String>, String> {
    let nums: Vec<String> = target.split(',').map(|s| s.chars().filter(|c| !matches!(c, ' ' | '-' | '(' | ')')).collect::<String>()).filter(|s| !s.is_empty()).collect();
    if nums.is_empty() {
        return Err("sms target must be one or more comma-separated phone numbers in E.164 format, like +46701234567".into());
    }
    if nums.len() > 10 {
        return Err("sms target can have at most 10 phone numbers".into());
    }
    for n in &nums {
        let digits = n.strip_prefix('+').unwrap_or("");
        if !(8..=15).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) || digits.starts_with('0') {
            return Err(format!("{n} is not an E.164 phone number (use +<country code><number>, like +46701234567)"));
        }
    }
    Ok(nums)
}

// ---------------------------------------------------------------------------
// Message text
// ---------------------------------------------------------------------------

const GSM_BASIC: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
const GSM_EXT: &str = "^{}\\[~]|€\x0c";

/// Septets a character takes in GSM-7, or None if it needs UCS-2.
fn gsm_cost(c: char) -> Option<usize> {
    if GSM_BASIC.contains(c) {
        Some(1)
    } else if GSM_EXT.contains(c) {
        Some(2)
    } else {
        None
    }
}

/// Length in the unit of a single segment and that segment's limit (160 GSM-7 septets or 70 UCS-2 characters).
fn seg_len(s: &str) -> (usize, usize) {
    match s.chars().map(gsm_cost).sum::<Option<usize>>() {
        Some(n) => (n, 160),
        None => (s.encode_utf16().count(), 70),
    }
}

/// Replace characters that would force UCS-2 (halving the length) with GSM-safe look-alikes.
fn gsm_friendly(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '`' | '´' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            '→' => '>',
            '\t' | '\n' | '\r' => ' ',
            c => c,
        })
        .collect();
    // Collapse runs of spaces (keeping a leading or trailing one, which joins the parts).
    let mut out = String::with_capacity(s.len());
    for c in s.replace('…', "...").chars() {
        if !(c == ' ' && out.ends_with(' ')) {
            out.push(c);
        }
    }
    out
}

/// Build `head` + `body` + `tail` so the whole fits one segment, shortening `body` (then `head`) with "...".
pub fn fit(head: &str, body: &str, tail: &str) -> String {
    let (head, body, tail) = (gsm_friendly(head), gsm_friendly(body), gsm_friendly(tail));
    let full = format!("{head}{body}{tail}");
    let (len, max) = seg_len(&full);
    if len <= max {
        return full;
    }
    // Drop characters from the end of body until it fits.
    let mut chars: Vec<char> = body.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cut: String = chars.iter().collect::<String>().trim_end().to_string();
        let cand = format!("{head}{cut}...{tail}");
        let (l, m) = seg_len(&cand);
        if l <= m {
            return cand;
        }
    }
    // Still too long (very long project name or URL): keep the tail, cut the head.
    let mut chars: Vec<char> = head.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand = format!("{}...{tail}", chars.iter().collect::<String>());
        let (l, m) = seg_len(&cand);
        if l <= m {
            return cand;
        }
    }
    tail.chars().take(70).collect()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// The single-segment SMS text for an alert payload.
pub fn message(trigger: &str, payload: &Value) -> String {
    let project = s(&payload["project"]["name"]);
    let url = s(&payload["url"]);
    let tail = if url.is_empty() { String::new() } else { format!(" {url}") };
    match trigger {
        "release_created" => fit(&format!("Vaka: new release {} in ", s(&payload["release"]["version"])), project, ""),
        "perf_regression" => fit("Vaka: ", &crate::alerts::perf_text(payload), &tail),
        "new_feedback" => {
            let fb = &payload["feedback"];
            fit(&format!("Vaka feedback {} [{project}]: ", s(&fb["short_id"])), s(&fb["message"]), &tail)
        }
        "digest" => {
            let projects = payload["projects"].as_array().cloned().unwrap_or_default();
            let n = |v: &Value| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|x| x.parse().ok())).unwrap_or(0);
            let errors: i64 = projects.iter().map(|p| n(&p["errors"])).sum();
            let new_issues: i64 = projects.iter().map(|p| n(&p["new_issue_count"])).sum();
            let anomalies: usize = projects.iter().map(|p| p["anomalies"].as_array().map(|a| a.len()).unwrap_or(0)).sum();
            let title = payload["title"].as_str().unwrap_or("Vaka daily digest");
            let body = if projects.is_empty() {
                format!("no activity in {}.", s(&payload["period"]["label"]))
            } else {
                let worst = projects.iter().max_by_key(|p| n(&p["errors"])).map(|p| s(&p["project"]["name"]).to_string()).unwrap_or_default();
                let mut b = format!("{errors} errors, {new_issues} new issues in {} project{}", projects.len(), if projects.len() == 1 { "" } else { "s" });
                if anomalies > 0 {
                    b.push_str(&format!(", {anomalies} anomal{}", if anomalies == 1 { "y" } else { "ies" }));
                }
                if errors > 0 && projects.len() > 1 {
                    b.push_str(&format!("; most in {worst}"));
                }
                b
            };
            fit(&format!("{title}: "), &body, "")
        }
        _ => {
            let issue = &payload["issue"];
            let what = match trigger {
                "new_issue" => "New issue",
                "regression" => "Regression",
                "spike" => "Spike",
                "threshold" => "Threshold reached",
                "issue_resolved" => "Resolved",
                "issue_assigned" => "Assigned",
                "issue_commented" => "New comment",
                "test" => "Test alert",
                _ => "Alert",
            };
            let short = s(&issue["short_id"]);
            let id = if short.is_empty() { String::new() } else { format!("{short} ") };
            let extra = payload["details"]["last_hour"].as_i64().map(|h| format!(" ({h}/h)")).unwrap_or_default();
            fit(&format!("Vaka {what} [{project}]{extra}: {id}"), s(&issue["title"]), &tail)
        }
    }
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

/// SMS deliveries recorded in the last 24 hours (successful or still being retried).
async fn sent_last_day(state: &AppState) -> anyhow::Result<i64> {
    Ok(sqlx::query(
        "SELECT COUNT(*) AS n FROM alert_deliveries d JOIN alert_rules r ON r.id = d.rule_id \
         WHERE r.channel = 'sms' AND d.created_at > $1 AND d.status IN ('ok', 'retrying')",
    )
    .bind(now_ms() - 86_400_000)
    .fetch_one(&state.db)
    .await?
    .i("n"))
}

/// Send an alert as SMS to every number in `target`.
pub async fn send(state: &AppState, target: &str, trigger: &str, payload: &Value) -> anyhow::Result<String> {
    let cfg = &state.config.sms;
    if let Some(why) = cfg.missing() {
        anyhow::bail!("{why} {NO_RETRY}");
    }
    let numbers = parse_numbers(target).map_err(|e| anyhow::anyhow!("{e} {NO_RETRY}"))?;
    if cfg.max_per_day > 0 && sent_last_day(state).await? >= cfg.max_per_day {
        anyhow::bail!("daily SMS cap reached (VAKA_SMS_MAX_PER_DAY={}) {NO_RETRY}", cfg.max_per_day);
    }
    let text = message(trigger, payload);
    let mut ids = Vec::new();
    match cfg.provider.as_str() {
        "sinch" => ids.push(sinch(state, cfg, &numbers, &text).await?),
        _ => {
            for to in &numbers {
                ids.push(if cfg.provider == "twilio" { twilio(state, cfg, to, &text).await? } else { elks(state, cfg, to, &text).await? });
            }
        }
    }
    Ok(format!("{} SMS to {} number{} ({})", cfg.provider, numbers.len(), if numbers.len() == 1 { "" } else { "s" }, ids.join(", ")))
}

async fn check(provider: &str, resp: reqwest::Response, id_key: &str) -> anyhow::Result<String> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // 4xx other than 429 means a bad number, sender or credentials: retrying will not help.
        let no_retry = status.is_client_error() && status.as_u16() != 429;
        anyhow::bail!("{provider} returned {status}: {}{}", crate::util::truncate(&body, 200), if no_retry { format!(" {NO_RETRY}") } else { String::new() });
    }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    Ok(v[id_key].as_str().unwrap_or("sent").to_string())
}

/// 46elks: `POST /a1/sms` with Basic auth and a form body. <https://46elks.com/docs/send-sms>
async fn elks(state: &AppState, cfg: &SmsConfig, to: &str, text: &str) -> anyhow::Result<String> {
    let resp = state
        .http
        .post(format!("{}/a1/sms", cfg.api_url))
        .basic_auth(cfg.user.as_deref().unwrap_or(""), cfg.secret.as_deref())
        .form(&[("from", cfg.from.as_str()), ("to", to), ("message", text)])
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    check("46elks", resp, "id").await
}

/// Twilio: `POST /2010-04-01/Accounts/{sid}/Messages.json` with Basic auth and a form body.
async fn twilio(state: &AppState, cfg: &SmsConfig, to: &str, text: &str) -> anyhow::Result<String> {
    let sid = cfg.user.as_deref().unwrap_or("");
    let resp = state
        .http
        .post(format!("{}/2010-04-01/Accounts/{sid}/Messages.json", cfg.api_url))
        .basic_auth(sid, cfg.secret.as_deref())
        .form(&[("From", cfg.from.as_str()), ("To", to), ("Body", text)])
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    check("Twilio", resp, "sid").await
}

/// Sinch: `POST /xms/v1/{service_plan_id}/batches` with a Bearer token; one batch for all numbers.
async fn sinch(state: &AppState, cfg: &SmsConfig, to: &[String], text: &str) -> anyhow::Result<String> {
    let plan = cfg.user.as_deref().unwrap_or("");
    let resp = state
        .http
        .post(format!("{}/xms/v1/{plan}/batches", cfg.api_url))
        .bearer_auth(cfg.secret.as_deref().unwrap_or(""))
        .json(&serde_json::json!({"from": cfg.from, "to": to, "body": text}))
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    check("Sinch", resp, "id").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers() {
        assert_eq!(parse_numbers("+46 70-123 45 67, +14155550100").unwrap(), vec!["+46701234567", "+14155550100"]);
        assert!(parse_numbers("0701234567").is_err());
        assert!(parse_numbers("").is_err());
        assert!(parse_numbers("+46abc12345").is_err());
    }

    #[test]
    fn single_segment() {
        let long = "TypeError: Cannot read properties of undefined (reading 'map') at renderList in components/List.tsx while rendering the checkout page for a logged in user";
        let m = message("new_issue", &json!({"project": {"name": "Web"}, "url": "https://vaka.example.com/ui/projects/web/issues/abc123", "issue": {"short_id": "WEB-3", "title": long}}));
        assert!(m.starts_with("Vaka New issue [Web]: WEB-3 TypeError"), "{m}");
        assert!(m.ends_with("... https://vaka.example.com/ui/projects/web/issues/abc123"), "{m}");
        assert_eq!(seg_len(&m).1, 160);
        assert!(seg_len(&m).0 <= 160, "{}", seg_len(&m).0);

        // Smart quotes and ellipses are mapped so the message stays GSM-7.
        let m = message("test", &json!({"project": {"name": "Vaka test"}, "url": "https://v", "issue": {"title": "it’s “quoted” …"}}));
        assert_eq!(m, "Vaka Test alert [Vaka test]: it's \"quoted\" ... https://v");

        // Emoji forces UCS-2: 70 characters.
        let m = message("new_feedback", &json!({"project": {"name": "App"}, "url": "https://v/f", "feedback": {"short_id": "APP-F1", "message": "🙂 ".repeat(60)}}));
        assert!(seg_len(&m).1 == 70 && seg_len(&m).0 <= 70, "{m}");
        assert!(m.ends_with(" https://v/f"), "{m}");
    }

    #[test]
    fn digest_summary() {
        let p = json!({"title": "Vaka daily digest", "projects": [
            {"project": {"name": "Web"}, "errors": 12, "new_issue_count": 2, "anomalies": [{}]},
            {"project": {"name": "API"}, "errors": "30", "new_issue_count": 1, "anomalies": []}
        ]});
        assert_eq!(message("digest", &p), "Vaka daily digest: 42 errors, 3 new issues in 2 projects, 1 anomaly; most in API");
        let quiet = json!({"title": "Vaka weekly digest", "projects": [], "period": {"label": "the last 7 days"}});
        assert_eq!(message("digest", &quiet), "Vaka weekly digest: no activity in the last 7 days.");
    }
}
