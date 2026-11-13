//! Performance tracing: spans (root spans are transactions), slow endpoints, app start, Web
//! Vitals, traces and release-over-release regressions.

use crate::auth::{ApiError, Principal};
use crate::models::SpanItem;
use crate::processor::SpanRow;
use crate::query::{project_for, QResult};
use crate::state::{AppState, ProjectInfo};
use crate::util::{ch_time, iso_time, now_ms, parse_time_expr, parse_timestamp, truncate};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Web Vitals: (name, good up to, poor above, unit). Values follow web.dev thresholds.
pub const VITALS: &[(&str, f64, f64, &str)] = &[
    ("lcp", 2500.0, 4000.0, "ms"),
    ("inp", 200.0, 500.0, "ms"),
    ("cls", 0.1, 0.25, ""),
    ("fcp", 1800.0, 3000.0, "ms"),
    ("ttfb", 800.0, 1800.0, "ms"),
];

fn is_id_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    let digits = seg.chars().all(|c| c.is_ascii_digit());
    let hexish = seg.len() >= 16 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let uuid = seg.len() == 36 && seg.matches('-').count() == 4;
    digits || hexish || uuid
}

/// Group HTTP transactions: `GET /users/123?x=1` -> `GET /users/:id`.
pub fn normalize_http_name(name: &str) -> String {
    let (method, rest) = match name.split_once(' ') {
        Some((m, r)) if m.chars().all(|c| c.is_ascii_uppercase()) => (Some(m), r),
        _ => (None, name),
    };
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let path = match path.find("://") {
        Some(i) => path[i + 3..].find('/').map(|j| &path[i + 3 + j..]).unwrap_or("/"),
        None => path,
    };
    let norm: Vec<&str> = path.split('/').map(|seg| if is_id_segment(seg) { ":id" } else { seg }).collect();
    let p = norm.join("/");
    match method {
        Some(m) => format!("{m} {p}"),
        None => p,
    }
}

fn status_of(s: &SpanItem) -> String {
    let code = s.http.as_ref().and_then(|h| h.status_code).unwrap_or(0);
    match s.status.as_deref().map(str::to_lowercase).as_deref() {
        Some("error" | "internal_error" | "unknown_error" | "failed") => "error".into(),
        Some("cancelled" | "canceled" | "aborted") => "cancelled".into(),
        _ if code >= 500 => "error".into(),
        Some(other) if !other.is_empty() && other != "ok" && other != "unset" => truncate(other, 32),
        _ => "ok".into(),
    }
}

/// Build the ClickHouse row for an ingested span.
pub fn span_row(project: &ProjectInfo, s: &SpanItem, received_at: i64) -> Result<SpanRow, String> {
    let trace_id = s.trace_id.trim().to_lowercase();
    if trace_id.is_empty() || trace_id.len() > 64 {
        return Err("span items need a `trace_id` (up to 64 characters)".into());
    }
    let start = s.start_timestamp.as_ref().and_then(parse_timestamp);
    let end = s.timestamp.as_ref().and_then(parse_timestamp);
    let duration = s.duration_ms.or_else(|| match (start, end) {
        (Some(a), Some(b)) if b >= a => Some((b - a) as f64),
        _ => None,
    });
    let start = start.or_else(|| end.map(|e| e - duration.unwrap_or(0.0) as i64)).unwrap_or(received_at);
    let op = truncate(s.op.as_deref().unwrap_or("default").trim(), 64);
    let parent = s.parent_span_id.clone().unwrap_or_default();
    let is_root = parent.is_empty() || s.is_transaction.unwrap_or(s.kind.as_deref() == Some("server"));
    let http = s.http.clone().unwrap_or_default();
    let mut name = truncate(s.name.trim(), 500);
    let grouped = op.starts_with("http") || matches!(op.as_str(), "pageload" | "navigation" | "web.vitals");
    if grouped {
        name = normalize_http_name(&name);
    }
    let transaction = s.transaction.clone().filter(|t| !t.is_empty()).map(|t| if grouped { normalize_http_name(&t) } else { truncate(&t, 500) });
    Ok(SpanRow {
        project_id: project.id.clone(),
        trace_id,
        span_id: s.span_id.clone().filter(|i| !i.is_empty()).unwrap_or_else(|| crate::util::new_id()[..16].to_string()),
        parent_span_id: parent,
        transaction: transaction.unwrap_or_else(|| if is_root { name.clone() } else { String::new() }),
        name,
        op,
        kind: truncate(s.kind.as_deref().unwrap_or(""), 16),
        status: status_of(s),
        is_root: is_root as u8,
        start: ch_time(start),
        duration_ms: duration.unwrap_or(0.0).max(0.0),
        release: s.release.clone().unwrap_or_default(),
        environment: s.environment.clone().unwrap_or_else(|| "production".into()),
        platform: s.platform.clone().unwrap_or_else(|| project.platform.clone()),
        service: truncate(s.service.as_deref().unwrap_or(""), 128),
        session_id: s.session_id.clone().unwrap_or_default(),
        user_id: s.user_id.clone().unwrap_or_default(),
        http_method: http.method.unwrap_or_default().to_uppercase(),
        http_status: http.status_code.unwrap_or(0),
        attributes: s.attributes.iter().take(64).map(|(k, v)| (truncate(k, 128), truncate(&crate::models::value_to_string(v), 1024))).collect(),
        measurements: s.measurements.iter().take(32).filter(|(_, v)| v.is_finite()).map(|(k, v)| (truncate(&k.to_lowercase(), 64), *v)).collect(),
        retention_days: project.retention_days as u16,
    })
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct PerfParams {
    pub project: String,
    pub since: Option<String>,
    pub until: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub op: Option<String>,
    pub transaction: Option<String>,
    pub limit: Option<i64>,
    pub sort: Option<String>,
}

struct W {
    sql: String,
    params: Vec<(&'static str, String)>,
    since: i64,
    until: i64,
}

fn where_for(project_id: &str, a: &PerfParams, roots_only: bool) -> W {
    let now = now_ms();
    let since = a.since.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 86_400_000);
    let until = a.until.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now + 60_000);
    let mut conds = vec![
        "project_id = {p:String}".to_string(),
        "start >= fromUnixTimestamp64Milli({since:Int64})".to_string(),
        "start < fromUnixTimestamp64Milli({until:Int64})".to_string(),
    ];
    let mut params: Vec<(&'static str, String)> = vec![("p", project_id.to_string()), ("since", since.to_string()), ("until", until.to_string())];
    if roots_only {
        conds.push("is_root = 1".into());
    }
    if let Some(e) = a.environment.as_deref().filter(|e| !e.is_empty()) {
        conds.push("environment = {env:String}".into());
        params.push(("env", e.to_string()));
    }
    if let Some(r) = a.release.as_deref().filter(|e| !e.is_empty()) {
        conds.push("release = {rel:String}".into());
        params.push(("rel", r.to_string()));
    }
    if let Some(op) = a.op.as_deref().filter(|e| !e.is_empty()) {
        if let Some(prefix) = op.strip_suffix('*') {
            conds.push("startsWith(op, {op:String})".into());
            params.push(("op", prefix.to_string()));
        } else {
            conds.push("op = {op:String}".into());
            params.push(("op", op.to_string()));
        }
    }
    if let Some(t) = a.transaction.as_deref().filter(|e| !e.is_empty()) {
        conds.push("transaction = {tx:String}".into());
        params.push(("tx", t.to_string()));
    }
    W { sql: conds.join(" AND "), params, since, until }
}

fn round(v: &Value, digits: i32) -> Value {
    match v.as_f64() {
        Some(f) if f.is_finite() => {
            let m = 10f64.powi(digits);
            json!((f * m).round() / m)
        }
        _ => Value::Null,
    }
}

fn num(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)
}

const STATS: &str = "count() AS count, round(count() / greatest(dateDiff('minute', fromUnixTimestamp64Milli({since:Int64}), least(now64(3), fromUnixTimestamp64Milli({until:Int64}))), 1), 3) AS per_minute, \
    quantiles(0.5, 0.75, 0.95, 0.99)(duration_ms) AS q, avg(duration_ms) AS avg_ms, max(duration_ms) AS max_ms, \
    countIf(status = 'error') / count() AS error_rate";

fn stats_json(r: &Value) -> Value {
    let q = r["q"].as_array().cloned().unwrap_or_default();
    json!({
        "count": r["count"], "per_minute": r["per_minute"],
        "p50_ms": round(q.first().unwrap_or(&Value::Null), 1), "p75_ms": round(q.get(1).unwrap_or(&Value::Null), 1),
        "p95_ms": round(q.get(2).unwrap_or(&Value::Null), 1), "p99_ms": round(q.get(3).unwrap_or(&Value::Null), 1),
        "avg_ms": round(&r["avg_ms"], 1), "max_ms": round(&r["max_ms"], 1), "error_rate": round(&r["error_rate"], 4),
    })
}

/// Per-transaction latency percentiles, throughput and error rate.
pub async fn summary(state: &AppState, p: &Principal, a: PerfParams) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    let w = where_for(&proj.id, &a, true);
    let order = match a.sort.as_deref().unwrap_or("p95") {
        "count" => "count",
        "p50" => "q[1]",
        "p99" => "q[4]",
        "errors" | "error_rate" => "error_rate",
        "total" => "sum(duration_ms)",
        _ => "q[3]",
    };
    let mut params = w.params.clone();
    params.push(("lim", a.limit.unwrap_or(20).clamp(1, 200).to_string()));
    let rows = state
        .ch
        .query(
            &format!("SELECT transaction, any(op) AS top_op, {STATS} FROM spans WHERE {} GROUP BY transaction ORDER BY {order} DESC LIMIT {{lim:UInt32}}", w.sql),
            &params,
        )
        .await?;
    let total = state.ch.query(&format!("SELECT {STATS} FROM spans WHERE {}", w.sql), &w.params).await?;
    let txs: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = stats_json(r);
            v["transaction"] = r["transaction"].clone();
            v["op"] = r["top_op"].clone();
            v
        })
        .collect();
    Ok(json!({"project": proj.slug, "since": iso_time(w.since), "until": iso_time(w.until.min(now_ms())), "overall": total.first().map(stats_json).unwrap_or(Value::Null), "transactions": txs}))
}

/// Slowest HTTP server endpoints by p95 (at least `min_count` requests).
pub async fn slow_endpoints(state: &AppState, p: &Principal, mut a: PerfParams, min_count: Option<i64>) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    if a.op.as_deref().unwrap_or("").is_empty() {
        a.op = Some("http.server*".into());
    }
    let w = where_for(&proj.id, &a, true);
    let mut params = w.params.clone();
    params.push(("lim", a.limit.unwrap_or(10).clamp(1, 100).to_string()));
    params.push(("minc", min_count.unwrap_or(5).max(1).to_string()));
    let rows = state
        .ch
        .query(
            &format!(
                "SELECT transaction, {STATS}, sum(duration_ms) AS total_ms, countIf(http_status >= 500) AS server_errors FROM spans WHERE {} \
                 GROUP BY transaction HAVING count >= {{minc:UInt32}} ORDER BY q[3] DESC LIMIT {{lim:UInt32}}",
                w.sql
            ),
            &params,
        )
        .await?;
    let endpoints: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = stats_json(r);
            v["endpoint"] = r["transaction"].clone();
            v["total_time_ms"] = round(&r["total_ms"], 0);
            v["server_errors"] = r["server_errors"].clone();
            v
        })
        .collect();
    Ok(json!({"project": proj.slug, "since": iso_time(w.since), "endpoints": endpoints,
        "hint": "Use span_breakdown with an endpoint as `transaction` to see where its time goes, and list_slow_traces + get_trace for examples."}))
}

/// Cold and warm app start durations by release.
pub async fn app_startup(state: &AppState, p: &Principal, mut a: PerfParams) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    a.op = None;
    let w = where_for(&proj.id, &a, false);
    let mut params = w.params.clone();
    params.push(("lim", a.limit.unwrap_or(10).clamp(1, 50).to_string()));
    let rows = state
        .ch
        .query(
            &format!(
                "SELECT release, if(op = 'app.start.warm', 'warm', 'cold') AS start_type, {STATS}, max(start) AS last_seen FROM spans WHERE {} \
                 AND op IN ('app.start', 'app.start.cold', 'app.start.warm') GROUP BY release, start_type ORDER BY last_seen DESC LIMIT {{lim:UInt32}}",
                w.sql
            ),
            &params,
        )
        .await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = stats_json(r);
            v["release"] = r["release"].clone();
            v["start_type"] = r["start_type"].clone();
            v
        })
        .collect();
    Ok(json!({"project": proj.slug, "since": iso_time(w.since), "app_starts": out,
        "hint": "Send spans with op app.start.cold / app.start.warm (duration from process start to first frame)."}))
}

fn vital_sql() -> String {
    let names: Vec<String> = VITALS.iter().map(|v| format!("'{}'", v.0)).collect();
    let good: Vec<String> = VITALS.iter().map(|v| v.1.to_string()).collect();
    let poor: Vec<String> = VITALS.iter().map(|v| v.2.to_string()).collect();
    format!(
        "k AS metric, count() AS samples, quantile(0.75)(v) AS p75, quantile(0.5)(v) AS p50, \
         countIf(v <= transform(k, [{n}], [{g}], 0.0)) / count() AS good, \
         countIf(v > transform(k, [{n}], [{pr}], 0.0)) / count() AS poor",
        n = names.join(", "),
        g = good.join(", "),
        pr = poor.join(", ")
    )
}

fn rating(metric: &str, p75: f64) -> &'static str {
    match VITALS.iter().find(|v| v.0 == metric) {
        Some((_, good, poor, _)) if p75 <= *good => "good",
        Some((_, _, poor, _)) if p75 > *poor => "poor",
        Some(_) => "needs-improvement",
        None => "unknown",
    }
}

fn vital_rows(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            let m = r["metric"].as_str().unwrap_or("");
            let p75 = num(&r["p75"]);
            let digits = if m == "cls" { 3 } else { 0 };
            json!({"metric": m, "samples": r["samples"], "p75": round(&r["p75"], digits), "p50": round(&r["p50"], digits),
                "rating": rating(m, p75), "good_share": round(&r["good"], 3), "poor_share": round(&r["poor"], 3), "page": r["page"]})
        })
        .collect()
}

/// Web Vitals (p75 and good/poor shares) overall and for the pages with the most samples.
pub async fn web_vitals(state: &AppState, p: &Principal, mut a: PerfParams, by_page: Option<bool>) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    a.op = None;
    let page_filter = a.transaction.take();
    let w = where_for(&proj.id, &a, false);
    let mut params = w.params.clone();
    let mut cond = format!("{} AND op = 'web.vitals'", w.sql);
    if let Some(pg) = page_filter.filter(|p| !p.is_empty()) {
        cond.push_str(" AND transaction = {page:String}");
        params.push(("page", pg));
    }
    let known: Vec<String> = VITALS.iter().map(|v| format!("'{}'", v.0)).collect();
    let base = format!("FROM spans ARRAY JOIN mapKeys(measurements) AS k, mapValues(measurements) AS v WHERE {cond} AND k IN ({})", known.join(", "));
    let overall = state.ch.query(&format!("SELECT {}, '' AS page {base} GROUP BY metric ORDER BY metric", vital_sql()), &params).await?;
    let mut out = json!({"project": proj.slug, "since": iso_time(w.since), "overall": vital_rows(&overall),
        "thresholds": VITALS.iter().map(|v| json!({"metric": v.0, "good_up_to": v.1, "poor_above": v.2, "unit": v.3})).collect::<Vec<_>>()});
    if by_page.unwrap_or(true) {
        params.push(("lim", a.limit.unwrap_or(10).clamp(1, 50).to_string()));
        let pages = state
            .ch
            .query(
                &format!(
                    "SELECT {}, transaction AS page {base} AND transaction IN (SELECT transaction FROM spans WHERE {cond} GROUP BY transaction ORDER BY count() DESC LIMIT {{lim:UInt32}}) \
                     GROUP BY page, metric ORDER BY page, metric",
                    vital_sql()
                ),
                &params,
            )
            .await?;
        let mut grouped: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for v in vital_rows(&pages) {
            grouped.entry(v["page"].as_str().unwrap_or("").to_string()).or_default().push(v);
        }
        out["pages"] = json!(grouped.into_iter().map(|(page, metrics)| json!({"page": page, "metrics": metrics})).collect::<Vec<_>>());
    }
    Ok(out)
}

/// One trace as a tree, with errors that carry the trace id.
pub async fn get_trace(state: &AppState, p: &Principal, project: &str, trace_id: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let tid = trace_id.trim().to_lowercase();
    let params = [("p", proj.id.clone()), ("t", tid.clone())];
    let rows = state
        .ch
        .query(
            "SELECT span_id, parent_span_id, name, op, status, transaction, toUnixTimestamp64Milli(start) AS start_ms, duration_ms, release, environment, service, \
             http_method, http_status, attributes, measurements FROM spans WHERE project_id = {p:String} AND trace_id = {t:String} ORDER BY start LIMIT 2000",
            &params,
        )
        .await?;
    if rows.is_empty() {
        return Err(ApiError::not_found("trace"));
    }
    let errors = state
        .ch
        .query(
            "SELECT toString(event_id) AS event_id, issue_id, exception_type, exception_value, message, toUnixTimestamp64Milli(timestamp) AS ts FROM errors \
             WHERE project_id = {p:String} AND trace_id = {t:String} ORDER BY timestamp LIMIT 100",
            &params,
        )
        .await?;
    let t0 = rows.iter().map(|r| num(&r["start_ms"]) as i64).min().unwrap_or(0);
    let t1 = rows.iter().map(|r| num(&r["start_ms"]) + num(&r["duration_ms"])).fold(0.0, f64::max);
    let ids: std::collections::HashSet<String> = rows.iter().map(|r| r["span_id"].as_str().unwrap_or("").to_string()).collect();
    let mut children: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut roots = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let parent = r["parent_span_id"].as_str().unwrap_or("");
        if parent.is_empty() || !ids.contains(parent) {
            roots.push(i);
        } else {
            children.entry(parent.to_string()).or_default().push(i);
        }
    }
    let mut flat = Vec::new();
    fn walk(i: usize, depth: usize, rows: &[Value], children: &BTreeMap<String, Vec<usize>>, t0: i64, out: &mut Vec<Value>) {
        let r = &rows[i];
        let id = r["span_id"].as_str().unwrap_or("");
        let kids = children.get(id).cloned().unwrap_or_default();
        let child_time: f64 = kids.iter().map(|k| num(&rows[*k]["duration_ms"])).sum();
        let dur = num(&r["duration_ms"]);
        let mut v = json!({
            "span_id": id, "parent_span_id": r["parent_span_id"], "depth": depth, "name": r["name"], "op": r["op"], "status": r["status"],
            "offset_ms": num(&r["start_ms"]) as i64 - t0, "duration_ms": round(&r["duration_ms"], 2), "self_time_ms": round(&json!((dur - child_time).max(0.0)), 2),
        });
        for k in ["service", "http_method", "release"] {
            if r[k].as_str().map(|s| !s.is_empty()).unwrap_or(false) {
                v[k] = r[k].clone();
            }
        }
        if num(&r["http_status"]) > 0.0 {
            v["http_status"] = r["http_status"].clone();
        }
        for k in ["attributes", "measurements"] {
            if r[k].as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                v[k] = r[k].clone();
            }
        }
        out.push(v);
        if out.len() < 2000 {
            for k in kids {
                walk(k, depth + 1, rows, children, t0, out);
            }
        }
    }
    for r in &roots {
        walk(*r, 0, &rows, &children, t0, &mut flat);
    }
    let errs: Vec<Value> = errors
        .iter()
        .map(|e| json!({"event_id": e["event_id"], "issue_id": e["issue_id"], "offset_ms": num(&e["ts"]) as i64 - t0,
            "title": if e["exception_type"].as_str().unwrap_or("").is_empty() { e["message"].clone() } else { json!(format!("{}: {}", e["exception_type"].as_str().unwrap_or(""), e["exception_value"].as_str().unwrap_or(""))) }}))
        .collect();
    let root = &rows[roots.first().copied().unwrap_or(0)];
    let mut out = json!({
        "project": proj.slug, "trace_id": tid, "transaction": root["transaction"], "start": iso_time(t0), "duration_ms": round(&json!(t1 - t0 as f64), 2),
        "span_count": rows.len(), "spans": flat, "errors": errs,
        "hint": "Spans are depth-first; self_time_ms is time not covered by child spans. The span with the largest self time is usually where to look.",
    });
    if let Ok(logs) = crate::otel::logs_for_trace(state, &proj.id, &tid, t0).await {
        if !logs.is_empty() {
            out["logs"] = json!(logs);
        }
    }
    Ok(out)
}

/// Slowest root spans (examples to open with get_trace).
pub async fn slow_traces(state: &AppState, p: &Principal, a: PerfParams, min_ms: Option<f64>) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    let w = where_for(&proj.id, &a, true);
    let mut params = w.params.clone();
    params.push(("lim", a.limit.unwrap_or(10).clamp(1, 100).to_string()));
    params.push(("minms", min_ms.unwrap_or(0.0).max(0.0).to_string()));
    let rows = state
        .ch
        .query(
            &format!(
                "SELECT trace_id, transaction, op, status, duration_ms, release, toUnixTimestamp64Milli(start) AS start_ms, http_status FROM spans WHERE {} \
                 AND duration_ms >= {{minms:Float64}} ORDER BY duration_ms DESC LIMIT {{lim:UInt32}}",
                w.sql
            ),
            &params,
        )
        .await?;
    Ok(json!({"project": proj.slug, "traces": rows.iter().map(|r| json!({"trace_id": r["trace_id"], "transaction": r["transaction"], "op": r["op"], "status": r["status"],
        "duration_ms": round(&r["duration_ms"], 1), "release": r["release"], "start": iso_time(num(&r["start_ms"]) as i64), "http_status": r["http_status"]})).collect::<Vec<_>>()}))
}

/// Where a transaction's time goes: child spans grouped by op and name.
pub async fn span_breakdown(state: &AppState, p: &Principal, a: PerfParams) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    let tx = a.transaction.clone().filter(|t| !t.is_empty()).ok_or_else(|| ApiError::bad("`transaction` is required"))?;
    let w = where_for(&proj.id, &a, true);
    let mut params = w.params.clone();
    params.push(("lim", a.limit.unwrap_or(15).clamp(1, 100).to_string()));
    let roots = format!("SELECT trace_id FROM spans WHERE {} ORDER BY start DESC LIMIT 1000", w.sql);
    let total = state
        .ch
        .query(&format!("SELECT count() AS n, sum(duration_ms) AS total FROM (SELECT duration_ms FROM spans WHERE {} ORDER BY start DESC LIMIT 1000)", w.sql), &params)
        .await?;
    let rows = state
        .ch
        .query(
            &format!(
                "SELECT op, name, count() AS count, sum(duration_ms) AS total_ms, avg(duration_ms) AS avg_ms, quantile(0.95)(duration_ms) AS p95_ms, \
                 countIf(status = 'error') AS errors FROM spans WHERE project_id = {{p:String}} AND is_root = 0 AND trace_id IN ({roots}) \
                 GROUP BY op, name ORDER BY total_ms DESC LIMIT {{lim:UInt32}}"
            ),
            &params,
        )
        .await?;
    let n = total.first().map(|r| num(&r["n"])).unwrap_or(0.0);
    let total_ms = total.first().map(|r| num(&r["total"])).unwrap_or(0.0);
    let parts: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({"op": r["op"], "name": r["name"], "count": r["count"], "per_transaction": if n > 0.0 { round(&json!(num(&r["count"]) / n), 2) } else { Value::Null },
                "total_ms": round(&r["total_ms"], 1), "avg_ms": round(&r["avg_ms"], 1), "p95_ms": round(&r["p95_ms"], 1), "errors": r["errors"],
                "share_of_transaction_time": if total_ms > 0.0 { round(&json!(num(&r["total_ms"]) / total_ms), 3) } else { Value::Null }})
        })
        .collect();
    Ok(json!({"project": proj.slug, "transaction": tx, "transactions_sampled": n, "avg_transaction_ms": if n > 0.0 { round(&json!(total_ms / n), 1) } else { Value::Null }, "spans": parts,
        "hint": "Shares can add up to more than 1 when child spans run in parallel."}))
}

/// Compare transaction latency, app start and Web Vitals between two releases.
pub async fn compare_releases(state: &AppState, p: &Principal, project: &str, base: &str, target: &str, min_count: Option<i64>, threshold: Option<f64>) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let v = compare_inner(state, &proj, base, target, min_count.unwrap_or(20), threshold.unwrap_or(0.2)).await?;
    Ok(v)
}

pub async fn compare_inner(state: &AppState, proj: &ProjectInfo, base: &str, target: &str, min_count: i64, threshold: f64) -> anyhow::Result<Value> {
    let params = [("p", proj.id.clone()), ("b", base.to_string()), ("t", target.to_string())];
    let rows = state
        .ch
        .query(
            "SELECT transaction, release, any(op) AS top_op, count() AS count, quantile(0.5)(duration_ms) AS p50, quantile(0.95)(duration_ms) AS p95, countIf(status = 'error') / count() AS error_rate \
             FROM spans WHERE project_id = {p:String} AND is_root = 1 AND release IN ({b:String}, {t:String}) AND op != 'web.vitals' \
             GROUP BY transaction, release",
            &params,
        )
        .await?;
    let vit_names: Vec<String> = VITALS.iter().map(|v| format!("'{}'", v.0)).collect();
    let vitals = state
        .ch
        .query(
            &format!(
                "SELECT k AS metric, release, count() AS count, quantile(0.75)(v) AS p75 FROM spans ARRAY JOIN mapKeys(measurements) AS k, mapValues(measurements) AS v \
                 WHERE project_id = {{p:String}} AND op = 'web.vitals' AND release IN ({{b:String}}, {{t:String}}) AND k IN ({}) GROUP BY metric, release",
                vit_names.join(", ")
            ),
            &params,
        )
        .await?;
    let mut by: BTreeMap<String, (Option<Value>, Option<Value>)> = BTreeMap::new();
    for r in rows.iter() {
        let e = by.entry(r["transaction"].as_str().unwrap_or("").to_string()).or_default();
        if r["release"] == base {
            e.0 = Some(r.clone());
        } else {
            e.1 = Some(r.clone());
        }
    }
    let mut regressions = Vec::new();
    let mut improvements = Vec::new();
    let mut table = Vec::new();
    for (tx, (b, t)) in by {
        let (Some(b), Some(t)) = (b, t) else { continue };
        let (bp, tp) = (num(&b["p95"]), num(&t["p95"]));
        let enough = num(&b["count"]) >= min_count as f64 && num(&t["count"]) >= min_count as f64;
        let change = if bp > 0.0 { (tp - bp) / bp } else { 0.0 };
        let row = json!({"transaction": tx, "op": t["top_op"], "base": {"count": b["count"], "p50_ms": round(&b["p50"], 1), "p95_ms": round(&b["p95"], 1), "error_rate": round(&b["error_rate"], 4)},
            "target": {"count": t["count"], "p50_ms": round(&t["p50"], 1), "p95_ms": round(&t["p95"], 1), "error_rate": round(&t["error_rate"], 4)},
            "p95_change": round(&json!(change), 3), "enough_data": enough});
        if enough && change > threshold && tp - bp > 10.0 {
            regressions.push(row.clone());
        } else if enough && change < -threshold && bp - tp > 10.0 {
            improvements.push(row.clone());
        }
        table.push(row);
    }
    let by_change = |a: &Value, b: &Value| num(&b["p95_change"]).partial_cmp(&num(&a["p95_change"])).unwrap_or(std::cmp::Ordering::Equal);
    regressions.sort_by(by_change);
    improvements.sort_by(|a, b| by_change(b, a));
    let mut vit: BTreeMap<String, (f64, f64, f64, f64)> = BTreeMap::new();
    for r in &vitals {
        let e = vit.entry(r["metric"].as_str().unwrap_or("").to_string()).or_default();
        if r["release"] == base {
            e.0 = num(&r["p75"]);
            e.2 = num(&r["count"]);
        } else {
            e.1 = num(&r["p75"]);
            e.3 = num(&r["count"]);
        }
    }
    let vitals_cmp: Vec<Value> = vit
        .into_iter()
        .filter(|(_, (_, _, bc, tc))| *bc > 0.0 && *tc > 0.0)
        .map(|(m, (b, t, bc, tc))| {
            let change = if b > 0.0 { (t - b) / b } else { 0.0 };
            let regressed = bc >= min_count as f64 && tc >= min_count as f64 && change > threshold && rating(&m, t) != "good";
            json!({"metric": m, "base_p75": round(&json!(b), 3), "target_p75": round(&json!(t), 3), "change": round(&json!(change), 3), "target_rating": rating(&m, t), "regressed": regressed})
        })
        .collect();
    let vital_regressions = vitals_cmp.iter().filter(|v| v["regressed"] == true).count();
    Ok(json!({
        "project": proj.slug, "base": base, "target": target, "min_count": min_count, "threshold": threshold,
        "regressions": regressions, "improvements": improvements, "transactions": table, "web_vitals": vitals_cmp,
        "regressed": !regressions.is_empty() || vital_regressions > 0,
        "hint": "A regression is a p95 increase above `threshold` (and more than 10 ms) with at least `min_count` samples in both releases.",
    }))
}

/// The newest two releases with spans, newest first.
pub async fn latest_two_releases(state: &AppState, project_id: &str, min_count: i64) -> anyhow::Result<Option<(String, String)>> {
    let rows = state
        .ch
        .query(
            "SELECT release, min(start) AS first_start FROM spans WHERE project_id = {p:String} AND release != '' AND start > now64(3) - INTERVAL 30 DAY \
             GROUP BY release HAVING count() >= {m:UInt32} ORDER BY first_start DESC LIMIT 2",
            &[("p", project_id.to_string()), ("m", min_count.max(1).to_string())],
        )
        .await?;
    if rows.len() < 2 {
        return Ok(None);
    }
    Ok(Some((rows[0]["release"].as_str().unwrap_or("").to_string(), rows[1]["release"].as_str().unwrap_or("").to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_names() {
        assert_eq!(normalize_http_name("GET /users/123/orders/9f86d081884c7d659a2feaa0c55ad015?x=1"), "GET /users/:id/orders/:id");
        assert_eq!(normalize_http_name("POST https://api.example.com/v1/carts/550e8400-e29b-41d4-a716-446655440000"), "POST /v1/carts/:id");
        assert_eq!(normalize_http_name("/pricing"), "/pricing");
        assert_eq!(normalize_http_name("checkout"), "checkout");
    }

    #[test]
    fn rows_and_ratings() {
        let proj = ProjectInfo { id: "p".into(), slug: "s".into(), name: "n".into(), platform: "javascript".into(), retention_days: 30, scrub_enabled: true, scrub_fields: String::new() };
        let item: SpanItem = serde_json::from_value(json!({"trace_id": "ABC", "name": "GET /a/42", "op": "http.server", "start": 1000.0, "end": 1000.25, "http": {"status_code": 503}})).unwrap();
        let r = span_row(&proj, &item, 0).unwrap();
        assert_eq!((r.trace_id.as_str(), r.name.as_str(), r.transaction.as_str(), r.status.as_str(), r.is_root), ("abc", "GET /a/:id", "GET /a/:id", "error", 1));
        assert!((r.duration_ms - 250.0).abs() < 0.01, "{}", r.duration_ms);
        let child: SpanItem = serde_json::from_value(json!({"trace_id": "abc", "parent_span_id": "x", "name": "SELECT", "op": "db", "duration_ms": 5})).unwrap();
        assert_eq!(span_row(&proj, &child, 7).unwrap().transaction, "");
        assert!(span_row(&proj, &serde_json::from_value(json!({"trace_id": "", "name": "x"})).unwrap(), 0).is_err());
        assert_eq!(rating("lcp", 2000.0), "good");
        assert_eq!(rating("lcp", 3000.0), "needs-improvement");
        assert_eq!(rating("cls", 0.3), "poor");
        assert!(vital_sql().contains("transform(k, ['lcp', 'inp', 'cls', 'fcp', 'ttfb']"));
    }
}
