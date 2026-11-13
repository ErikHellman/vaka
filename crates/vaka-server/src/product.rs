//! Product analytics on top of the `events` table: funnels, retention, paths, the event
//! catalog, and identified users with their timelines. Shared by the REST API and MCP.

use crate::auth::{ApiError, Principal};
use crate::db::RowExt;
use crate::query::{analytics_where, filter_cond, project_for, AnalyticsParams, QResult};
use crate::state::AppState;
use crate::util::{iso_time, now_ms, parse_time_expr};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const MAX_STEPS: usize = 10;

/// One funnel step: an event name (`pageview`, `screen_view` or a custom event name) and
/// optional filters on any analytics dimension, e.g. `{"path": "/pricing"}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Step {
    pub event: String,
    #[serde(default)]
    pub filters: BTreeMap<String, String>,
}

impl Step {
    /// Parse the query-string form `event|dimension:value|dimension:value`.
    pub fn parse(s: &str) -> QResult<Step> {
        let mut parts = s.split('|');
        let event = parts.next().unwrap_or("").trim().to_string();
        if event.is_empty() {
            return Err(ApiError::bad("a step needs an event name, e.g. `step=pageview|path:/pricing` or `step=signup`"));
        }
        let mut filters = BTreeMap::new();
        for f in parts {
            let bad = || ApiError::bad(format!("step filter `{f}` must look like dimension:value (or prop:<key>:value)"));
            // `prop:<key>:value` names a custom property; otherwise the first colon separates the
            // dimension from the value (values such as URLs may contain colons).
            let (d, v) = match f.trim_start().strip_prefix("prop:") {
                Some(rest) => rest.split_once(':').map(|(k, v)| (format!("prop:{k}"), v)).ok_or_else(bad)?,
                None => f.split_once(':').map(|(d, v)| (d.to_string(), v)).ok_or_else(bad)?,
            };
            filters.insert(d.trim().to_string(), v.to_string());
        }
        Ok(Step { event, filters })
    }

    fn label(&self) -> String {
        if self.filters.is_empty() {
            return self.event.clone();
        }
        let f: Vec<String> = self.filters.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{} ({})", self.event, f.join(", "))
    }
}

/// Owned query parameters (step parameters have generated names).
struct Params(Vec<(String, String)>);

impl Params {
    fn from_where(w: &[(&'static str, String)]) -> Self {
        Params(w.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }
    fn push(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.0.push((k.into(), v.into()));
    }
    fn refs(&self) -> Vec<(&str, String)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.clone())).collect()
    }
}

/// SQL condition for a step, adding its parameters as `s{i}`, `s{i}f{j}`.
fn step_cond(i: usize, step: &Step, params: &mut Params) -> QResult<String> {
    let mut conds = vec![format!("name = {{s{i}:String}}")];
    params.push(format!("s{i}"), step.event.clone());
    for (j, (dim, val)) in step.filters.iter().enumerate() {
        if j >= 8 {
            return Err(ApiError::bad("at most 8 filters per step"));
        }
        let name = format!("s{i}f{j}");
        let (cond, value) = filter_cond(dim, val, &name)?;
        conds.push(cond);
        params.push(name, value);
    }
    Ok(format!("({})", conds.join(" AND ")))
}

/// Who counts as "one person". `visitor` (default for funnels and paths) is the cookieless
/// visitor id: stable per install on mobile, rotating daily on the web. `user` only counts
/// identified events. `auto` uses the user id when present, else the visitor id.
fn actor_expr(identity: Option<&str>, default: &str) -> QResult<(&'static str, &'static str)> {
    Ok(match identity.unwrap_or(default) {
        "visitor" => ("visitor_id", "1"),
        "user" => ("user_id", "user_id != ''"),
        "auto" => ("if(user_id != '', user_id, visitor_id)", "1"),
        other => return Err(ApiError::bad(format!("identity must be visitor, user or auto, not `{other}`"))),
    })
}

fn parse_window(s: Option<&str>, default_secs: i64) -> QResult<i64> {
    let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) else { return Ok(default_secs) };
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num.parse().map_err(|_| ApiError::bad(format!("bad duration `{s}`; use e.g. 30m, 1h, 7d")))?;
    let secs = match unit {
        "s" => n,
        "m" | "" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return Err(ApiError::bad(format!("bad duration `{s}`; use e.g. 30m, 1h, 7d"))),
    };
    if !(1..=90 * 86_400).contains(&secs) {
        return Err(ApiError::bad("conversion window must be between 1s and 90d"));
    }
    Ok(secs)
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct FunnelParams {
    #[serde(flatten)]
    pub base: AnalyticsParams,
    pub steps: Vec<Step>,
    /// Max time from the first to the last step, e.g. "30m", "1d" (default 1d).
    pub window: Option<String>,
    pub identity: Option<String>,
    /// Optional dimension to break the funnel down by (each person's first non-empty value among matching events).
    pub breakdown: Option<String>,
    pub limit: Option<i64>,
}

pub async fn funnel(state: &AppState, p: &Principal, f: FunnelParams) -> QResult<Value> {
    if f.steps.len() < 2 || f.steps.len() > MAX_STEPS {
        return Err(ApiError::bad(format!("a funnel needs 2 to {MAX_STEPS} steps")));
    }
    let project = project_for(state, p, &f.base.project).await?;
    let w = analytics_where(&project.id, &f.base)?;
    let window = parse_window(f.window.as_deref(), 86_400)?;
    let (actor, actor_ok) = actor_expr(f.identity.as_deref(), "visitor")?;
    let mut params = Params::from_where(&w.params);
    let conds: Vec<String> = f.steps.iter().enumerate().map(|(i, s)| step_cond(i, s, &mut params)).collect::<QResult<_>>()?;
    params.push("win", window.to_string());
    let breakdown = match f.breakdown.as_deref().filter(|b| !b.is_empty()) {
        Some(d) => Some(crate::query::dimension_expr(d).ok_or_else(|| ApiError::bad(format!("unknown breakdown dimension `{d}`")))?),
        None => None,
    };
    // Breakdown value = the person's first non-empty value of the dimension among matching events.
    let bd_select = breakdown.as_ref().map(|e| format!(", argMinIf({e}, timestamp, {e} != '') AS bd")).unwrap_or_default();
    let inner = format!(
        "SELECT {actor} AS actor, windowFunnel({{win:UInt64}})(toDateTime(timestamp), {}) AS level{bd_select} \
         FROM events WHERE {} AND {actor_ok} AND ({}) GROUP BY actor",
        conds.join(", "),
        w.sql,
        conds.join(" OR ")
    );
    let counts: Vec<String> = (1..=f.steps.len()).map(|k| format!("countIf(level >= {k}) AS step_{k}")).collect();
    let totals = state.ch.query(&format!("SELECT {} FROM ({inner})", counts.join(", ")), &params.refs()).await?;
    let row = totals.first().cloned().unwrap_or_default();
    let shape = |row: &Value| -> Vec<Value> {
        let first = row["step_1"].as_f64().unwrap_or(0.0);
        let mut prev = first;
        f.steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let n = row[format!("step_{}", i + 1)].as_f64().unwrap_or(0.0);
                let pct = |a: f64, b: f64| if b > 0.0 { (a / b * 1000.0).round() / 10.0 } else { 0.0 };
                let v = json!({
                    "step": i + 1,
                    "label": s.label(),
                    "count": n as i64,
                    "conversion_from_start_pct": pct(n, first),
                    "conversion_from_previous_pct": if i == 0 { 100.0 } else { pct(n, prev) },
                    "dropped_from_previous": if i == 0 { 0 } else { (prev - n) as i64 },
                });
                prev = n;
                v
            })
            .collect()
    };
    let mut out = json!({
        "project": project.slug,
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "conversion_window_seconds": window,
        "identity": f.identity.as_deref().unwrap_or("visitor"),
        "filters": f.base.filters,
        "steps": shape(&row),
        "how_to_read": "count = people who did this step after all earlier steps, in order, within the conversion window of their first step.",
    });
    if breakdown.is_some() {
        params.push("lim", f.limit.unwrap_or(10).clamp(1, 100).to_string());
        let rows = state
            .ch
            .query(&format!("SELECT bd AS value, {} FROM ({inner}) GROUP BY value ORDER BY step_1 DESC LIMIT {{lim:UInt32}}", counts.join(", ")), &params.refs())
            .await?;
        out["breakdown"] = json!({
            "dimension": f.breakdown,
            "rows": rows.iter().map(|r| json!({"value": if r["value"] == "" { json!("(none)") } else { r["value"].clone() }, "steps": shape(r)})).collect::<Vec<_>>(),
        });
    }
    Ok(out)
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct RetentionParams {
    #[serde(flatten)]
    pub base: AnalyticsParams,
    /// Event that puts a person in a cohort (first time they did it). Default: any event.
    pub start: Option<Step>,
    /// Event that counts as coming back. Default: any event.
    #[serde(rename = "return")]
    pub return_step: Option<Step>,
    /// day or week (default week)
    pub period: Option<String>,
    /// Number of periods to report after the cohort period (default 8, max 30).
    pub periods: Option<i64>,
    pub identity: Option<String>,
}

pub async fn retention(state: &AppState, p: &Principal, r: RetentionParams) -> QResult<Value> {
    let project = project_for(state, p, &r.base.project).await?;
    let (period, bucket, default_span) = match r.period.as_deref().unwrap_or("week") {
        "day" => ("day", "toStartOfDay", 14 * 86_400_000i64),
        "week" => ("week", "toMonday", 8 * 7 * 86_400_000),
        other => return Err(ApiError::bad(format!("period must be day or week, not `{other}`"))),
    };
    let n_periods = r.periods.unwrap_or(if period == "day" { 14 } else { 8 }).clamp(1, 30);
    let mut base = r.base.clone();
    if base.since.is_none() {
        base.since = Some((now_ms() - default_span).to_string());
    }
    let w = analytics_where(&project.id, &base)?;
    let (actor, actor_ok) = actor_expr(r.identity.as_deref(), "auto")?;
    let mut params = Params::from_where(&w.params);
    let start_cond = match &r.start {
        Some(s) => step_cond(0, s, &mut params)?,
        None => "1".into(),
    };
    let return_cond = match &r.return_step {
        Some(s) => step_cond(1, s, &mut params)?,
        None => "1".into(),
    };
    params.push("np", n_periods.to_string());
    // Cohort = the period of a person's first start event ever (as far as retention allows),
    // limited to cohorts that begin inside the requested window.
    let sql = format!(
        "WITH cohorts AS ( \
            SELECT {actor} AS actor, {bucket}(min(timestamp)) AS cohort FROM events \
            WHERE project_id = {{p:String}} AND {actor_ok} AND {start_cond} GROUP BY actor \
            HAVING cohort >= {bucket}(fromUnixTimestamp64Milli({{since:Int64}})) AND cohort <= fromUnixTimestamp64Milli({{until:Int64}}) \
         ), activity AS ( \
            SELECT DISTINCT {actor} AS actor, {bucket}(timestamp) AS period FROM events \
            WHERE {} AND {actor_ok} AND {return_cond} \
         ) \
         SELECT formatDateTime(c.cohort, '%Y-%m-%d') AS cohort, \
                dateDiff('{period}', c.cohort, a.period) AS n, uniqExact(c.actor) AS people \
         FROM cohorts c INNER JOIN activity a ON a.actor = c.actor \
         WHERE a.period >= c.cohort AND n <= {{np:UInt32}} \
         GROUP BY cohort, n ORDER BY cohort, n",
        w.sql
    );
    let rows = state.ch.query(&sql, &params.refs()).await?;
    let sizes_sql = format!(
        "SELECT formatDateTime(cohort, '%Y-%m-%d') AS cohort, count() AS size FROM ( \
            SELECT {actor} AS actor, {bucket}(min(timestamp)) AS cohort FROM events \
            WHERE project_id = {{p:String}} AND {actor_ok} AND {start_cond} GROUP BY actor \
            HAVING cohort >= {bucket}(fromUnixTimestamp64Milli({{since:Int64}})) AND cohort <= fromUnixTimestamp64Milli({{until:Int64}}) \
         ) GROUP BY cohort ORDER BY cohort"
    );
    let sizes = state.ch.query(&sizes_sql, &params.refs()).await?;
    let mut cohorts = Vec::new();
    for s in &sizes {
        let name = s["cohort"].as_str().unwrap_or("").to_string();
        let size = s["size"].as_f64().unwrap_or(0.0);
        let mut retained = vec![Value::Null; n_periods as usize + 1];
        for r in rows.iter().filter(|r| r["cohort"].as_str() == Some(&name)) {
            let n = r["n"].as_i64().unwrap_or(-1);
            if (0..=n_periods).contains(&n) {
                let people = r["people"].as_f64().unwrap_or(0.0);
                retained[n as usize] = json!({"people": people as i64, "pct": if size > 0.0 { (people / size * 1000.0).round() / 10.0 } else { 0.0 }});
            }
        }
        // Periods that have not happened yet stay null; past periods with nobody back are 0.
        let cohort_start = crate::util::parse_timestamp(&json!(format!("{name}T00:00:00Z"))).unwrap_or(0);
        let period_ms = if period == "day" { 86_400_000 } else { 7 * 86_400_000 };
        for (i, v) in retained.iter_mut().enumerate() {
            if v.is_null() && cohort_start + i as i64 * period_ms <= now_ms() {
                *v = json!({"people": 0, "pct": 0.0});
            }
        }
        cohorts.push(json!({"cohort": name, "size": size as i64, "retained": retained}));
    }
    let mut notes = vec!["retained[n] = people from the cohort who did the return event n periods after their first start event; retained[0] is the cohort period itself. null = that period has not happened yet."];
    if project.platform == "javascript" && r.identity.as_deref() != Some("user") {
        notes.push("Anonymous web visitors get a new id every day (cookieless), so web retention beyond the first day only counts identified users. Use identity=user after calling identify() in the SDK.");
    }
    Ok(json!({
        "project": project.slug,
        "period": period,
        "identity": r.identity.as_deref().unwrap_or("auto"),
        "start": r.start.as_ref().map(|s| s.label()).unwrap_or_else(|| "any event".into()),
        "return": r.return_step.as_ref().map(|s| s.label()).unwrap_or_else(|| "any event".into()),
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "cohorts": cohorts,
        "notes": notes,
    }))
}

/// Label used for paths: `pageview /pricing`, `screen Checkout`, or the custom event name.
pub const LABEL: &str = "multiIf(type = 'pageview', concat('pageview ', path), type = 'screen_view', concat('screen ', props['screen']), name)";

#[derive(Debug, Default, Clone, Deserialize)]
pub struct PathParams {
    #[serde(flatten)]
    pub base: AnalyticsParams,
    /// Where paths start (or end, for direction=before): a label such as `pageview /pricing`,
    /// `screen Checkout` or `signup`. Omit to get the most common paths from any entry point.
    pub from: Option<String>,
    /// after (default) or before
    pub direction: Option<String>,
    /// Steps to follow (default 3, max 6).
    pub depth: Option<i64>,
    pub identity: Option<String>,
    pub limit: Option<i64>,
}

pub async fn paths(state: &AppState, p: &Principal, a: PathParams) -> QResult<Value> {
    let project = project_for(state, p, &a.base.project).await?;
    let w = analytics_where(&project.id, &a.base)?;
    let (actor, actor_ok) = actor_expr(a.identity.as_deref(), "visitor")?;
    let depth = a.depth.unwrap_or(3).clamp(1, 6);
    let before = match a.direction.as_deref().unwrap_or("after") {
        "after" => false,
        "before" => true,
        other => return Err(ApiError::bad(format!("direction must be after or before, not `{other}`"))),
    };
    let mut params = Params::from_where(&w.params);
    params.push("depth", depth.to_string());
    params.push("lim", a.limit.unwrap_or(20).clamp(1, 200).to_string());
    // Each visit's events in time order (first 1000), with consecutive repeats collapsed. A visit
    // is the SDK session when there is one, otherwise the person's calendar day.
    let seqs = format!(
        "SELECT actor, arrayFilter((x, i) -> i = 1 OR x != raw[i - 1], raw, arrayEnumerate(raw)) AS labels FROM ( \
            SELECT {actor} AS actor, if(session_id != '', session_id, toString(toDate(timestamp))) AS visit, \
                   arraySlice(arrayMap(x -> x.2, arraySort(x -> x.1, groupArray((timestamp, {LABEL})))), 1, 1000) AS raw \
            FROM events WHERE {} AND {actor_ok} GROUP BY actor, visit)",
        w.sql
    );
    let sql = match &a.from {
        Some(from) => {
            params.push("from", from.clone());
            let slice = if before {
                "arraySlice(labels, greatest(1, toInt64(i) - {depth:Int64}), least(toInt64(i), {depth:Int64} + 1))"
            } else {
                "arraySlice(labels, i, {depth:Int64} + 1)"
            };
            format!(
                "SELECT seq, count() AS occurrences, uniqExact(actor) AS people FROM ( \
                    SELECT actor, {slice} AS seq FROM ({seqs}) \
                    ARRAY JOIN arrayFilter(j -> labels[j] = {{from:String}}, arrayEnumerate(labels)) AS i) \
                 GROUP BY seq ORDER BY people DESC, occurrences DESC LIMIT {{lim:UInt32}}"
            )
        }
        None => format!(
            "SELECT arraySlice(labels, 1, {{depth:Int64}} + 1) AS seq, count() AS occurrences, uniqExact(actor) AS people FROM ({seqs}) \
             GROUP BY seq ORDER BY people DESC LIMIT {{lim:UInt32}}"
        ),
    };
    let rows = state.ch.query(&sql, &params.refs()).await?;
    let total_from = if let Some(_from) = &a.from {
        let q = format!("SELECT uniqExact(actor) AS people FROM ({seqs}) WHERE has(labels, {{from:String}})");
        state.ch.query(&q, &params.refs()).await?.first().and_then(|r| r["people"].as_i64()).unwrap_or(0)
    } else {
        0
    };
    let paths: Vec<Value> = rows
        .iter()
        .map(|r| {
            let seq: Vec<String> = r["seq"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
            let mut shown = seq.clone();
            if !before && (seq.len() as i64) < depth + 1 {
                shown.push("(end)".into());
            }
            json!({"path": shown.join(" → "), "steps": seq, "people": r["people"], "occurrences": r["occurrences"]})
        })
        .collect();
    let mut out = json!({
        "project": project.slug,
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "direction": if before { "before" } else { "after" },
        "from": a.from,
        "depth": depth,
        "paths": paths,
        "labels": "pageview <path>, screen <name>, or the custom event name; consecutive repeats are collapsed; paths stay within one visit (SDK session, or calendar day); (end) = the visit ended",
    });
    if a.from.is_some() {
        out["people_with_from"] = json!(total_from);
    }
    Ok(out)
}

/// The event catalog: which event names exist, how often, and which property keys they carry.
pub async fn event_catalog(state: &AppState, p: &Principal, a: AnalyticsParams, limit: Option<i64>) -> QResult<Value> {
    let project = project_for(state, p, &a.project).await?;
    let w = analytics_where(&project.id, &a)?;
    let mut params = Params::from_where(&w.params);
    params.push("lim", limit.unwrap_or(100).clamp(1, 1000).to_string());
    let rows = state
        .ch
        .query(
            &format!(
                "SELECT name, any(type) AS type, count() AS events, uniq(visitor_id) AS visitors, \
                 formatDateTime(max(timestamp), '%Y-%m-%dT%H:%i:%SZ') AS last_seen, \
                 arraySlice(arraySort(groupUniqArrayArray(100)(mapKeys(props))), 1, 50) AS prop_keys \
                 FROM events WHERE {} GROUP BY name ORDER BY events DESC LIMIT {{lim:UInt32}}",
                w.sql
            ),
            &params.refs(),
        )
        .await?;
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "events": rows,
        "hint": "Use names as funnel/retention steps (`event`) and prop keys as dimensions `prop:<key>` in breakdowns and filters.",
    }))
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct UserListParams {
    pub project: String,
    pub since: Option<String>,
    /// Substring of the user id.
    pub query: Option<String>,
    pub limit: Option<i64>,
}

/// Identified users (those the app passed to `identify`) with activity and error counts.
pub async fn list_users(state: &AppState, p: &Principal, u: UserListParams) -> QResult<Value> {
    let project = project_for(state, p, &u.project).await?;
    let now = now_ms();
    let since = u.since.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 30 * 86_400_000);
    let params = vec![
        ("p", project.id.clone()),
        ("since", since.to_string()),
        ("q", u.query.clone().unwrap_or_default()),
        ("lim", u.limit.unwrap_or(50).clamp(1, 500).to_string()),
    ];
    let sql = "SELECT user_id, sum(events) AS events, sum(errors) AS errors, \
        formatDateTime(min(first), '%Y-%m-%dT%H:%i:%SZ') AS first_seen, formatDateTime(max(last), '%Y-%m-%dT%H:%i:%SZ') AS last_seen, \
        argMax(release, last) AS last_release, argMax(platform, last) AS platform FROM ( \
          SELECT user_id, count() AS events, 0 AS errors, min(timestamp) AS first, max(timestamp) AS last, argMax(release, timestamp) AS release, argMax(platform, timestamp) AS platform \
          FROM events WHERE project_id = {p:String} AND user_id != '' AND timestamp >= fromUnixTimestamp64Milli({since:Int64}) GROUP BY user_id \
          UNION ALL \
          SELECT user_id, 0, count(), min(timestamp), max(timestamp), argMax(release, timestamp), argMax(platform, timestamp) \
          FROM errors WHERE project_id = {p:String} AND user_id != '' AND timestamp >= fromUnixTimestamp64Milli({since:Int64}) GROUP BY user_id \
        ) WHERE positionCaseInsensitive(user_id, {q:String}) > 0 OR {q:String} = '' \
        GROUP BY user_id ORDER BY max(last) DESC LIMIT {lim:UInt32}";
    let rows = state.ch.query(sql, &params).await?;
    Ok(json!({"project": project.slug, "since": iso_time(since), "users": rows,
        "note": "Only users the app identified with identify(userId) appear here; anonymous visitors are never listed."}))
}

/// Everything one identified user did, newest first: analytics events, errors and sessions.
pub async fn user_timeline(state: &AppState, p: &Principal, project: &str, user_id: &str, since: Option<&str>, limit: Option<i64>) -> QResult<Value> {
    let project = project_for(state, p, project).await?;
    if user_id.is_empty() {
        return Err(ApiError::bad("user id is required"));
    }
    let now = now_ms();
    let since = since.and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 7 * 86_400_000);
    let params = vec![
        ("p", project.id.clone()),
        ("u", user_id.to_string()),
        ("since", since.to_string()),
        ("lim", limit.unwrap_or(200).clamp(1, 2000).to_string()),
    ];
    let sql = format!(
        "SELECT * FROM ( \
          SELECT 'event' AS kind, formatDateTime(timestamp, '%Y-%m-%dT%H:%i:%S.%fZ') AS ts, timestamp AS t, {LABEL} AS what, \
                 release, session_id, toJSONString(props) AS detail, '' AS issue_id \
          FROM events WHERE project_id = {{p:String}} AND user_id = {{u:String}} AND timestamp >= fromUnixTimestamp64Milli({{since:Int64}}) \
          UNION ALL \
          SELECT 'error', formatDateTime(timestamp, '%Y-%m-%dT%H:%i:%S.%fZ'), timestamp, \
                 concat(if(fatal = 1, 'crash: ', 'error: '), exception_type, if(exception_value != '', concat(': ', exception_value), message)), \
                 release, session_id, culprit, issue_id \
          FROM errors WHERE project_id = {{p:String}} AND user_id = {{u:String}} AND timestamp >= fromUnixTimestamp64Milli({{since:Int64}}) \
          UNION ALL \
          SELECT 'session', formatDateTime(s_started, '%Y-%m-%dT%H:%i:%S.%fZ'), s_started, concat('session ', s_status), s_release, session_id, \
                 concat('duration_ms=', toString(s_duration)), '' \
          FROM (SELECT session_id, argMax(status, timestamp) AS s_status, argMax(release, timestamp) AS s_release, \
                       argMax(duration_ms, timestamp) AS s_duration, min(started) AS s_started \
                FROM sessions WHERE project_id = {{p:String}} AND user_id = {{u:String}} AND started >= fromUnixTimestamp64Milli({{since:Int64}}) GROUP BY session_id) \
        ) ORDER BY t DESC LIMIT {{lim:UInt32}}"
    );
    let rows = state.ch.query(&sql, &params).await?;
    let mut issue_ids: Vec<String> = rows.iter().filter_map(|r| r["issue_id"].as_str().filter(|s| !s.is_empty()).map(String::from)).collect();
    issue_ids.sort();
    issue_ids.dedup();
    let mut short: BTreeMap<String, Value> = BTreeMap::new();
    for id in issue_ids.iter().take(50) {
        if let Some(i) = crate::query::get_issue_row(state, id).await? {
            short.insert(id.clone(), i["short_id"].clone());
        }
    }
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = json!({"kind": r["kind"], "timestamp": r["ts"], "what": r["what"], "release": r["release"], "session_id": r["session_id"]});
            match r["kind"].as_str() {
                Some("event") => {
                    if let Ok(props) = serde_json::from_str::<Value>(r["detail"].as_str().unwrap_or("{}")) {
                        if props.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                            v["props"] = props;
                        }
                    }
                }
                Some("error") => {
                    v["culprit"] = r["detail"].clone();
                    if let Some(s) = short.get(r["issue_id"].as_str().unwrap_or("")) {
                        v["issue"] = s.clone();
                    }
                }
                _ => v["detail"] = r["detail"].clone(),
            }
            v
        })
        .collect();
    Ok(json!({"project": project.slug, "user_id": user_id, "since": iso_time(since), "timeline": items,
        "hint": "Newest first. To see what led to a crash, read the entries just below (older than) the error, and open the issue with get_issue."}))
}

// ---------------------------------------------------------------------------
// Goals and revenue
// ---------------------------------------------------------------------------

fn prop_key(k: &str) -> QResult<String> {
    if k.is_empty() || k.len() > 128 || !k.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) {
        return Err(ApiError::bad(format!("bad property name `{k}`")));
    }
    Ok(k.to_string())
}

/// Revenue per currency (and optionally per dimension value) from a numeric event property.
async fn revenue_rows(
    state: &AppState,
    w: &crate::query::Where,
    step: &Step,
    value_prop: &str,
    currency_prop: &str,
    breakdown: Option<&str>,
    limit: i64,
) -> QResult<Vec<Value>> {
    let mut params = Params::from_where(&w.params);
    let cond = step_cond(0, step, &mut params)?;
    params.push("vp", prop_key(value_prop)?);
    params.push("cp", prop_key(currency_prop)?);
    params.push("lim", limit.clamp(1, 100).to_string());
    let (dim_sel, dim_group) = match breakdown.filter(|b| !b.is_empty()) {
        Some(b) => {
            let expr = crate::query::dimension_expr(b).ok_or_else(|| ApiError::bad(format!("unknown breakdown dimension `{b}`")))?;
            (format!(", {expr} AS value"), ", value")
        }
        None => (String::new(), ""),
    };
    Ok(state
        .ch
        .query(
            &format!(
                "SELECT upper(props[{{cp:String}}]) AS currency{dim_sel}, round(sum(toFloat64OrZero(props[{{vp:String}}])), 2) AS revenue, \
                 count() AS conversions, uniq(if(user_id != '', user_id, visitor_id)) AS customers, round(revenue / greatest(conversions, 1), 2) AS average \
                 FROM events WHERE {} AND {cond} GROUP BY currency{dim_group} ORDER BY revenue DESC LIMIT {{lim:UInt32}}",
                w.sql
            ),
            &params.refs(),
        )
        .await?)
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct RevenueParams {
    #[serde(flatten)]
    pub base: AnalyticsParams,
    /// Step syntax, default `purchase`.
    pub event: Option<String>,
    /// Numeric property holding the amount, default `revenue`.
    pub value_prop: Option<String>,
    /// Property holding the ISO currency code, default `currency`.
    pub currency_prop: Option<String>,
    pub breakdown: Option<String>,
    pub limit: Option<i64>,
}

pub async fn revenue(state: &AppState, p: &Principal, r: RevenueParams) -> QResult<Value> {
    let project = project_for(state, p, &r.base.project).await?;
    let w = analytics_where(&project.id, &r.base)?;
    let step = Step::parse(r.event.as_deref().unwrap_or("purchase"))?;
    let vp = r.value_prop.as_deref().unwrap_or("revenue");
    let cp = r.currency_prop.as_deref().unwrap_or("currency");
    let totals = revenue_rows(state, &w, &step, vp, cp, None, 20).await?;
    let breakdown = match r.breakdown.as_deref().filter(|b| !b.is_empty()) {
        Some(b) => Some(revenue_rows(state, &w, &step, vp, cp, Some(b), r.limit.unwrap_or(10)).await?),
        None => None,
    };
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "event": step.label(),
        "value_prop": vp,
        "currency_prop": cp,
        "totals": totals,
        "breakdown_by": r.breakdown,
        "breakdown": breakdown,
        "note": "Amounts are summed per currency as sent (no conversion). An empty currency means the property was missing.",
    }))
}

pub async fn list_goals(state: &AppState, p: &Principal, a: AnalyticsParams) -> QResult<Value> {
    let project = project_for(state, p, &a.project).await?;
    let w = analytics_where(&project.id, &a)?;
    let rows = sqlx::query("SELECT id, name, step, value_prop, created_at, created_by FROM goals WHERE project_id = $1 ORDER BY name LIMIT 100")
        .bind(&project.id)
        .fetch_all(&state.db)
        .await?;
    let mut params = Params::from_where(&w.params);
    let mut sel = vec!["uniq(visitor_id) AS visitors".to_string()];
    let mut steps = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let step = Step::parse(&r.s("step"))?;
        let cond = step_cond(i, &step, &mut params)?;
        sel.push(format!("countIf({cond}) AS c{i}, uniqIf(visitor_id, {cond}) AS u{i}"));
        steps.push(step);
    }
    let stats = state.ch.query(&format!("SELECT {} FROM events WHERE {}", sel.join(", "), w.sql), &params.refs()).await?;
    let stats = stats.into_iter().next().unwrap_or(json!({}));
    let visitors = stats["visitors"].as_f64().or_else(|| stats["visitors"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
    let mut goals = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let n = |k: String| stats[&k].as_f64().or_else(|| stats[&k].as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
        let converters = n(format!("u{i}"));
        let vp = r.s("value_prop");
        let revenue = if vp.is_empty() { None } else { Some(revenue_rows(state, &w, &steps[i], &vp, "currency", None, 20).await?) };
        goals.push(json!({
            "id": r.s("id"), "name": r.s("name"), "step": r.s("step"), "value_prop": if vp.is_empty() { None } else { Some(vp) },
            "conversions": n(format!("c{i}")) as i64, "converters": converters as i64,
            "conversion_rate": if visitors > 0.0 { (converters / visitors * 10_000.0).round() / 10_000.0 } else { 0.0 },
            "revenue": revenue,
            "created_at": iso_time(r.i("created_at")), "created_by": r.s("created_by"),
        }));
    }
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "visitors": visitors as i64,
        "note": "conversion_rate = unique visitors who converted / unique visitors in the window.",
        "goals": goals,
    }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoalIn {
    pub name: String,
    /// Step syntax: `signup`, `pageview|path:/thanks`, `purchase|prop:plan:pro`.
    pub step: String,
    /// Optional numeric property to sum as revenue (with the `currency` property).
    #[serde(default)]
    pub value_prop: Option<String>,
}

pub async fn create_goal(state: &AppState, p: &Principal, project: &str, g: GoalIn) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let name = g.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(ApiError::bad("goal name must be 1 to 100 characters"));
    }
    Step::parse(&g.step)?;
    let vp = match g.value_prop.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => prop_key(v)?,
        None => String::new(),
    };
    let id = crate::util::new_id();
    let res = sqlx::query(
        "INSERT INTO goals (id, project_id, name, step, value_prop, created_at, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (project_id, name) DO NOTHING",
    )
    .bind(&id)
    .bind(&proj.id)
    .bind(name)
    .bind(g.step.trim())
    .bind(&vp)
    .bind(now_ms())
    .bind(p.label())
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::bad(format!("a goal named `{name}` already exists")));
    }
    Ok(json!({"id": id, "project": proj.slug, "name": name, "step": g.step.trim(), "value_prop": if vp.is_empty() { None } else { Some(vp) }}))
}

pub async fn delete_goal(state: &AppState, p: &Principal, project: &str, goal: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let res = sqlx::query("DELETE FROM goals WHERE project_id = $1 AND (id = $2 OR name = $2)").bind(&proj.id).bind(goal).execute(&state.db).await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("goal"));
    }
    Ok(json!({"deleted": goal}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steps() {
        let s = Step::parse("pageview|path:/pricing|country:SE").unwrap();
        assert_eq!(s.event, "pageview");
        assert_eq!(s.filters.get("path").unwrap(), "/pricing");
        assert_eq!(s.filters.get("country").unwrap(), "SE");
        assert_eq!(Step::parse("signup").unwrap().filters.len(), 0);
        assert!(Step::parse("").is_err());
        assert!(Step::parse("pageview|nocolon").is_err());
        // Values may contain colons (URLs).
        assert_eq!(Step::parse("pageview|url:https://x.se/a").unwrap().filters["url"], "https://x.se/a");
        assert_eq!(Step::parse("purchase|prop:plan:pro").unwrap().filters["prop:plan"], "pro");
    }

    #[test]
    fn windows() {
        assert_eq!(parse_window(None, 86400).unwrap(), 86400);
        assert_eq!(parse_window(Some("30m"), 0).unwrap(), 1800);
        assert_eq!(parse_window(Some("2h"), 0).unwrap(), 7200);
        assert_eq!(parse_window(Some("7d"), 0).unwrap(), 7 * 86400);
        assert!(parse_window(Some("1y"), 0).is_err());
        assert!(parse_window(Some("200d"), 0).is_err());
    }

    #[test]
    fn step_sql_uses_parameters() {
        let mut p = Params(vec![]);
        let sql = step_cond(2, &Step::parse("pageview|path:~pric").unwrap(), &mut p).unwrap();
        assert!(sql.contains("{s2:String}") && sql.contains("{s2f0:String}"), "{sql}");
        assert!(!sql.contains("pric"));
        assert_eq!(p.0.len(), 2);
    }
}
