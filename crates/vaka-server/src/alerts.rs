//! Alerts: webhooks and email (Resend) on new issues, regressions and spikes.

use crate::db::RowExt;
use crate::state::{AppState, ProjectInfo};
use crate::util::{new_id, now_ms};
use serde_json::json;
use sha2::Sha256;
use std::time::Duration;

pub const TRIGGERS: &[&str] = &["new_issue", "regression", "spike"];
pub const CHANNELS: &[&str] = &["webhook", "email"];
const SPIKE_COOLDOWN_MS: i64 = 6 * 3_600_000;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Rule {
    pub id: String,
    pub project_id: Option<String>,
    pub name: String,
    pub trigger: String,
    pub channel: String,
    pub target: String,
    pub threshold: i64,
    pub enabled: bool,
}

pub fn rule_from_row(r: &sqlx::any::AnyRow) -> Rule {
    Rule {
        id: r.s("id"),
        project_id: r.os("project_id").filter(|p| !p.is_empty()),
        name: r.s("name"),
        trigger: r.s("trigger"),
        channel: r.s("channel"),
        target: r.s("target"),
        threshold: r.i("threshold"),
        enabled: r.i("enabled") != 0,
    }
}

pub const RULE_COLS: &str = "id, project_id, name, trigger, channel, target, secret, threshold, enabled";

async fn rules_for(state: &AppState, project_id: &str, trigger: &str) -> anyhow::Result<Vec<(Rule, String)>> {
    let rows = sqlx::query(&format!(
        "SELECT {RULE_COLS} FROM alert_rules WHERE enabled = 1 AND trigger = $1 AND (project_id IS NULL OR project_id = '' OR project_id = $2)"
    ))
    .bind(trigger)
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.iter().map(|r| (rule_from_row(r), r.s("secret"))).collect())
}

/// Fire-and-forget notification for a new or regressed issue.
pub fn notify_issue(state: &AppState, project: &ProjectInfo, issue_id: &str, trigger: &str) {
    let state = state.clone();
    let project = project.clone();
    let issue_id = issue_id.to_string();
    let trigger = trigger.to_string();
    tokio::spawn(async move {
        // Give the event a moment to reach ClickHouse so links show data.
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Err(e) = dispatch(&state, &project, &issue_id, &trigger, None).await {
            tracing::warn!("alert dispatch failed: {e:#}");
        }
    });
}

async fn dispatch(state: &AppState, project: &ProjectInfo, issue_id: &str, trigger: &str, extra: Option<serde_json::Value>) -> anyhow::Result<()> {
    let rules = rules_for(state, &project.id, trigger).await?;
    if rules.is_empty() {
        return Ok(());
    }
    let issue = crate::query::get_issue_row(state, issue_id).await?;
    let Some(issue) = issue else { return Ok(()) };
    for (rule, secret) in rules {
        if trigger == "spike" {
            if let Some(extra) = &extra {
                let count = extra["last_hour"].as_i64().unwrap_or(0);
                if count < rule.threshold.max(1) {
                    continue;
                }
            }
            if recently_delivered(state, &rule.id, issue_id).await? {
                continue;
            }
        }
        let payload = json!({
            "trigger": trigger,
            "project": {"id": project.id, "slug": project.slug, "name": project.name},
            "issue": issue,
            "url": format!("{}/ui/projects/{}/issues/{}", state.config.public_url, project.slug, issue_id),
            "api_url": format!("{}/api/v1/issues/{}", state.config.public_url, issue_id),
            "details": extra,
            "sent_at": crate::util::iso_time(now_ms()),
        });
        let result = match rule.channel.as_str() {
            "webhook" => send_webhook(state, &rule.target, &secret, &payload).await,
            "email" => send_email(state, &rule.target, trigger, &payload).await,
            other => Err(anyhow::anyhow!("unknown channel {other}")),
        };
        let (status, detail) = match &result {
            Ok(d) => ("ok", d.clone()),
            Err(e) => ("failed", e.to_string()),
        };
        sqlx::query("INSERT INTO alert_deliveries (id, rule_id, issue_id, trigger, status, detail, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)")
            .bind(new_id())
            .bind(&rule.id)
            .bind(issue_id)
            .bind(trigger)
            .bind(status)
            .bind(crate::util::truncate(&detail, 1000))
            .bind(now_ms())
            .execute(&state.db)
            .await?;
    }
    Ok(())
}

async fn recently_delivered(state: &AppState, rule_id: &str, issue_id: &str) -> anyhow::Result<bool> {
    let row = sqlx::query("SELECT COUNT(*) AS n FROM alert_deliveries WHERE rule_id = $1 AND issue_id = $2 AND created_at > $3 AND status = 'ok'")
        .bind(rule_id)
        .bind(issue_id)
        .bind(now_ms() - SPIKE_COOLDOWN_MS)
        .fetch_one(&state.db)
        .await?;
    Ok(row.i("n") > 0)
}

pub fn sign(secret: &str, body: &[u8]) -> String {
    use hmac_lite::hmac_sha256;
    hex::encode(hmac_sha256(secret.as_bytes(), body))
}

/// Tiny HMAC-SHA256 so we don't need another dependency.
mod hmac_lite {
    use sha2::{Digest, Sha256};
    pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
        let mut k = [0u8; 64];
        if key.len() > 64 {
            k[..32].copy_from_slice(&Sha256::digest(key));
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut ipad = [0x36u8; 64];
        let mut opad = [0x5cu8; 64];
        for i in 0..64 {
            ipad[i] ^= k[i];
            opad[i] ^= k[i];
        }
        let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
        Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
    }
}

async fn send_webhook(state: &AppState, url: &str, secret: &str, payload: &serde_json::Value) -> anyhow::Result<String> {
    let body = serde_json::to_vec(payload)?;
    let mut req = state
        .http
        .post(url)
        .header("content-type", "application/json")
        .header("user-agent", "Vaka-Webhook/1")
        .timeout(Duration::from_secs(10));
    if !secret.is_empty() {
        req = req.header("x-vaka-signature", format!("sha256={}", sign(secret, &body)));
    }
    let resp = req.body(body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("webhook returned {status}");
    }
    Ok(format!("HTTP {status}"))
}

async fn send_email(state: &AppState, to: &str, trigger: &str, payload: &serde_json::Value) -> anyhow::Result<String> {
    let Some(key) = &state.config.resend_api_key else {
        anyhow::bail!("email alerts need VAKA_RESEND_API_KEY");
    };
    let issue = &payload["issue"];
    let what = match trigger {
        "new_issue" => "New issue",
        "regression" => "Regression",
        "spike" => "Spike",
        _ => "Alert",
    };
    let subject = format!("[{}] {what}: {}", payload["project"]["name"].as_str().unwrap_or(""), issue["title"].as_str().unwrap_or(""));
    let url = payload["url"].as_str().unwrap_or("");
    let text = format!(
        "{what} in {}\n\n{}\n{}\n\nSeen {} times, last in release {}.\n\n{url}\n",
        payload["project"]["name"].as_str().unwrap_or(""),
        issue["title"].as_str().unwrap_or(""),
        issue["culprit"].as_str().unwrap_or(""),
        issue["times_seen"],
        issue["last_release"].as_str().unwrap_or("-"),
    );
    let html = format!("<pre style=\"font-family:ui-monospace,monospace\">{}</pre>", html_escape(&text));
    let resp = state
        .http
        .post(format!("{}/emails", state.config.resend_api_url))
        .bearer_auth(key)
        .json(&json!({
            "from": state.config.email_from,
            "to": to.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>(),
            "subject": subject,
            "text": text,
            "html": html,
        }))
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Resend returned {status}: {body}");
    }
    Ok(format!("Resend {status}"))
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Every 5 minutes, look for issues whose last-hour volume is a spike.
pub fn spawn_spike_detector(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            if let Err(e) = detect_spikes(&state).await {
                tracing::warn!("spike detection failed: {e:#}");
            }
        }
    });
}

async fn detect_spikes(state: &AppState) -> anyhow::Result<()> {
    let has_rules = sqlx::query("SELECT COUNT(*) AS n FROM alert_rules WHERE enabled = 1 AND trigger = 'spike'")
        .fetch_one(&state.db)
        .await?
        .i("n");
    if has_rules == 0 {
        return Ok(());
    }
    let rows = state
        .ch
        .query(
            "SELECT project_id, issue_id, countIf(timestamp > now64(3) - INTERVAL 1 HOUR) AS last_hour, \
             countIf(timestamp <= now64(3) - INTERVAL 1 HOUR) / 24 AS hourly_avg \
             FROM errors WHERE timestamp > now64(3) - INTERVAL 25 HOUR GROUP BY project_id, issue_id \
             HAVING last_hour >= 10 AND last_hour > 3 * hourly_avg",
            &[],
        )
        .await?;
    for r in rows {
        let project_id = r["project_id"].as_str().unwrap_or("");
        let issue_id = r["issue_id"].as_str().unwrap_or("");
        if let Some(project) = crate::projects::find(state, project_id).await? {
            dispatch(state, &project, issue_id, "spike", Some(json!({"last_hour": r["last_hour"], "hourly_avg_prev_24h": r["hourly_avg"]}))).await?;
        }
    }
    Ok(())
}

/// Send a test notification for a rule (used by the API "test" endpoint).
pub async fn test_rule(state: &AppState, rule_id: &str) -> anyhow::Result<String> {
    let row = sqlx::query(&format!("SELECT {RULE_COLS} FROM alert_rules WHERE id = $1"))
        .bind(rule_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("rule not found"))?;
    let rule = rule_from_row(&row);
    let payload = json!({
        "trigger": "test",
        "project": {"id": rule.project_id, "slug": "", "name": "Vaka test"},
        "issue": {"title": "Test alert from Vaka", "culprit": "", "times_seen": 0, "last_release": ""},
        "url": state.config.public_url,
        "sent_at": crate::util::iso_time(now_ms()),
    });
    match rule.channel.as_str() {
        "webhook" => send_webhook(state, &rule.target, &row.s("secret"), &payload).await,
        _ => send_email(state, &rule.target, "test", &payload).await,
    }
}

#[allow(dead_code)]
fn _assert_sha(_: Sha256) {}

#[cfg(test)]
mod tests {
    #[test]
    fn hmac_matches_known_vector() {
        // RFC 4231 test case 2
        let mac = super::sign("Jefe", b"what do ya want for nothing?");
        assert_eq!(mac, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }
}
