//! Alerts: webhooks, Slack and email (Resend) on new issues, regressions and spikes, plus
//! daily digests.

use crate::db::RowExt;
use crate::state::{AppState, ProjectInfo};
use crate::util::{new_id, now_ms};
use serde_json::json;
use sha2::Sha256;
use std::time::Duration;

pub const TRIGGERS: &[&str] = &["new_issue", "regression", "spike", "digest"];
pub const CHANNELS: &[&str] = &["webhook", "slack", "email"];
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
            "slack" => send_slack(state, &rule.target, &slack_issue_message(trigger, &payload)).await,
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
        let last = sqlx::query("SELECT MAX(created_at) AS t FROM alert_deliveries WHERE rule_id = $1 AND status = 'ok'")
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
    let result = match rule.channel.as_str() {
        "webhook" => send_webhook(state, &rule.target, secret, &payload).await,
        "slack" => send_slack(state, &rule.target, &slack_digest_message(&payload, &state.config.public_url)).await,
        "email" => send_digest_email(state, &rule.target, &payload).await,
        other => Err(anyhow::anyhow!("unknown channel {other}")),
    };
    let (status, detail) = match &result {
        Ok(d) => ("ok", d.clone()),
        Err(e) => ("failed", e.to_string()),
    };
    sqlx::query("INSERT INTO alert_deliveries (id, rule_id, issue_id, trigger, status, detail, created_at) VALUES ($1, $2, '', 'digest', $3, $4, $5)")
        .bind(new_id())
        .bind(&rule.id)
        .bind(status)
        .bind(crate::util::truncate(&detail, 1000))
        .bind(now_ms())
        .execute(&state.db)
        .await?;
    result
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
    match rule.channel.as_str() {
        "webhook" => send_webhook(state, &rule.target, &row.s("secret"), &payload).await,
        "slack" => send_slack(state, &rule.target, &slack_issue_message("test", &payload)).await,
        _ => send_email(state, &rule.target, "test", &payload).await,
    }
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
