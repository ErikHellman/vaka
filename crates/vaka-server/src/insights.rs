//! Release adoption and real-time traffic. Shared by the REST API and MCP.

use crate::auth::{ApiError, Principal};
use crate::query::{analytics_where, project_for, AnalyticsParams, QResult};
use crate::state::AppState;
use crate::util::{iso_time, now_ms, parse_time_expr};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

fn num(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)
}

fn share(n: f64, d: f64) -> f64 {
    if d <= 0.0 {
        0.0
    } else {
        (n / d * 10_000.0).round() / 10_000.0
    }
}

/// Share of sessions and users on each release over time, for the most used releases.
pub async fn release_adoption(
    state: &AppState,
    p: &Principal,
    project: &str,
    since: Option<&str>,
    interval: Option<&str>,
    environment: Option<&str>,
    limit: Option<i64>,
) -> QResult<Value> {
    let project = project_for(state, p, project).await?;
    let now = now_ms();
    let since = since.and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 14 * 86_400_000);
    let interval = match interval {
        Some("hour") => "hour",
        Some("day") => "day",
        None => if now - since <= 2 * 86_400_000 { "hour" } else { "day" },
        Some(other) => return Err(ApiError::bad(format!("unknown interval `{other}`: hour or day"))),
    };
    let bucket = if interval == "hour" { "toStartOfHour(started)" } else { "toStartOfDay(started)" };
    let mut cond = "project_id = {p:String} AND started >= fromUnixTimestamp64Milli({since:Int64})".to_string();
    let mut ps = vec![("p", project.id.clone()), ("since", since.to_string())];
    if let Some(env) = environment.filter(|e| !e.is_empty()) {
        cond.push_str(" AND environment = {env:String}");
        ps.push(("env", env.to_string()));
    }
    let rows = state
        .ch
        .query(
            &format!(
                "SELECT formatDateTime({bucket}, '%Y-%m-%dT%H:%i:%SZ') AS t, release, uniq(session_id) AS sessions, uniq(distinct_id) AS users \
                 FROM sessions WHERE {cond} GROUP BY t, release ORDER BY t"
            ),
            &ps,
        )
        .await?;
    let firsts = state
        .ch
        .query(
            &format!("SELECT release, toString(min(started)) AS first_session, uniq(session_id) AS sessions FROM sessions WHERE {cond} GROUP BY release"),
            &ps,
        )
        .await?;
    let limit = limit.unwrap_or(5).clamp(1, 20) as usize;
    let mut ranked: Vec<(String, f64, String)> =
        firsts.iter().map(|r| (r["release"].as_str().unwrap_or("").to_string(), num(&r["sessions"]), r["first_session"].as_str().unwrap_or("").to_string())).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top: Vec<String> = ranked.iter().take(limit).map(|r| r.0.clone()).collect();
    let label = |r: &str| if top.iter().any(|t| t == r) { if r.is_empty() { "(none)".to_string() } else { r.to_string() } } else { "other".to_string() };

    // bucket -> release label -> (sessions, users)
    let mut buckets: BTreeMap<String, BTreeMap<String, (f64, f64)>> = BTreeMap::new();
    for r in &rows {
        let b = buckets.entry(r["t"].as_str().unwrap_or("").to_string()).or_default();
        let e = b.entry(label(r["release"].as_str().unwrap_or(""))).or_default();
        e.0 += num(&r["sessions"]);
        // Users can span releases; summing per label over-counts slightly for "other" only.
        e.1 += num(&r["users"]);
    }
    let mut series = Vec::new();
    for (t, rels) in &buckets {
        let total_s: f64 = rels.values().map(|v| v.0).sum();
        let total_u: f64 = rels.values().map(|v| v.1).sum();
        let mut m = Map::new();
        for (rel, (s, u)) in rels {
            m.insert(rel.clone(), json!({"sessions": *s as i64, "users": *u as i64, "share_sessions": share(*s, total_s), "share_users": share(*u, total_u)}));
        }
        series.push(json!({"t": t, "sessions": total_s as i64, "releases": m}));
    }
    // Per-release summary: first seen, current share (last bucket) and peak share.
    let mut summary = Vec::new();
    for (rel, sessions, first) in ranked.iter().take(limit) {
        let name = label(rel);
        let shares: Vec<f64> = series.iter().map(|s| s["releases"][&name]["share_sessions"].as_f64().unwrap_or(0.0)).collect();
        summary.push(json!({
            "release": name,
            "sessions": *sessions as i64,
            "first_session": first,
            "current_share_sessions": shares.last().copied().unwrap_or(0.0),
            "peak_share_sessions": shares.iter().copied().fold(0.0, f64::max),
        }));
    }
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(since), "until": iso_time(now)},
        "interval": interval,
        "note": "share_* are fractions of all sessions (or users) in the bucket; releases outside the top ones are grouped as `other`.",
        "releases": summary,
        "series": series,
    }))
}

/// Who is on the site or app right now: the last few minutes of traffic.
pub async fn realtime(state: &AppState, p: &Principal, project: &str, minutes: Option<i64>) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let minutes = minutes.unwrap_or(5).clamp(1, 60);
    let now = now_ms();
    let a = AnalyticsParams { project: proj.id.clone(), since: Some((now - minutes * 60_000).to_string()), ..Default::default() };
    let w = analytics_where(state, &proj.id, &a).await?;
    let totals = state
        .ch
        .query(
            &format!("SELECT uniq(visitor_id) AS visitors, countIf(type = 'pageview') AS pageviews, countIf(type = 'screen_view') AS screen_views, count() AS events FROM events WHERE {}", w.sql),
            &w.params,
        )
        .await?;
    let top = |expr: &str, n: u32| format!("SELECT {expr} AS value, uniq(visitor_id) AS visitors, count() AS events FROM events WHERE {} AND value != '' GROUP BY value ORDER BY visitors DESC, events DESC LIMIT {n}", w.sql);
    let pages = state.ch.query(&top("if(type = 'screen_view', props['screen'], path)", 10), &w.params).await?;
    let referrers = state.ch.query(&top("referrer_host", 5), &w.params).await?;
    let countries = state.ch.query(&top("country", 5), &w.params).await?;
    let events = state.ch.query(&format!("SELECT name AS value, count() AS events FROM events WHERE {} AND type = 'custom' GROUP BY value ORDER BY events DESC LIMIT 5", w.sql), &w.params).await?;
    let ps = vec![("p", proj.id.clone()), ("since", (now - minutes * 60_000).to_string()), ("since30", (now - 30 * 60_000).to_string())];
    let per_minute = state
        .ch
        .query(
            "SELECT formatDateTime(toStartOfMinute(timestamp), '%Y-%m-%dT%H:%i:00Z') AS minute, uniq(visitor_id) AS visitors, count() AS events \
             FROM events WHERE project_id = {p:String} AND timestamp >= fromUnixTimestamp64Milli({since30:Int64}) GROUP BY minute ORDER BY minute",
            &ps,
        )
        .await?;
    let sessions = state
        .ch
        .query(
            "SELECT uniq(session_id) AS active_sessions FROM sessions WHERE project_id = {p:String} AND timestamp >= fromUnixTimestamp64Milli({since:Int64})",
            &ps,
        )
        .await?;
    let errors = state
        .ch
        .query(
            "SELECT count() AS errors, uniq(issue_id) AS issues FROM errors WHERE project_id = {p:String} AND timestamp >= fromUnixTimestamp64Milli({since:Int64})",
            &ps,
        )
        .await?;
    let t = totals.first().cloned().unwrap_or(json!({}));
    Ok(json!({
        "project": proj.slug,
        "window": {"since": iso_time(now - minutes * 60_000), "until": iso_time(now), "minutes": minutes},
        "visitors": t["visitors"],
        "pageviews": t["pageviews"],
        "screen_views": t["screen_views"],
        "events": t["events"],
        "active_sessions": sessions.first().map(|r| r["active_sessions"].clone()),
        "errors": errors.first().map(|r| r["errors"].clone()),
        "issues": errors.first().map(|r| r["issues"].clone()),
        "top_pages": pages,
        "top_referrers": referrers,
        "top_countries": countries,
        "top_events": events,
        "per_minute_last_30": per_minute,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares() {
        assert_eq!(share(1.0, 3.0), 0.3333);
        assert_eq!(share(1.0, 0.0), 0.0);
        assert_eq!(num(&json!("12")), 12.0);
    }
}
