//! Session timelines: what happened in a session, in order — pageviews, screen views, custom
//! events, errors and the breadcrumbs the SDK recorded — plus "crash precursors", the steps that
//! most often come right before an issue.

use crate::auth::{ApiError, Principal};
use crate::query::{project_for, QResult};
use crate::state::AppState;
use crate::util::{iso_time, parse_timestamp};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

/// Well-known breadcrumb categories; others are kept as sent.
pub fn normalize_category(c: &str) -> String {
    let c = c.trim().to_lowercase();
    match c.as_str() {
        "fetch" | "xhr" | "http" | "network" | "request" | "okhttp" | "urlsession" => "http".into(),
        "click" | "ui.click" | "tap" | "ui.tap" | "touch" => "ui.click".into(),
        "navigation" | "nav" | "route" | "screen" => "navigation".into(),
        "console" | "log" | "logcat" => "console".into(),
        "lifecycle" | "app.lifecycle" | "app" | "device" => "lifecycle".into(),
        "" => "default".into(),
        _ => c,
    }
}

/// A breadcrumb in the common shape: {timestamp, category, type, level, message, data}.
/// Accepts strings, missing fields and unix or RFC 3339 timestamps.
pub fn normalize_breadcrumb(b: &Value, fallback_ms: i64) -> Value {
    if let Some(s) = b.as_str() {
        return json!({"timestamp": iso_time(fallback_ms), "ts_ms": fallback_ms, "category": "default", "type": "default", "level": "info", "message": s, "data": null});
    }
    let ts = b.get("timestamp").and_then(parse_timestamp).unwrap_or(fallback_ms);
    let category = normalize_category(b["category"].as_str().unwrap_or(""));
    let ty = b["type"].as_str().map(String::from).unwrap_or_else(|| match category.as_str() {
        "http" => "http".into(),
        "navigation" => "navigation".into(),
        "ui.click" => "user".into(),
        "console" => "debug".into(),
        _ => "default".into(),
    });
    let level = match b["level"].as_str().unwrap_or("info") {
        "warn" => "warning",
        l => l,
    };
    let mut message = b["message"].as_str().unwrap_or("").to_string();
    let data = b.get("data").cloned().filter(|d| !d.is_null());
    if message.is_empty() {
        if let Some(d) = &data {
            if category == "http" {
                message = format!("{} {} {}", d["method"].as_str().unwrap_or("GET"), d["url"].as_str().unwrap_or(""), d["status_code"].as_u64().or(d["status"].as_u64()).map(|s| s.to_string()).unwrap_or_default()).trim().to_string();
            } else if category == "navigation" {
                message = format!("{} -> {}", d["from"].as_str().unwrap_or(""), d["to"].as_str().unwrap_or(""));
            }
        }
    }
    json!({"timestamp": iso_time(ts), "ts_ms": ts, "category": category, "type": ty, "level": level, "message": crate::util::truncate(&message, 1000), "data": data})
}

fn entry(ts: i64, kind: &str, title: String, extra: Value) -> Value {
    let mut v = json!({"timestamp": iso_time(ts), "ts_ms": ts, "kind": kind, "title": title});
    if let Some(o) = extra.as_object() {
        for (k, x) in o {
            if !x.is_null() {
                v[k] = x.clone();
            }
        }
    }
    v
}

fn n(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

/// Build a merged, sorted, de-duplicated timeline from rows of events, errors and sessions.
pub fn merge(events: &[Value], errors: &[Value], sessions: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for e in events {
        let ts = n(&e["ts"]);
        let kind = match e["type"].as_str() {
            Some("pageview") => "pageview",
            Some("screen_view") => "screen_view",
            _ => "event",
        };
        let props: Value = serde_json::from_str(e["props_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
        let props = if props.as_object().map(|o| o.is_empty()).unwrap_or(true) { Value::Null } else { props };
        out.push(entry(ts, kind, e["label"].as_str().unwrap_or("").to_string(), json!({"url": e["url"].as_str().filter(|u| !u.is_empty()), "props": props})));
    }
    let mut seen: HashSet<(i64, String, String)> = HashSet::new();
    for r in errors {
        let ts = n(&r["ts"]);
        let crumbs: Value = serde_json::from_str(r["breadcrumbs"].as_str().unwrap_or("[]")).unwrap_or(json!([]));
        for b in crumbs.as_array().into_iter().flatten() {
            let nb = normalize_breadcrumb(b, ts);
            let key = (n(&nb["ts_ms"]), nb["category"].as_str().unwrap_or("").to_string(), nb["message"].as_str().unwrap_or("").to_string());
            // The SDK's own "error" crumb duplicates the error entry; navigation crumbs duplicate pageviews.
            if nb["category"] == "error" || !seen.insert(key) {
                continue;
            }
            let title = format!("{}: {}", nb["category"].as_str().unwrap_or(""), nb["message"].as_str().unwrap_or(""));
            out.push(entry(n(&nb["ts_ms"]), "breadcrumb", title, json!({"category": nb["category"], "level": nb["level"], "data": nb["data"]})));
        }
        let what = if r["exception_type"].as_str().unwrap_or("").is_empty() {
            r["message"].as_str().unwrap_or("").to_string()
        } else {
            format!("{}: {}", r["exception_type"].as_str().unwrap_or(""), r["exception_value"].as_str().unwrap_or(""))
        };
        let kind = if r["fatal"].as_u64() == Some(1) || n(&r["fatal"]) == 1 { "crash" } else { "error" };
        out.push(entry(ts, kind, crate::util::truncate(&what, 300), json!({"issue_id": r["issue_id"], "event_id": r["event_id"], "culprit": r["culprit"], "release": r["release"]})));
    }
    for s in sessions {
        out.push(entry(n(&s["started_ms"]), "session_start", format!("session started ({})", s["release"].as_str().unwrap_or("")), json!({})));
        let status = s["status"].as_str().unwrap_or("");
        if status != "ok" {
            out.push(entry(n(&s["ts"]), "session_end", format!("session {status}"), json!({"duration_ms": s["duration_ms"]})));
        }
    }
    out.sort_by_key(|e| n(&e["ts_ms"]));
    out
}

const EVENT_SQL: &str = "SELECT toUnixTimestamp64Milli(timestamp) AS ts, type, LABEL AS label, url, toJSONString(props) AS props_json FROM events \
    WHERE project_id = {p:String} AND session_id = {s:String} AND timestamp >= fromUnixTimestamp64Milli({from:Int64}) AND timestamp <= fromUnixTimestamp64Milli({to:Int64}) \
    ORDER BY timestamp LIMIT 2000";
const ERROR_SQL: &str = "SELECT toUnixTimestamp64Milli(timestamp) AS ts, toString(event_id) AS event_id, issue_id, exception_type, exception_value, message, culprit, \
    release, fatal, breadcrumbs FROM errors WHERE project_id = {p:String} AND session_id = {s:String} \
    AND timestamp >= fromUnixTimestamp64Milli({from:Int64}) AND timestamp <= fromUnixTimestamp64Milli({to:Int64}) ORDER BY timestamp LIMIT 200";
const SESSION_SQL: &str = "SELECT toUnixTimestamp64Milli(min(started)) AS started_ms, toUnixTimestamp64Milli(max(timestamp)) AS ts, argMax(status, timestamp) AS status, \
    any(release) AS release, max(duration_ms) AS duration_ms FROM sessions WHERE project_id = {p:String} AND session_id = {s:String} HAVING count() > 0";

async fn load(state: &AppState, project_id: &str, session_id: &str, from: i64, to: i64) -> anyhow::Result<Vec<Value>> {
    let params = [("p", project_id.to_string()), ("s", session_id.to_string()), ("from", from.to_string()), ("to", to.to_string())];
    let events = state.ch.query(&EVENT_SQL.replace("LABEL", crate::product::LABEL), &params).await?;
    let errors = state.ch.query(ERROR_SQL, &params).await?;
    let sessions = state.ch.query(SESSION_SQL, &params).await?;
    Ok(merge(&events, &errors, &sessions))
}

async fn attach_short_ids(state: &AppState, items: &mut [Value]) {
    let mut cache: BTreeMap<String, Value> = BTreeMap::new();
    for it in items.iter_mut() {
        let Some(id) = it["issue_id"].as_str().filter(|s| !s.is_empty()).map(String::from) else { continue };
        if !cache.contains_key(&id) && cache.len() < 50 {
            let short = crate::query::get_issue_row(state, &id).await.ok().flatten().map(|i| i["short_id"].clone()).unwrap_or(Value::Null);
            cache.insert(id.clone(), short);
        }
        if let Some(s) = cache.get(&id) {
            it["issue"] = s.clone();
        }
    }
}

fn strip(items: &mut [Value]) {
    for it in items.iter_mut() {
        if let Some(o) = it.as_object_mut() {
            o.remove("ts_ms");
        }
    }
}

/// Everything recorded for one session, oldest first.
pub async fn session_timeline(state: &AppState, p: &Principal, project: &str, session_id: &str, limit: Option<i64>) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if session_id.trim().is_empty() {
        return Err(ApiError::bad("session id is required"));
    }
    let now = crate::util::now_ms();
    let mut items = load(state, &proj.id, session_id, now - 30 * 86_400_000, now + 60_000).await?;
    for f in crate::feedback::for_timeline(state, &proj.id, Some(session_id), None, now - 30 * 86_400_000, now + 60_000).await {
        let ts = f["ts_ms"].as_i64().unwrap_or(0);
        let title = crate::util::truncate(f["message"].as_str().unwrap_or(""), 300);
        items.push(entry(ts, "feedback", title, json!({"feedback": f["short_id"], "sentiment": f["sentiment"], "source": f["source"], "screenshots": f["screenshots"]})));
    }
    items.sort_by_key(|e| e["ts_ms"].as_i64().unwrap_or(0));
    let limit = limit.unwrap_or(500).clamp(1, 2000) as usize;
    let truncated = items.len() > limit;
    if truncated {
        items = items.split_off(items.len() - limit);
    }
    attach_short_ids(state, &mut items).await;
    strip(&mut items);
    let crashes = items.iter().filter(|i| i["kind"] == "crash" || i["kind"] == "error").count();
    Ok(json!({"project": proj.slug, "session_id": session_id, "entries": items.len(), "errors": crashes, "truncated": truncated, "timeline": items,
        "hint": "Oldest first. Kinds: session_start, pageview, screen_view, event, breadcrumb, error, crash, feedback, session_end. Open feedback with get_feedback."}))
}

/// The last `max` timeline entries within `minutes` before an error event (inclusive).
pub async fn before_event(state: &AppState, project_id: &str, ev: &Value, minutes: i64, max: usize) -> Value {
    let Some(ts) = ev.get("timestamp").and_then(parse_timestamp) else { return Value::Null };
    let session = ev["session_id"].as_str().unwrap_or("");
    let mut items = if session.is_empty() {
        // No session: the event's own breadcrumbs are all we have.
        let crumbs = ev["breadcrumbs"].clone();
        let row = json!({"ts": ts, "event_id": ev["event_id"], "issue_id": ev["issue_id"], "exception_type": ev["exception_type"], "exception_value": ev["exception_value"],
            "message": ev["message"], "culprit": ev["culprit"], "release": ev["release"], "fatal": if ev["fatal"] == json!(true) { 1 } else { 0 },
            "breadcrumbs": crumbs.to_string()});
        merge(&[], &[row], &[])
    } else {
        match load(state, project_id, session, ts - minutes * 60_000, ts + 1_000).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("timeline before crash failed: {e:#}");
                return Value::Null;
            }
        }
    };
    items.retain(|i| n(&i["ts_ms"]) <= ts + 1_000 && i["kind"] != "session_end");
    // End at the crash itself: drop anything recorded after this event.
    if let Some(pos) = items.iter().position(|i| i["event_id"] == ev["event_id"] && !ev["event_id"].is_null()) {
        items.truncate(pos + 1);
    }
    if items.len() > max {
        items = items.split_off(items.len() - max);
    }
    strip(&mut items);
    json!(items)
}

// ---------------------------------------------------------------------------
// Crash precursors
// ---------------------------------------------------------------------------

/// What users most often did in the minutes before an issue, compared with all sessions.
pub async fn precursors(state: &AppState, p: &Principal, issue: &str, minutes: Option<i64>, limit: Option<i64>) -> QResult<Value> {
    let issue = crate::query::issue_for(state, p, issue).await?;
    let project_id = issue["project_id"].as_str().unwrap_or("").to_string();
    let issue_id = issue["id"].as_str().unwrap_or("").to_string();
    let minutes = minutes.unwrap_or(5).clamp(1, 60);
    let limit = limit.unwrap_or(15).clamp(1, 100);
    let params = [("p", project_id.clone()), ("i", issue_id.clone()), ("w", minutes.to_string()), ("lim", limit.to_string())];
    let steps = state
        .ch
        .query(
            &format!(
                "WITH crashes AS (SELECT session_id, max(timestamp) AS ts FROM errors WHERE project_id = {{p:String}} AND issue_id = {{i:String}} AND session_id != '' \
                   GROUP BY session_id ORDER BY ts DESC LIMIT 1000) \
                 SELECT step, uniqExact(sid) AS sessions FROM ( \
                   SELECT e.session_id AS sid, {} AS step FROM events AS e INNER JOIN crashes AS c ON e.session_id = c.session_id \
                   WHERE e.project_id = {{p:String}} AND e.timestamp <= c.ts AND e.timestamp >= c.ts - toIntervalMinute({{w:UInt32}})) \
                 GROUP BY step ORDER BY sessions DESC LIMIT {{lim:UInt32}}",
                crate::product::LABEL.replace("type", "e.type").replace("path", "e.path").replace("props", "e.props").replace("name)", "e.name)")
            ),
            &params,
        )
        .await?;
    let crash_sessions = state
        .ch
        .query("SELECT uniqExact(session_id) AS n FROM errors WHERE project_id = {p:String} AND issue_id = {i:String} AND session_id != ''", &params)
        .await?
        .first()
        .map(|r| n(&r["n"]))
        .unwrap_or(0);
    let baseline = state
        .ch
        .query(
            &format!(
                "SELECT {} AS step, uniqExact(session_id) AS sessions FROM events WHERE project_id = {{p:String}} AND session_id != '' \
                 AND timestamp > now64(3) - INTERVAL 7 DAY GROUP BY step ORDER BY sessions DESC LIMIT 5000",
                crate::product::LABEL
            ),
            &params,
        )
        .await?;
    let total_sessions = state
        .ch
        .query("SELECT uniqExact(session_id) AS n FROM events WHERE project_id = {p:String} AND session_id != '' AND timestamp > now64(3) - INTERVAL 7 DAY", &params)
        .await?
        .first()
        .map(|r| n(&r["n"]))
        .unwrap_or(0);
    let base: BTreeMap<String, i64> = baseline.iter().map(|r| (r["step"].as_str().unwrap_or("").to_string(), n(&r["sessions"]))).collect();
    let step_rows: Vec<Value> = steps
        .iter()
        .map(|r| {
            let step = r["step"].as_str().unwrap_or("").to_string();
            let s = n(&r["sessions"]);
            let share = if crash_sessions > 0 { s as f64 / crash_sessions as f64 } else { 0.0 };
            let base_share = match (base.get(&step), total_sessions) {
                (Some(b), t) if t > 0 => Some(*b as f64 / t as f64),
                _ => None,
            };
            let lift = base_share.filter(|b| *b > 0.0).map(|b| (share / b * 100.0).round() / 100.0);
            json!({"step": step, "sessions": s, "share_of_crash_sessions": (share * 1000.0).round() / 1000.0,
                "share_of_all_sessions": base_share.map(|b| (b * 1000.0).round() / 1000.0), "lift": lift})
        })
        .collect();
    // Breadcrumbs from the issue's recent events (no baseline: they only exist on errors).
    let rows = state
        .ch
        .query(
            "SELECT toUnixTimestamp64Milli(timestamp) AS ts, breadcrumbs FROM errors WHERE project_id = {p:String} AND issue_id = {i:String} ORDER BY timestamp DESC LIMIT 200",
            &params,
        )
        .await?;
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for r in &rows {
        let ts = n(&r["ts"]);
        let crumbs: Value = serde_json::from_str(r["breadcrumbs"].as_str().unwrap_or("[]")).unwrap_or(json!([]));
        let mut uniq = HashSet::new();
        for b in crumbs.as_array().into_iter().flatten() {
            let nb = normalize_breadcrumb(b, ts);
            if nb["category"] == "error" || n(&nb["ts_ms"]) < ts - minutes * 60_000 {
                continue;
            }
            uniq.insert(format!("{}: {}", nb["category"].as_str().unwrap_or(""), crate::util::truncate(nb["message"].as_str().unwrap_or(""), 120)));
        }
        for u in uniq {
            *counts.entry(u).or_default() += 1;
        }
    }
    let mut crumb_rows: Vec<(String, i64)> = counts.into_iter().collect();
    crumb_rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let crumbs: Vec<Value> = crumb_rows
        .into_iter()
        .take(limit as usize)
        .map(|(k, c)| json!({"breadcrumb": k, "events": c, "share_of_events": if rows.is_empty() { 0.0 } else { (c as f64 / rows.len() as f64 * 1000.0).round() / 1000.0 }}))
        .collect();
    Ok(json!({
        "issue": issue["short_id"], "window_minutes": minutes, "sessions_with_issue": crash_sessions, "sessions_last_7d": total_sessions,
        "steps": step_rows, "breadcrumbs": crumbs, "events_sampled": rows.len(),
        "hint": "lift > 1 means the step is more common right before this issue than in sessions overall; high lift with a high share is a likely trigger.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumbs_normalize() {
        let b = normalize_breadcrumb(&json!({"category": "fetch", "level": "warn", "message": "GET /api 500", "timestamp": "2026-11-13T10:00:00Z"}), 0);
        assert_eq!(b["category"], "http");
        assert_eq!(b["level"], "warning");
        assert_eq!(b["ts_ms"], 1_794_564_000_000i64);
        let b = normalize_breadcrumb(&json!({"category": "http", "data": {"method": "POST", "url": "/pay", "status_code": 502}}), 5);
        assert_eq!(b["message"], "POST /pay 502");
        assert_eq!(b["ts_ms"], 5);
        assert_eq!(normalize_breadcrumb(&json!("plain"), 7)["message"], "plain");
        assert_eq!(normalize_category("Tap"), "ui.click");
    }

    #[test]
    fn merges_in_order() {
        let events = vec![
            json!({"ts": 1000, "type": "pageview", "label": "pageview /", "url": "https://x/", "props": "{}"}),
            json!({"ts": 3000, "type": "custom", "label": "add_to_cart", "url": "", "props_json": "{\"sku\":\"a\"}"}),
        ];
        let crumbs = json!([{"category": "ui.click", "message": "button#buy", "timestamp": 3.5}, {"category": "error", "message": "boom", "timestamp": 4.0}]).to_string();
        let errors = vec![
            json!({"ts": 4000, "event_id": "e1", "issue_id": "i1", "exception_type": "TypeError", "exception_value": "boom", "message": "", "culprit": "buy", "release": "1", "fatal": 0, "breadcrumbs": crumbs.clone()}),
            json!({"ts": 5000, "event_id": "e2", "issue_id": "i1", "exception_type": "TypeError", "exception_value": "boom", "message": "", "culprit": "buy", "release": "1", "fatal": 1, "breadcrumbs": crumbs}),
        ];
        let sessions = vec![json!({"started_ms": 500, "ts": 5000, "status": "crashed", "release": "1", "duration_ms": 4500})];
        let t = merge(&events, &errors, &sessions);
        let kinds: Vec<&str> = t.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["session_start", "pageview", "event", "breadcrumb", "error", "crash", "session_end"]);
        assert_eq!(t[2]["props"]["sku"], "a");
        assert_eq!(t[3]["title"], "ui.click: button#buy");
    }
}
