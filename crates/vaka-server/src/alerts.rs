//! Alerts: webhooks, Slack and email (Resend) on new issues, regressions and spikes, plus
//! daily digests.

use crate::db::RowExt;
use crate::state::{AppState, ProjectInfo};
use crate::util::{new_id, now_ms};
use serde_json::{json, Value};
use sha2::Sha256;
use std::time::Duration;

pub const TRIGGERS: &[&str] = &[
    "new_issue",
    "regression",
    "spike",
    "threshold",
    "issue_resolved",
    "issue_assigned",
    "issue_commented",
    "release_created",
    "digest",
];
pub const CHANNELS: &[&str] = &["webhook", "slack", "email", "github_dispatch", "github_issue"];
const SPIKE_COOLDOWN_MS: i64 = 6 * 3_600_000;
/// Retry schedule after a failed delivery (attempt 1 is the first try).
const BACKOFF_MS: &[i64] = &[60_000, 5 * 60_000, 30 * 60_000, 2 * 3_600_000, 6 * 3_600_000];

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
    /// {environment, min_level, release, owner, unhandled_only, tags: {k: v}, mention}
    pub filters: Value,
    /// 0 = unlimited.
    pub max_per_hour: i64,
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
        filters: serde_json::from_str(&r.s("filters")).unwrap_or_else(|_| json!({})),
        max_per_hour: r.i("max_per_hour"),
    }
}

pub const RULE_COLS: &str = "id, project_id, name, trigger, channel, target, secret, threshold, enabled, filters, max_per_hour";

pub const FILTER_KEYS: &[&str] = &["environment", "min_level", "release", "owner", "unhandled_only", "tags", "mention"];

/// Check rule filters; returns a description of the problem for invalid filters.
pub fn validate_filters(f: &Value) -> Result<(), String> {
    let Some(o) = f.as_object() else { return Err("filters must be an object".into()) };
    for (k, v) in o {
        if !FILTER_KEYS.contains(&k.as_str()) {
            return Err(format!("unknown filter `{k}`; use {}", FILTER_KEYS.join(", ")));
        }
        match k.as_str() {
            "unhandled_only" if !v.is_boolean() => return Err("unhandled_only must be true or false".into()),
            "tags" if !v.as_object().map(|t| t.values().all(|x| x.is_string())).unwrap_or(false) => return Err("tags must be an object of strings".into()),
            "min_level" if !matches!(v.as_str(), Some("debug" | "info" | "warning" | "error" | "fatal")) => return Err("min_level must be debug, info, warning, error or fatal".into()),
            "unhandled_only" | "tags" | "min_level" => {}
            _ if !v.is_string() => return Err(format!("{k} must be a string")),
            _ => {}
        }
    }
    Ok(())
}

fn level_rank(l: &str) -> i32 {
    match l {
        "debug" => 0,
        "info" => 1,
        "warning" | "warn" => 2,
        "error" => 3,
        "fatal" | "critical" => 4,
        _ => 3,
    }
}

/// Does an issue (and, when known, the event that triggered the alert) pass a rule's filters?
/// Event-level filters (environment, unhandled_only, tags) pass when there is no event.
pub fn filters_match(filters: &Value, issue: &Value, event: Option<&Value>) -> bool {
    let Some(f) = filters.as_object() else { return true };
    if let Some(min) = f.get("min_level").and_then(|v| v.as_str()) {
        let lvl = event.and_then(|e| e["level"].as_str()).or_else(|| issue["level"].as_str()).unwrap_or("error");
        if level_rank(lvl) < level_rank(min) {
            return false;
        }
    }
    if let Some(r) = f.get("release").and_then(|v| v.as_str()).filter(|r| !r.is_empty()) {
        let rel = event.and_then(|e| e["release"].as_str()).or_else(|| issue["last_release"].as_str()).unwrap_or("");
        if !crate::ownership::glob(r, rel) {
            return false;
        }
    }
    if let Some(o) = f.get("owner").and_then(|v| v.as_str()).filter(|o| !o.is_empty()) {
        let owners: Vec<&str> = issue["owners"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
        if !owners.iter().any(|x| x.eq_ignore_ascii_case(o)) && !issue["assignee"].as_str().map(|a| a.eq_ignore_ascii_case(o)).unwrap_or(false) {
            return false;
        }
    }
    if let Some(ev) = event {
        if let Some(env) = f.get("environment").and_then(|v| v.as_str()).filter(|e| !e.is_empty()) {
            if ev["environment"].as_str().map(|e| !crate::ownership::glob(env, e)).unwrap_or(false) {
                return false;
            }
        }
        if f.get("unhandled_only").and_then(|v| v.as_bool()) == Some(true) && ev["handled"].as_bool() == Some(true) {
            return false;
        }
        if let Some(tags) = f.get("tags").and_then(|v| v.as_object()) {
            for (k, v) in tags {
                let want = v.as_str().unwrap_or("");
                if !ev["tags"][k].as_str().map(|x| crate::ownership::glob(want, x)).unwrap_or(false) {
                    return false;
                }
            }
        }
    }
    true
}

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

/// Fire-and-forget notification for an issue event. `event` describes the error event or the
/// action that caused it ({environment, level, release, handled, tags} or {actor, ...}).
pub fn notify_issue(state: &AppState, project: &ProjectInfo, issue_id: &str, trigger: &str, event: Option<Value>) {
    let state = state.clone();
    let project = project.clone();
    let issue_id = issue_id.to_string();
    let trigger = trigger.to_string();
    tokio::spawn(async move {
        // Give the event a moment to reach ClickHouse so links and fix context show data.
        if trigger == "new_issue" || trigger == "regression" {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        if let Err(e) = dispatch(&state, &project, &issue_id, &trigger, None, event).await {
            tracing::warn!("alert dispatch failed: {e:#}");
        }
    });
}

/// Like `notify_issue` for a project id (looks the project up).
pub fn notify_issue_by_project(state: &AppState, project_id: &str, issue_id: &str, trigger: &str, event: Option<Value>) {
    let state2 = state.clone();
    let project_id = project_id.to_string();
    let issue_id = issue_id.to_string();
    let trigger = trigger.to_string();
    tokio::spawn(async move {
        if let Ok(Some(p)) = crate::projects::find(&state2, &project_id).await {
            notify_issue(&state2, &p, &issue_id, &trigger, event);
        }
    });
}

fn wants_fix_context(channel: &str) -> bool {
    matches!(channel, "webhook" | "github_dispatch" | "github_issue")
}

async fn system_fix_context(state: &AppState, issue_id: &str) -> Option<Value> {
    match crate::fixctx::fix_context(state, &crate::auth::Principal::system(), issue_id).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("fix context for alert failed: {}", e.1);
            None
        }
    }
}

async fn dispatch(state: &AppState, project: &ProjectInfo, issue_id: &str, trigger: &str, extra: Option<Value>, event: Option<Value>) -> anyhow::Result<()> {
    let rules = rules_for(state, &project.id, trigger).await?;
    if rules.is_empty() {
        return Ok(());
    }
    let issue = crate::query::get_issue_row(state, issue_id).await?;
    let Some(issue) = issue else { return Ok(()) };
    let mut fix: Option<Option<Value>> = None;
    for (rule, secret) in rules {
        if !filters_match(&rule.filters, &issue, event.as_ref()) {
            continue;
        }
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
        let mut payload = json!({
            "trigger": trigger,
            "project": {"id": project.id, "slug": project.slug, "name": project.name},
            "issue": issue,
            "url": format!("{}/ui/projects/{}/issues/{}", state.config.public_url, project.slug, issue_id),
            "api_url": format!("{}/api/v1/issues/{}", state.config.public_url, issue_id),
            "details": extra,
            "event": event,
            "sent_at": crate::util::iso_time(now_ms()),
        });
        if wants_fix_context(&rule.channel) {
            if fix.is_none() {
                fix = Some(system_fix_context(state, issue_id).await);
            }
            payload["fix_context"] = fix.clone().flatten().unwrap_or(Value::Null);
        }
        deliver_new(state, &rule, &secret, trigger, issue_id, payload).await?;
    }
    Ok(())
}

/// Notify `release_created` rules about a release seen for the first time.
pub fn notify_release(state: &AppState, project: &ProjectInfo, release: &str) {
    let state = state.clone();
    let project = project.clone();
    let release = release.to_string();
    tokio::spawn(async move {
        let r: anyhow::Result<()> = async {
            let rules = rules_for(&state, &project.id, "release_created").await?;
            for (rule, secret) in rules {
                if let Some(g) = rule.filters["release"].as_str().filter(|g| !g.is_empty()) {
                    if !crate::ownership::glob(g, &release) {
                        continue;
                    }
                }
                let payload = json!({
                    "trigger": "release_created",
                    "project": {"id": project.id, "slug": project.slug, "name": project.name},
                    "release": {"version": release},
                    "api_url": format!("{}/api/v1/projects/{}/releases/health?release={}", state.config.public_url, project.slug, url_encode(&release)),
                    "sent_at": crate::util::iso_time(now_ms()),
                });
                deliver_new(&state, &rule, &secret, "release_created", "", payload).await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = r {
            tracing::warn!("release notification failed: {e:#}");
        }
    });
}

fn url_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
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

/// Record a delivery and attempt it now; failures are retried by the delivery worker.
pub async fn deliver_new(state: &AppState, rule: &Rule, secret: &str, trigger: &str, issue_id: &str, payload: Value) -> anyhow::Result<String> {
    let id = new_id();
    let now = now_ms();
    if rule.max_per_hour > 0 {
        let sent = sqlx::query("SELECT COUNT(*) AS n FROM alert_deliveries WHERE rule_id = $1 AND created_at > $2 AND status != 'rate_limited'")
            .bind(&rule.id)
            .bind(now - 3_600_000)
            .fetch_one(&state.db)
            .await?
            .i("n");
        if sent >= rule.max_per_hour {
            insert_delivery(state, &id, rule, issue_id, trigger, "rate_limited", &format!("max_per_hour {} reached", rule.max_per_hour), &payload, 0, None).await?;
            return Ok("rate limited".into());
        }
    }
    let result = send(state, rule, secret, trigger, issue_id, &payload, &id).await;
    let (status, detail, next) = match &result {
        Ok(d) => ("ok", d.clone(), None),
        Err(e) if retryable(&rule.channel, e) => ("retrying", e.to_string(), Some(now + BACKOFF_MS[0])),
        Err(e) => ("failed", e.to_string(), None),
    };
    insert_delivery(state, &id, rule, issue_id, trigger, status, &detail, &payload, 1, next).await?;
    result
}

fn retryable(channel: &str, e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    !(channel == "email" && msg.contains("VAKA_RESEND_API_KEY")) && !msg.contains("needs a GitHub token")
}

#[allow(clippy::too_many_arguments)]
async fn insert_delivery(state: &AppState, id: &str, rule: &Rule, issue_id: &str, trigger: &str, status: &str, detail: &str, payload: &Value, attempts: i64, next: Option<i64>) -> anyhow::Result<()> {
    let mut body = serde_json::to_string(payload)?;
    if body.len() > 256 * 1024 {
        let mut slim = payload.clone();
        slim["fix_context"] = Value::Null;
        body = serde_json::to_string(&slim)?;
    }
    sqlx::query(
        "INSERT INTO alert_deliveries (id, rule_id, issue_id, trigger, status, detail, created_at, payload, attempts, next_attempt_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(&rule.id)
    .bind(issue_id)
    .bind(trigger)
    .bind(status)
    .bind(crate::util::truncate(detail, 1000))
    .bind(now_ms())
    .bind(body)
    .bind(attempts)
    .bind(next)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Send one payload through a rule's channel.
async fn send(state: &AppState, rule: &Rule, secret: &str, trigger: &str, issue_id: &str, payload: &Value, delivery_id: &str) -> anyhow::Result<String> {
    match (rule.channel.as_str(), trigger) {
        ("webhook", _) => send_webhook(state, &rule.target, secret, trigger, delivery_id, payload).await,
        ("slack", "digest") => send_slack(state, &rule.target, &slack_digest_message(payload, &state.config.public_url)).await,
        ("slack", "release_created") => send_slack(state, &rule.target, &slack_release_message(payload)).await,
        ("slack", _) => send_slack(state, &rule.target, &slack_issue_message(trigger, payload)).await,
        ("email", "digest") => send_digest_email(state, &rule.target, payload).await,
        ("email", _) => send_email(state, &rule.target, trigger, payload).await,
        ("github_dispatch", _) => crate::github::dispatch(state, &rule.target, secret, trigger, payload).await,
        ("github_issue", _) => crate::github::issue(state, &rule.target, secret, trigger, issue_id, payload, &rule.filters).await,
        (other, _) => Err(anyhow::anyhow!("unknown channel {other}")),
    }
}

/// Every 30 seconds retry failed deliveries whose backoff has passed; every minute check
/// `threshold` rules.
pub fn spawn_delivery_worker(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            n += 1;
            if let Err(e) = retry_due(&state).await {
                tracing::warn!("delivery retries failed: {e:#}");
            }
            if n % 2 == 0 {
                if let Err(e) = check_thresholds(&state).await {
                    tracing::warn!("threshold alerts failed: {e:#}");
                }
            }
        }
    });
}

async fn retry_due(state: &AppState) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT id, rule_id, issue_id, trigger, payload, attempts FROM alert_deliveries WHERE status = 'retrying' AND next_attempt_at <= $1 ORDER BY next_attempt_at LIMIT 50")
        .bind(now_ms())
        .fetch_all(&state.db)
        .await?;
    for r in rows {
        let id = r.s("id");
        let Some(rule_row) = sqlx::query(&format!("SELECT {RULE_COLS} FROM alert_rules WHERE id = $1")).bind(r.s("rule_id")).fetch_optional(&state.db).await? else {
            sqlx::query("UPDATE alert_deliveries SET status = 'failed', detail = 'rule deleted', next_attempt_at = NULL WHERE id = $1").bind(&id).execute(&state.db).await?;
            continue;
        };
        let rule = rule_from_row(&rule_row);
        let payload: Value = serde_json::from_str(&r.s("payload")).unwrap_or(Value::Null);
        let attempts = r.i("attempts") + 1;
        let result = send(state, &rule, &rule_row.s("secret"), &r.s("trigger"), &r.s("issue_id"), &payload, &id).await;
        let (status, detail, next) = match &result {
            Ok(d) => ("ok", d.clone(), None),
            Err(e) if (attempts as usize) <= BACKOFF_MS.len() && retryable(&rule.channel, e) => ("retrying", e.to_string(), Some(now_ms() + BACKOFF_MS[attempts as usize - 1])),
            Err(e) => ("failed", e.to_string(), None),
        };
        sqlx::query("UPDATE alert_deliveries SET status = $1, detail = $2, attempts = $3, next_attempt_at = $4 WHERE id = $5")
            .bind(status)
            .bind(crate::util::truncate(&format!("attempt {attempts}: {detail}"), 1000))
            .bind(attempts)
            .bind(next)
            .bind(&id)
            .execute(&state.db)
            .await?;
    }
    Ok(())
}

/// Send a stored delivery's payload again now (new delivery row).
pub async fn redeliver(state: &AppState, rule_id: &str, delivery_id: &str) -> anyhow::Result<Value> {
    let row = sqlx::query("SELECT issue_id, trigger, payload FROM alert_deliveries WHERE id = $1 AND rule_id = $2")
        .bind(delivery_id)
        .bind(rule_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("delivery not found"))?;
    let rule_row = sqlx::query(&format!("SELECT {RULE_COLS} FROM alert_rules WHERE id = $1")).bind(rule_id).fetch_one(&state.db).await?;
    let rule = rule_from_row(&rule_row);
    let payload: Value = serde_json::from_str(&row.s("payload")).map_err(|_| anyhow::anyhow!("this delivery has no stored payload"))?;
    let r = deliver_new(state, &rule, &rule_row.s("secret"), &row.s("trigger"), &row.s("issue_id"), payload).await;
    Ok(match r {
        Ok(d) => json!({"ok": true, "detail": d}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    })
}

/// `threshold` rules fire once per issue when it reaches `threshold` events.
async fn check_thresholds(state: &AppState) -> anyhow::Result<()> {
    let rules = sqlx::query(&format!("SELECT {RULE_COLS} FROM alert_rules WHERE enabled = 1 AND trigger = 'threshold'")).fetch_all(&state.db).await?;
    for row in rules {
        let rule = rule_from_row(&row);
        let issues = sqlx::query(
            "SELECT i.id, i.project_id FROM issues i WHERE i.times_seen >= $1 AND i.last_seen > $2 AND i.status = 'open' \
             AND ($3 = '' OR i.project_id = $3) \
             AND NOT EXISTS (SELECT 1 FROM alert_deliveries d WHERE d.rule_id = $4 AND d.issue_id = i.id) LIMIT 20",
        )
        .bind(rule.threshold.max(1))
        .bind(now_ms() - 86_400_000)
        .bind(rule.project_id.clone().unwrap_or_default())
        .bind(&rule.id)
        .fetch_all(&state.db)
        .await?;
        for i in issues {
            let Some(project) = crate::projects::find(state, &i.s("project_id")).await? else { continue };
            let Some(issue) = crate::query::get_issue_row(state, &i.s("id")).await? else { continue };
            if !filters_match(&rule.filters, &issue, None) {
                // Record a skip so the issue is not checked again.
                insert_delivery(state, &new_id(), &rule, &i.s("id"), "threshold", "skipped", "filters did not match", &Value::Null, 0, None).await?;
                continue;
            }
            let mut payload = json!({
                "trigger": "threshold",
                "project": {"id": project.id, "slug": project.slug, "name": project.name},
                "issue": issue,
                "url": format!("{}/ui/projects/{}/issues/{}", state.config.public_url, project.slug, i.s("id")),
                "api_url": format!("{}/api/v1/issues/{}", state.config.public_url, i.s("id")),
                "details": {"threshold": rule.threshold},
                "sent_at": crate::util::iso_time(now_ms()),
            });
            if wants_fix_context(&rule.channel) {
                payload["fix_context"] = system_fix_context(state, &i.s("id")).await.unwrap_or(Value::Null);
            }
            let _ = deliver_new(state, &rule, &row.s("secret"), "threshold", &i.s("id"), payload).await;
        }
    }
    Ok(())
}

pub fn sign(secret: &str, body: &[u8]) -> String {
    use hmac_lite::hmac_sha256;
    hex::encode(hmac_sha256(secret.as_bytes(), body))
}

/// Tiny HMAC-SHA256 so we don't need another dependency.
pub(crate) mod hmac_lite {
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

async fn send_webhook(state: &AppState, url: &str, secret: &str, trigger: &str, delivery_id: &str, payload: &Value) -> anyhow::Result<String> {
    let body = serde_json::to_vec(payload)?;
    let ts = (now_ms() / 1000).to_string();
    let mut req = state
        .http
        .post(url)
        .header("content-type", "application/json")
        .header("user-agent", "Vaka-Webhook/1")
        .header("x-vaka-event", trigger)
        .header("x-vaka-delivery", delivery_id)
        .header("x-vaka-timestamp", &ts)
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
        "threshold" => "Threshold reached",
        "issue_resolved" => "Resolved",
        "issue_assigned" => "Assigned",
        "issue_commented" => "New comment",
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
    let text = match owners_line(issue) {
        Some(o) => format!("{text}Owners: {o}\n"),
        None => text,
    };
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

fn owners_line(issue: &serde_json::Value) -> Option<String> {
    let owners: Vec<&str> = issue["owners"].as_array()?.iter().filter_map(|o| o.as_str()).collect();
    (!owners.is_empty()).then(|| owners.join(", "))
}

fn slack_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A Slack incoming-webhook message for an issue alert.
pub fn slack_issue_message(trigger: &str, payload: &serde_json::Value) -> serde_json::Value {
    let issue = &payload["issue"];
    let what = match trigger {
        "new_issue" => ":new: New issue",
        "regression" => ":leftwards_arrow_with_hook: Regression",
        "spike" => ":chart_with_upwards_trend: Spike",
        "threshold" => ":rotating_light: Threshold reached",
        "issue_resolved" => ":white_check_mark: Resolved",
        "issue_assigned" => ":bust_in_silhouette: Assigned",
        "issue_commented" => ":speech_balloon: New comment",
        "test" => ":white_check_mark: Test alert",
        _ => "Alert",
    };
    let project = payload["project"]["name"].as_str().unwrap_or("");
    let title = slack_escape(issue["title"].as_str().unwrap_or(""));
    let url = payload["url"].as_str().unwrap_or("");
    let short = issue["short_id"].as_str().unwrap_or("");
    let mut lines = vec![format!("*{what}* in {}", slack_escape(project)), format!("*<{url}|{} {title}>*", slack_escape(short))];
    if let Some(c) = issue["culprit"].as_str().filter(|c| !c.is_empty()) {
        lines.push(format!("`{}`", c.replace('`', "'")));
    }
    let mut facts = vec![format!("seen {} times", issue["times_seen"]), format!("release {}", issue["last_release"].as_str().filter(|r| !r.is_empty()).unwrap_or("-"))];
    if let Some(h) = payload["details"]["last_hour"].as_i64() {
        facts.push(format!("{h} in the last hour"));
    }
    if let Some(o) = owners_line(issue) {
        facts.push(format!("owners {}", slack_escape(&o)));
    }
    lines.push(facts.join(" · "));
    json!({
        "text": format!("{what}: {title}"),
        "blocks": [{"type": "section", "text": {"type": "mrkdwn", "text": lines.join("\n")}}],
        "unfurl_links": false,
    })
}

fn slack_release_message(payload: &Value) -> Value {
    let project = slack_escape(payload["project"]["name"].as_str().unwrap_or(""));
    let release = slack_escape(payload["release"]["version"].as_str().unwrap_or(""));
    json!({"text": format!("New release {release} in {project}"), "blocks": [{"type": "section", "text": {"type": "mrkdwn", "text": format!(":rocket: *New release* `{release}` in {project}")}}], "unfurl_links": false})
}

async fn send_slack(state: &AppState, url: &str, message: &serde_json::Value) -> anyhow::Result<String> {
    let resp = state.http.post(url).json(message).timeout(Duration::from_secs(10)).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Slack returned {status}: {}", crate::util::truncate(&body, 200));
    }
    Ok(format!("HTTP {status}"))
}

// ---------------------------------------------------------------------------
// Daily digests
// ---------------------------------------------------------------------------

/// Every 10 minutes, send digests whose daily hour (UTC, the rule's `threshold`) has passed today
/// and that have not been sent since.
pub fn spawn_digest_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(600));
        loop {
            tick.tick().await;
            if let Err(e) = run_digests(&state, now_ms()).await {
                tracing::warn!("digests failed: {e:#}");
            }
        }
    });
}

/// When today's digest for `hour` is due (ms since epoch).
pub fn digest_due_at(now: i64, hour: i64) -> i64 {
    let day = now - now.rem_euclid(86_400_000);
    day + hour.clamp(0, 23) * 3_600_000
}

async fn run_digests(state: &AppState, now: i64) -> anyhow::Result<()> {
    let rows = sqlx::query(&format!("SELECT {RULE_COLS} FROM alert_rules WHERE enabled = 1 AND trigger = 'digest'")).fetch_all(&state.db).await?;
    for row in rows {
        let rule = rule_from_row(&row);
        let due = digest_due_at(now, rule.threshold);
        if now < due {
            continue;
        }
        let last = sqlx::query("SELECT MAX(created_at) AS t FROM alert_deliveries WHERE rule_id = $1 AND status IN ('ok', 'retrying')")
            .bind(&rule.id)
            .fetch_one(&state.db)
            .await?
            .oi("t")
            .unwrap_or(0);
        if last >= due {
            continue;
        }
        send_digest(state, &rule, &row.s("secret"), now).await?;
    }
    Ok(())
}

/// Build and send one digest now. Returns the delivery detail.
pub async fn send_digest(state: &AppState, rule: &Rule, secret: &str, now: i64) -> anyhow::Result<String> {
    let projects = match &rule.project_id {
        Some(pid) => crate::projects::find(state, pid).await?.into_iter().collect(),
        None => crate::projects::list(state).await?,
    };
    let mut items = Vec::new();
    for p in &projects {
        let d = project_digest(state, p, now).await?;
        let quiet = d["errors"].as_i64() == Some(0) && d["visitors"].as_i64() == Some(0) && d["sessions"].as_i64() == Some(0);
        if !quiet {
            items.push(d);
        }
    }
    let payload = json!({
        "trigger": "digest",
        "period": {"since": crate::util::iso_time(now - 86_400_000), "until": crate::util::iso_time(now)},
        "projects": items,
        "sent_at": crate::util::iso_time(now),
    });
    deliver_new(state, rule, secret, "digest", "", payload).await
}

fn n(v: &serde_json::Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

async fn project_digest(state: &AppState, p: &ProjectInfo, now: i64) -> anyhow::Result<serde_json::Value> {
    let ps = [("p", p.id.clone()), ("d1", (now - 86_400_000).to_string()), ("d2", (now - 2 * 86_400_000).to_string())];
    let t1 = "fromUnixTimestamp64Milli({d1:Int64})";
    let t2 = "fromUnixTimestamp64Milli({d2:Int64})";
    let errs = state
        .ch
        .query(
            &format!("SELECT countIf(timestamp >= {t1}) AS errors, countIf(timestamp < {t1}) AS errors_prev FROM errors WHERE project_id = {{p:String}} AND timestamp >= {t2}"),
            &ps,
        )
        .await?;
    let top = state
        .ch
        .query(
            &format!("SELECT issue_id, count() AS events, uniq(if(user_id != '', user_id, anon_id)) AS users FROM errors WHERE project_id = {{p:String}} AND timestamp >= {t1} GROUP BY issue_id ORDER BY events DESC LIMIT 5"),
            &ps,
        )
        .await?;
    let traffic = state
        .ch
        .query(
            &format!(
                "SELECT uniqIf(visitor_id, timestamp >= {t1}) AS visitors, uniqIf(visitor_id, timestamp < {t1}) AS visitors_prev, \
                 countIf(type = 'pageview' AND timestamp >= {t1}) AS pageviews, countIf(type = 'pageview' AND timestamp < {t1}) AS pageviews_prev \
                 FROM events WHERE project_id = {{p:String}} AND timestamp >= {t2}"
            ),
            &ps,
        )
        .await?;
    let sessions = state
        .ch
        .query(
            &format!(
                "SELECT count() AS sessions, countIf(s_status = 'crashed') AS crashed FROM \
                 (SELECT session_id, argMax(status, timestamp) AS s_status FROM sessions WHERE project_id = {{p:String}} AND started >= {t1} GROUP BY session_id)"
            ),
            &ps,
        )
        .await?;
    let new_rows = sqlx::query("SELECT id FROM issues WHERE project_id = $1 AND first_seen >= $2 ORDER BY times_seen DESC LIMIT 5")
        .bind(&p.id)
        .bind(now - 86_400_000)
        .fetch_all(&state.db)
        .await?;
    let new_count = sqlx::query("SELECT COUNT(*) AS n FROM issues WHERE project_id = $1 AND first_seen >= $2").bind(&p.id).bind(now - 86_400_000).fetch_one(&state.db).await?.i("n");
    let slim = |i: &serde_json::Value| json!({"short_id": i["short_id"], "title": i["title"], "culprit": i["culprit"], "status": i["status"], "url": i["url"], "owners": i["owners"]});
    let mut new_issues = Vec::new();
    for r in &new_rows {
        if let Some(i) = crate::query::get_issue_row(state, &r.s("id")).await? {
            new_issues.push(slim(&i));
        }
    }
    let mut top_issues = Vec::new();
    for r in &top {
        if let Some(i) = crate::query::get_issue_row(state, r["issue_id"].as_str().unwrap_or("")).await? {
            let mut v = slim(&i);
            v["events"] = json!(n(&r["events"]));
            v["users"] = json!(n(&r["users"]));
            top_issues.push(v);
        }
    }
    let e = errs.first().cloned().unwrap_or(json!({}));
    let t = traffic.first().cloned().unwrap_or(json!({}));
    let s = sessions.first().cloned().unwrap_or(json!({}));
    let (sess, crashed) = (n(&s["sessions"]), n(&s["crashed"]));
    Ok(json!({
        "project": {"id": p.id, "slug": p.slug, "name": p.name},
        "errors": n(&e["errors"]), "errors_prev": n(&e["errors_prev"]),
        "new_issue_count": new_count, "new_issues": new_issues, "top_issues": top_issues,
        "sessions": sess,
        "crash_free_sessions": if sess > 0 { Some(((1.0 - crashed as f64 / sess as f64) * 10_000.0).round() / 10_000.0) } else { None },
        "visitors": n(&t["visitors"]), "visitors_prev": n(&t["visitors_prev"]),
        "pageviews": n(&t["pageviews"]), "pageviews_prev": n(&t["pageviews_prev"]),
    }))
}

fn delta(now: i64, prev: i64) -> String {
    if prev == 0 {
        return String::new();
    }
    let pct = ((now - prev) as f64 / prev as f64 * 100.0).round() as i64;
    format!(" ({}{pct}%)", if pct >= 0 { "+" } else { "" })
}

/// Plain-text digest, shared by email and (with mrkdwn tweaks) Slack.
pub fn digest_lines(payload: &serde_json::Value, slack: bool) -> Vec<String> {
    let mut out = Vec::new();
    let projects = payload["projects"].as_array().cloned().unwrap_or_default();
    if projects.is_empty() {
        out.push("No activity in the last 24 hours.".to_string());
    }
    let esc = |s: &str| if slack { slack_escape(s) } else { s.to_string() };
    for p in projects {
        let name = esc(p["project"]["name"].as_str().unwrap_or(""));
        out.push(if slack { format!("*{name}*") } else { format!("== {name} ==") });
        let mut facts = vec![format!("{} errors{}", n(&p["errors"]), delta(n(&p["errors"]), n(&p["errors_prev"]))), format!("{} new issues", n(&p["new_issue_count"]))];
        if let Some(cf) = p["crash_free_sessions"].as_f64() {
            facts.push(format!("{:.2}% crash-free of {} sessions", cf * 100.0, n(&p["sessions"])));
        }
        if n(&p["visitors"]) > 0 || n(&p["visitors_prev"]) > 0 {
            facts.push(format!("{} visitors{}, {} pageviews{}", n(&p["visitors"]), delta(n(&p["visitors"]), n(&p["visitors_prev"])), n(&p["pageviews"]), delta(n(&p["pageviews"]), n(&p["pageviews_prev"]))));
        }
        out.push(facts.join(" · "));
        let issue_line = |i: &serde_json::Value, extra: String| {
            let short = i["short_id"].as_str().unwrap_or("");
            let title = esc(i["title"].as_str().unwrap_or(""));
            if slack {
                format!("• <{}|{short}> {title}{extra}", i["url"].as_str().unwrap_or(""))
            } else {
                format!("  {short} {title}{extra}")
            }
        };
        let new: Vec<_> = p["new_issues"].as_array().cloned().unwrap_or_default();
        if !new.is_empty() {
            out.push("New:".into());
            out.extend(new.iter().map(|i| issue_line(i, String::new())));
        }
        let top: Vec<_> = p["top_issues"].as_array().cloned().unwrap_or_default();
        if !top.is_empty() {
            out.push("Top issues:".into());
            out.extend(top.iter().map(|i| issue_line(i, format!(" ({} events, {} users)", n(&i["events"]), n(&i["users"])))));
        }
        out.push(String::new());
    }
    out
}

fn slack_digest_message(payload: &serde_json::Value, _public_url: &str) -> serde_json::Value {
    let text = digest_lines(payload, true).join("\n");
    json!({
        "text": "Vaka daily digest",
        "blocks": [
            {"type": "header", "text": {"type": "plain_text", "text": "Vaka daily digest"}},
            {"type": "section", "text": {"type": "mrkdwn", "text": crate::util::truncate(&text, 2900)}},
        ],
        "unfurl_links": false,
    })
}

async fn send_digest_email(state: &AppState, to: &str, payload: &serde_json::Value) -> anyhow::Result<String> {
    let Some(key) = &state.config.resend_api_key else {
        anyhow::bail!("email digests need VAKA_RESEND_API_KEY");
    };
    let text = format!("Vaka daily digest\n\n{}", digest_lines(payload, false).join("\n"));
    let html = format!("<pre style=\"font-family:ui-monospace,monospace\">{}</pre>", html_escape(&text));
    let resp = state
        .http
        .post(format!("{}/emails", state.config.resend_api_url))
        .bearer_auth(key)
        .json(&json!({
            "from": state.config.email_from,
            "to": to.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>(),
            "subject": "Vaka daily digest",
            "text": text,
            "html": html,
        }))
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("Resend returned {status}: {}", resp.text().await.unwrap_or_default());
    }
    Ok(format!("Resend {status}"))
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
            dispatch(state, &project, issue_id, "spike", Some(json!({"last_hour": r["last_hour"], "hourly_avg_prev_24h": r["hourly_avg"]})), None).await?;
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
    if rule.trigger == "digest" {
        return send_digest(state, &rule, &row.s("secret"), now_ms()).await;
    }
    let payload = json!({
        "trigger": "test",
        "project": {"id": rule.project_id, "slug": "", "name": "Vaka test"},
        "issue": {"title": "Test alert from Vaka", "culprit": "", "times_seen": 0, "last_release": ""},
        "url": state.config.public_url,
        "sent_at": crate::util::iso_time(now_ms()),
    });
    send(state, &rule, &row.s("secret"), "test", "", &payload, &format!("test-{}", new_id())).await
}

#[allow(dead_code)]
fn _assert_sha(_: Sha256) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn digest_schedule_and_text() {
        let day = 1_763_000_000_000 - 1_763_000_000_000 % 86_400_000;
        assert_eq!(digest_due_at(day + 5 * 3_600_000, 8), day + 8 * 3_600_000);
        assert_eq!(digest_due_at(day + 9 * 3_600_000, 8), day + 8 * 3_600_000);
        let payload = json!({"projects": [{"project": {"name": "Web <prod>"}, "errors": 12, "errors_prev": 10, "new_issue_count": 1,
            "new_issues": [{"short_id": "WEB-3", "title": "TypeError", "url": "https://v/x"}], "top_issues": [], "sessions": 0,
            "crash_free_sessions": null, "visitors": 5, "visitors_prev": 0, "pageviews": 9, "pageviews_prev": 0}]});
        let slack = digest_lines(&payload, true).join("\n");
        assert!(slack.contains("*Web &lt;prod&gt;*"), "{slack}");
        assert!(slack.contains("12 errors (+20%)"), "{slack}");
        assert!(slack.contains("<https://v/x|WEB-3> TypeError"), "{slack}");
        let msg = slack_issue_message("new_issue", &json!({"project": {"name": "Web"}, "url": "https://v/i", "issue": {"short_id": "WEB-3", "title": "a < b", "times_seen": 2, "owners": ["a@x.se"]}}));
        assert!(msg["blocks"][0]["text"]["text"].as_str().unwrap().contains("a &lt; b"));
        assert!(msg["blocks"][0]["text"]["text"].as_str().unwrap().contains("owners a@x.se"));
    }

    #[test]
    fn hmac_matches_known_vector() {
        // RFC 4231 test case 2
        let mac = super::sign("Jefe", b"what do ya want for nothing?");
        assert_eq!(mac, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }
}
