//! Bucketed time series shared by analytics, errors, performance and sessions.
//!
//! `interval` takes a calendar unit (`minute`, `hour`, `day`, `week`, `month`) or a fixed
//! duration (`30s` is rounded up to `1m`; `5m`, `15m`, `1h`, `6h`, `1d`, ...). Fixed buckets are
//! aligned to the unix epoch (UTC), weeks start on Monday. With `fill=true` every bucket in the
//! window is returned, with zeros (or `null` for percentiles) where nothing happened, which is
//! what graphing and alerting tools such as Grafana expect.

use crate::auth::ApiError;
use crate::query::QResult;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

/// More buckets than this is almost always a mistake (and an expensive query).
pub const MAX_BUCKETS: i64 = 2000;

const MONDAY_OFFSET_S: i64 = 4 * 86_400; // 1970-01-05 was a Monday.

#[derive(Debug, Clone, PartialEq)]
pub enum Bucketing {
    /// Fixed length in seconds (a multiple of 60).
    Fixed(i64),
    Week,
    Month,
}

impl Bucketing {
    /// The name echoed in responses: `hour`, `day`, `week`, `month`, `minute` or e.g. `5m`.
    pub fn name(&self) -> String {
        match self {
            Bucketing::Week => "week".into(),
            Bucketing::Month => "month".into(),
            Bucketing::Fixed(60) => "minute".into(),
            Bucketing::Fixed(3600) => "hour".into(),
            Bucketing::Fixed(86_400) => "day".into(),
            Bucketing::Fixed(s) if s % 86_400 == 0 => format!("{}d", s / 86_400),
            Bucketing::Fixed(s) if s % 3600 == 0 => format!("{}h", s / 3600),
            Bucketing::Fixed(s) => format!("{}m", s / 60),
        }
    }

    /// Approximate bucket length in seconds (months count as 30 days).
    pub fn seconds(&self) -> i64 {
        match self {
            Bucketing::Fixed(s) => *s,
            Bucketing::Week => 7 * 86_400,
            Bucketing::Month => 30 * 86_400,
        }
    }

    /// SQL producing the bucket start as an RFC 3339 string, for a DateTime(64) column.
    pub fn sql(&self, col: &str) -> String {
        let secs = match self {
            Bucketing::Fixed(s) => format!("intDiv(toUnixTimestamp({col}), {s}) * {s}"),
            Bucketing::Week => format!("intDiv(toUnixTimestamp({col}) - {MONDAY_OFFSET_S}, 604800) * 604800 + {MONDAY_OFFSET_S}"),
            Bucketing::Month => format!("toUnixTimestamp(toDateTime(toStartOfMonth({col}), 'UTC'))"),
        };
        format!("formatDateTime(toDateTime({secs}, 'UTC'), '%Y-%m-%dT%H:%i:%SZ')")
    }

    fn floor(&self, secs: i64) -> i64 {
        match self {
            Bucketing::Fixed(s) => secs.div_euclid(*s) * s,
            Bucketing::Week => (secs - MONDAY_OFFSET_S).div_euclid(604_800) * 604_800 + MONDAY_OFFSET_S,
            Bucketing::Month => {
                let dt = time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
                month_start(dt.year(), dt.month() as u8)
            }
        }
    }

    fn next(&self, start: i64) -> i64 {
        match self {
            Bucketing::Fixed(s) => start + s,
            Bucketing::Week => start + 604_800,
            Bucketing::Month => {
                let dt = time::OffsetDateTime::from_unix_timestamp(start).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
                let (y, m) = if dt.month() as u8 == 12 { (dt.year() + 1, 1) } else { (dt.year(), dt.month() as u8 + 1) };
                month_start(y, m)
            }
        }
    }

    /// Bucket starts (unix seconds) covering `[since_ms, until_ms]`.
    pub fn starts(&self, since_ms: i64, until_ms: i64) -> Vec<i64> {
        let mut out = Vec::new();
        let mut t = self.floor(since_ms.div_euclid(1000));
        let end = until_ms.div_euclid(1000);
        while t <= end && (out.len() as i64) < MAX_BUCKETS {
            out.push(t);
            t = self.next(t);
        }
        out
    }
}

fn month_start(year: i32, month: u8) -> i64 {
    let m = time::Month::try_from(month).unwrap_or(time::Month::January);
    time::Date::from_calendar_date(year, m, 1).map(|d| d.midnight().assume_utc().unix_timestamp()).unwrap_or(0)
}

/// RFC 3339 for a bucket start, in the same format as [`Bucketing::sql`].
pub fn bucket_label(secs: i64) -> String {
    let dt = time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// Parse a duration like `30s`, `5m`, `1h`, `1d`, `1w` into seconds.
fn duration_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().ok().filter(|n| *n > 0)?;
    let mult = match unit {
        "s" => 1,
        "m" | "min" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return None,
    };
    Some(n * mult)
}

/// Choose the bucketing for a window. `None` keeps the long-standing default: hourly up to two
/// days, daily beyond.
pub fn parse_interval(interval: Option<&str>, since_ms: i64, until_ms: i64) -> QResult<Bucketing> {
    let span_s = ((until_ms - since_ms) / 1000).max(1);
    let b = match interval.map(str::trim).filter(|s| !s.is_empty()) {
        None => {
            if span_s <= 2 * 86_400 {
                Bucketing::Fixed(3600)
            } else {
                Bucketing::Fixed(86_400)
            }
        }
        Some("minute") => Bucketing::Fixed(60),
        Some("hour") => Bucketing::Fixed(3600),
        Some("day") => Bucketing::Fixed(86_400),
        Some("week") => Bucketing::Week,
        Some("month") => Bucketing::Month,
        Some(other) => {
            let secs = duration_secs(other).ok_or_else(|| {
                ApiError::bad(format!("unknown interval `{other}`: minute, hour, day, week, month or a duration such as 5m, 1h, 1d"))
            })?;
            // Round up to whole minutes; buckets smaller than a minute are not supported.
            let secs = ((secs + 59) / 60).max(1) * 60;
            if secs == 7 * 86_400 {
                Bucketing::Week
            } else {
                Bucketing::Fixed(secs)
            }
        }
    };
    let n = span_s / b.seconds() + 1;
    if n > MAX_BUCKETS {
        return Err(ApiError::bad(format!(
            "interval `{}` gives {n} buckets for this window; the maximum is {MAX_BUCKETS}, use a larger interval",
            b.name()
        )));
    }
    Ok(b)
}

/// Rename keys in every row (SQL aliases avoid table and column names).
pub fn rename(rows: Vec<Value>, pairs: &[(&str, &str)]) -> Vec<Value> {
    rows.into_iter()
        .map(|mut r| {
            if let Some(o) = r.as_object_mut() {
                for (from, to) in pairs {
                    if let Some(v) = o.remove(*from) {
                        o.insert((*to).to_string(), v);
                    }
                }
            }
            r
        })
        .collect()
}

/// True for `fill=true` / `1` / `yes`.
pub fn truthy(v: Option<&str>) -> bool {
    matches!(v.map(|s| s.trim().to_ascii_lowercase()).as_deref(), Some("true" | "1" | "yes"))
}

/// Fill missing buckets. `rows` must carry a `bucket` string; `empty` is the row used for an
/// empty bucket (without `bucket`).
pub fn fill(rows: Vec<Value>, b: &Bucketing, since_ms: i64, until_ms: i64, empty: &Value) -> Vec<Value> {
    let mut by: HashMap<String, Value> = HashMap::new();
    for r in rows {
        if let Some(k) = r["bucket"].as_str() {
            by.insert(k.to_string(), r);
        }
    }
    b.starts(since_ms, until_ms)
        .into_iter()
        .map(|t| {
            let label = bucket_label(t);
            by.remove(&label).unwrap_or_else(|| {
                let mut e = empty.clone();
                if let Some(o) = e.as_object_mut() {
                    o.insert("bucket".into(), Value::String(label.clone()));
                }
                e
            })
        })
        .collect()
}

/// An empty row with each name set to 0 and each of `nulls` set to null.
pub fn empty_row(zeros: &[&str], nulls: &[&str]) -> Value {
    let mut m = Map::new();
    for z in zeros {
        m.insert((*z).into(), json!(0));
    }
    for n in nulls {
        m.insert((*n).into(), Value::Null);
    }
    Value::Object(m)
}

/// Split rows carrying `grp` and `bucket` into groups ordered by the sum of `rank_by`, largest first.
pub fn split_groups(rows: Vec<Value>, rank_by: &str) -> Vec<(String, f64, Vec<Value>)> {
    let mut groups: BTreeMap<String, (f64, Vec<Value>)> = BTreeMap::new();
    for mut r in rows {
        let key = match &r["grp"] {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            v => v.to_string(),
        };
        let n = r[rank_by].as_f64().or_else(|| r[rank_by].as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
        if let Some(o) = r.as_object_mut() {
            o.remove("grp");
        }
        let e = groups.entry(key).or_default();
        e.0 += n;
        e.1.push(r);
    }
    let mut out: Vec<(String, f64, Vec<Value>)> = groups.into_iter().map(|(k, (t, v))| (k, t, v)).collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Options shared by every time-series endpoint.
#[derive(Debug, Default, Clone)]
pub struct SeriesOpts {
    pub interval: Option<String>,
    pub fill: bool,
    /// Split into one series per value of this dimension (top `limit` by volume).
    pub group_by: Option<String>,
    pub limit: Option<i64>,
}

impl SeriesOpts {
    pub fn from_map(m: &std::collections::BTreeMap<String, String>) -> Self {
        SeriesOpts {
            interval: m.get("interval").cloned(),
            fill: truthy(m.get("fill").map(|s| s.as_str())),
            group_by: m.get("group_by").cloned(),
            limit: m.get("limit").and_then(|l| l.parse().ok()),
        }
    }

    pub fn from_json(a: &Value) -> Self {
        let s = |k: &str| a.get(k).and_then(|v| v.as_str()).map(str::to_string);
        SeriesOpts {
            interval: s("interval"),
            fill: a.get("fill").map(|v| v.as_bool().unwrap_or_else(|| truthy(v.as_str()))).unwrap_or(false),
            group_by: s("group_by"),
            limit: a.get("limit").and_then(|v| v.as_i64()),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors, performance and sessions over time
// ---------------------------------------------------------------------------

use crate::auth::Principal;
use crate::query::project_for;
use crate::state::AppState;
use crate::util::{iso_time, now_ms, parse_time_expr};

#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct ErrorSeriesParams {
    pub project: String,
    pub since: Option<String>,
    pub until: Option<String>,
    pub level: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    /// Issue id or short id (`WEB-12`).
    pub issue: Option<String>,
    pub exception_type: Option<String>,
    /// `true`: only handled errors, `false`: only unhandled.
    pub handled: Option<String>,
}

pub const ERROR_GROUPS: &[&str] = &["issue", "release", "level", "environment", "exception_type", "platform", "os", "browser", "country"];

fn error_group_expr(g: &str) -> Option<&'static str> {
    Some(match g {
        "issue" => "issue_id",
        "release" => "release",
        "level" => "level",
        "environment" => "environment",
        "exception_type" => "exception_type",
        "platform" => "platform",
        "os" => "os_name",
        "browser" => "browser",
        "country" => "country",
        _ => return None,
    })
}

const ERROR_METRICS: &str = "count() AS n_errors, countIf(handled = 0) AS unhandled, countIf(fatal = 1) AS fatal, \
    uniq(if(user_id != '', user_id, anon_id)) AS users, uniq(issue_id) AS issues";
const ERR_RENAME: &[(&str, &str)] = &[("n_errors", "errors")];
const ERROR_ZEROS: &[&str] = &["errors", "unhandled", "fatal", "users", "issues"];

/// Error counts per bucket: `errors`, `unhandled`, `fatal`, `users` (affected), `issues`.
pub async fn errors_timeseries(state: &AppState, p: &Principal, a: ErrorSeriesParams, opts: SeriesOpts) -> QResult<Value> {
    let project = project_for(state, p, &a.project).await?;
    let now = now_ms();
    let since = a.since.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 86_400_000);
    let until = a.until.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now);
    let b = parse_interval(opts.interval.as_deref(), since, until)?;
    let mut conds = vec![
        "project_id = {p:String}".to_string(),
        "timestamp >= fromUnixTimestamp64Milli({since:Int64})".to_string(),
        "timestamp <= fromUnixTimestamp64Milli({until:Int64})".to_string(),
    ];
    let mut ps: Vec<(&'static str, String)> = vec![("p", project.id.clone()), ("since", since.to_string()), ("until", until.to_string())];
    let mut add = |name: &'static str, col: &str, v: Option<String>| {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            conds.push(format!("{col} = {{{name}:String}}"));
            ps.push((name, v));
        }
    };
    add("level", "level", a.level.clone());
    add("rel", "release", a.release.clone());
    add("env", "environment", a.environment.clone());
    add("etype", "exception_type", a.exception_type.clone());
    if let Some(id) = a.issue.as_deref().filter(|s| !s.is_empty()) {
        let issue = crate::query::issue_for(state, p, id).await?;
        if issue["project_id"].as_str() != Some(project.id.as_str()) {
            return Err(ApiError::bad("that issue belongs to another project"));
        }
        add("issue", "issue_id", issue["id"].as_str().map(str::to_string));
    }
    match a.handled.as_deref() {
        Some("true" | "1") => conds.push("handled = 1".into()),
        Some("false" | "0") => conds.push("handled = 0".into()),
        _ => {}
    }
    let w = conds.join(" AND ");
    let bucket = b.sql("timestamp");
    let empty = empty_row(ERROR_ZEROS, &[]);
    let rows = state
        .ch
        .query(&format!("SELECT {bucket} AS bucket, {ERROR_METRICS} FROM errors WHERE {w} GROUP BY bucket ORDER BY bucket"), &ps)
        .await?;
    let rows = rename(rows, ERR_RENAME);
    let mut out = json!({
        "project": project.slug, "interval": b.name(), "bucket_seconds": b.seconds(),
        "window": {"since": iso_time(since), "until": iso_time(until)},
        "series": if opts.fill { fill(rows, &b, since, until, &empty) } else { rows },
    });
    if let Some(g) = opts.group_by.as_deref().filter(|g| !g.is_empty()) {
        let expr = error_group_expr(g).ok_or_else(|| ApiError::bad(format!("unknown group_by `{g}`; one of {}", ERROR_GROUPS.join(", "))))?;
        ps.push(("glim", opts.limit.unwrap_or(10).clamp(1, 50).to_string()));
        let rows = state
            .ch
            .query(
                &format!(
                    "SELECT toString({expr}) AS grp, {bucket} AS bucket, {ERROR_METRICS} FROM errors WHERE {w} AND {expr} IN \
                     (SELECT {expr} FROM errors WHERE {w} GROUP BY {expr} ORDER BY count() DESC LIMIT {{glim:UInt32}}) GROUP BY grp, bucket ORDER BY bucket"
                ),
                &ps,
            )
            .await?;
        let mut groups = Vec::new();
        for (key, total, rows) in split_groups(rename(rows, ERR_RENAME), "errors") {
            let mut gj = json!({"key": key, "total": total, "series": if opts.fill { fill(rows, &b, since, until, &empty) } else { rows }});
            if g == "issue" {
                if let Ok(Some(issue)) = crate::query::get_issue_row(state, &key).await {
                    gj["key"] = issue["short_id"].clone();
                    gj["issue_id"] = json!(key);
                    gj["title"] = issue["title"].clone();
                    gj["status"] = issue["status"].clone();
                }
            }
            groups.push(gj);
        }
        out["group_by"] = json!(g);
        out["groups"] = json!(groups);
    }
    Ok(out)
}

pub const PERF_GROUPS: &[&str] = &["transaction", "op", "release", "environment", "service", "http_status"];

fn perf_group_expr(g: &str) -> Option<&'static str> {
    Some(match g {
        "transaction" => "transaction",
        "op" => "op",
        "release" => "release",
        "environment" => "environment",
        "service" => "service",
        "http_status" => "toString(http_status)",
        _ => return None,
    })
}

const PERF_METRICS: &str = "count() AS count, quantiles(0.5, 0.75, 0.95, 0.99)(duration_ms) AS q, avg(duration_ms) AS avg_d, \
    countIf(status = 'error') AS n_err";

fn perf_row(r: Value, bucket_s: i64) -> Value {
    let q = r["q"].as_array().cloned().unwrap_or_default();
    let rnd = |v: Option<&Value>| match v.and_then(|v| v.as_f64()) {
        Some(f) if f.is_finite() => json!((f * 10.0).round() / 10.0),
        _ => Value::Null,
    };
    let count = r["count"].as_f64().unwrap_or(0.0);
    let errors = r["n_err"].as_f64().unwrap_or(0.0);
    json!({
        "bucket": r["bucket"], "count": r["count"], "per_minute": (count / (bucket_s as f64 / 60.0) * 1000.0).round() / 1000.0,
        "p50_ms": rnd(q.first()), "p75_ms": rnd(q.get(1)), "p95_ms": rnd(q.get(2)), "p99_ms": rnd(q.get(3)), "avg_ms": rnd(Some(&r["avg_d"])),
        "errors": r["n_err"], "error_rate": if count > 0.0 { json!((errors / count * 10_000.0).round() / 10_000.0) } else { Value::Null },
    })
}

/// Latency per bucket for root spans (transactions): count, throughput, p50..p99, avg, error rate.
pub async fn performance_timeseries(state: &AppState, p: &Principal, a: crate::perf::PerfParams, opts: SeriesOpts) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    let w = crate::perf::where_for(&proj.id, &a, a.op.as_deref().map(|o| o.is_empty()).unwrap_or(true));
    let until = w.until.min(now_ms());
    let b = parse_interval(opts.interval.as_deref(), w.since, until)?;
    let bucket = b.sql("start");
    let empty = empty_row(&["count", "per_minute", "errors"], &["p50_ms", "p75_ms", "p95_ms", "p99_ms", "avg_ms", "error_rate"]);
    let rows = state
        .ch
        .query(&format!("SELECT {bucket} AS bucket, {PERF_METRICS} FROM spans WHERE {} GROUP BY bucket ORDER BY bucket", w.sql), &w.params)
        .await?;
    let rows: Vec<Value> = rows.into_iter().map(|r| perf_row(r, b.seconds())).collect();
    let mut out = json!({
        "project": proj.slug, "interval": b.name(), "bucket_seconds": b.seconds(),
        "window": {"since": iso_time(w.since), "until": iso_time(until)},
        "series": if opts.fill { fill(rows, &b, w.since, until, &empty) } else { rows },
    });
    if let Some(g) = opts.group_by.as_deref().filter(|g| !g.is_empty()) {
        let expr = perf_group_expr(g).ok_or_else(|| ApiError::bad(format!("unknown group_by `{g}`; one of {}", PERF_GROUPS.join(", "))))?;
        let mut ps = w.params.clone();
        ps.push(("glim", opts.limit.unwrap_or(10).clamp(1, 50).to_string()));
        let rows = state
            .ch
            .query(
                &format!(
                    "SELECT {expr} AS grp, {bucket} AS bucket, {PERF_METRICS} FROM spans WHERE {w} AND {expr} IN \
                     (SELECT {expr} FROM spans WHERE {w} GROUP BY {expr} ORDER BY count() DESC LIMIT {{glim:UInt32}}) GROUP BY grp, bucket ORDER BY bucket",
                    w = w.sql
                ),
                &ps,
            )
            .await?;
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let grp = r["grp"].clone();
                let mut v = perf_row(r, b.seconds());
                v["grp"] = grp;
                v
            })
            .collect();
        out["group_by"] = json!(g);
        out["groups"] = split_groups(rows, "count")
            .into_iter()
            .map(|(key, total, rows)| json!({"key": key, "total": total, "series": if opts.fill { fill(rows, &b, w.since, until, &empty) } else { rows }}))
            .collect();
    }
    Ok(out)
}

#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct SessionSeriesParams {
    pub project: String,
    pub since: Option<String>,
    pub until: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
}

const SESS_RENAME: &[(&str, &str)] = &[("n_sessions", "sessions")];
pub const SESSION_GROUPS: &[&str] = &["release", "environment", "platform"];

const SESSION_METRICS: &str = "count() AS n_sessions, countIf(s_status = 'crashed') AS crashed, countIf(s_status = 'errored') AS errored, \
    uniq(s_distinct) AS users, uniqIf(s_distinct, s_status = 'crashed') AS crashed_users, \
    round(1 - crashed / greatest(n_sessions, 1), 4) AS crash_free_sessions, round(1 - crashed_users / greatest(users, 1), 4) AS crash_free_users";

/// Sessions per bucket (by session start) with crash-free rates; `group_by=release` gives
/// crash-free sessions per release over time.
pub async fn sessions_timeseries(state: &AppState, p: &Principal, a: SessionSeriesParams, opts: SeriesOpts) -> QResult<Value> {
    let project = project_for(state, p, &a.project).await?;
    let now = now_ms();
    let since = a.since.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 7 * 86_400_000);
    let until = a.until.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now);
    let b = parse_interval(opts.interval.as_deref(), since, until)?;
    let mut cond = "project_id = {p:String} AND started >= fromUnixTimestamp64Milli({since:Int64}) AND started <= fromUnixTimestamp64Milli({until:Int64})".to_string();
    let mut ps: Vec<(&'static str, String)> = vec![("p", project.id.clone()), ("since", since.to_string()), ("until", until.to_string())];
    if let Some(r) = a.release.as_deref().filter(|r| !r.is_empty()) {
        cond.push_str(" AND release = {rel:String}");
        ps.push(("rel", r.to_string()));
    }
    if let Some(e) = a.environment.as_deref().filter(|r| !r.is_empty()) {
        cond.push_str(" AND environment = {env:String}");
        ps.push(("env", e.to_string()));
    }
    let group = match opts.group_by.as_deref().filter(|g| !g.is_empty()) {
        None => None,
        Some(g @ ("release" | "environment" | "platform")) => Some(g),
        Some(g) => return Err(ApiError::bad(format!("unknown group_by `{g}`; one of {}", SESSION_GROUPS.join(", ")))),
    };
    let inner = format!(
        "SELECT session_id, argMax(status, timestamp) AS s_status, any(distinct_id) AS s_distinct, any(release) AS s_release, \
         any(environment) AS s_env, any(platform) AS s_platform, min(started) AS s_started FROM sessions WHERE {cond} GROUP BY session_id"
    );
    let bucket = b.sql("s_started");
    let empty = empty_row(&["sessions", "crashed", "errored", "users", "crashed_users"], &["crash_free_sessions", "crash_free_users"]);
    let rows = state
        .ch
        .query(&format!("SELECT {bucket} AS bucket, {SESSION_METRICS} FROM ({inner}) GROUP BY bucket ORDER BY bucket"), &ps)
        .await?;
    let rows = rename(rows, SESS_RENAME);
    let mut out = json!({
        "project": project.slug, "interval": b.name(), "bucket_seconds": b.seconds(),
        "window": {"since": iso_time(since), "until": iso_time(until)},
        "series": if opts.fill { fill(rows, &b, since, until, &empty) } else { rows },
    });
    if let Some(g) = group {
        let col = match g {
            "release" => "s_release",
            "environment" => "s_env",
            _ => "s_platform",
        };
        ps.push(("glim", opts.limit.unwrap_or(5).clamp(1, 50).to_string()));
        let rows = state
            .ch
            .query(
                &format!(
                    "SELECT {col} AS grp, {bucket} AS bucket, {SESSION_METRICS} FROM ({inner}) WHERE {col} IN \
                     (SELECT {col} FROM ({inner}) GROUP BY {col} ORDER BY count() DESC LIMIT {{glim:UInt32}}) GROUP BY grp, bucket ORDER BY bucket"
                ),
                &ps,
            )
            .await?;
        out["group_by"] = json!(g);
        out["groups"] = split_groups(rename(rows, SESS_RENAME), "sessions")
            .into_iter()
            .map(|(key, total, rows)| json!({"key": key, "total": total, "series": if opts.fill { fill(rows, &b, since, until, &empty) } else { rows }}))
            .collect();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_intervals() {
        let day = 86_400_000;
        assert_eq!(parse_interval(None, 0, day).unwrap(), Bucketing::Fixed(3600));
        assert_eq!(parse_interval(None, 0, 10 * day).unwrap(), Bucketing::Fixed(86_400));
        assert_eq!(parse_interval(Some("minute"), 0, day).unwrap(), Bucketing::Fixed(60));
        assert_eq!(parse_interval(Some("5m"), 0, day).unwrap(), Bucketing::Fixed(300));
        assert_eq!(parse_interval(Some("30s"), 0, day).unwrap(), Bucketing::Fixed(60));
        assert_eq!(parse_interval(Some("90s"), 0, day).unwrap(), Bucketing::Fixed(120));
        assert_eq!(parse_interval(Some("1w"), 0, 30 * day).unwrap(), Bucketing::Week);
        assert_eq!(parse_interval(Some("6h"), 0, 30 * day).unwrap().name(), "6h");
        assert!(parse_interval(Some("minute"), 0, 30 * day).is_err());
        assert!(parse_interval(Some("fortnight"), 0, day).is_err());
    }

    #[test]
    fn sql_and_labels_agree_on_alignment() {
        // 2026-11-18 is a Wednesday; its week starts Monday 2026-11-16.
        let wed = 1_795_000_000; // 2026-11-18T...
        let b = Bucketing::Week;
        assert_eq!(bucket_label(b.floor(wed)), "2026-11-16T00:00:00Z");
        assert_eq!(bucket_label(Bucketing::Month.floor(wed)), "2026-11-01T00:00:00Z");
        assert_eq!(bucket_label(Bucketing::Month.next(Bucketing::Month.floor(wed))), "2026-12-01T00:00:00Z");
        assert_eq!(bucket_label(Bucketing::Fixed(300).floor(wed)), bucket_label(wed - wed % 300));
    }

    #[test]
    fn fills_missing_buckets() {
        let b = Bucketing::Fixed(60);
        let rows = vec![json!({"bucket": "1970-01-01T00:01:00Z", "n": 5})];
        let out = fill(rows, &b, 0, 179_000, &empty_row(&["n"], &["p95"]));
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], json!({"bucket": "1970-01-01T00:00:00Z", "n": 0, "p95": null}));
        assert_eq!(out[1]["n"], 5);
        assert_eq!(out[2]["bucket"], "1970-01-01T00:02:00Z");
    }

    #[test]
    fn groups_rank_by_total() {
        let rows = vec![
            json!({"grp": "a", "bucket": "x", "n": 1}),
            json!({"grp": "b", "bucket": "x", "n": 3}),
            json!({"grp": "a", "bucket": "y", "n": 1}),
        ];
        let g = split_groups(rows, "n");
        assert_eq!(g[0].0, "b");
        assert_eq!(g[1].1, 2.0);
        assert!(g[1].2[0].get("grp").is_none());
    }
}
