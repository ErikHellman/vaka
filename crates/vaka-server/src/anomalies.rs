//! Anomaly detection: compares the last window (default 24h) with the same window one to four
//! weeks earlier, across errors, issues, crash rate, traffic, custom events, latency and Web Vitals,
//! and explains each finding in a sentence. Also writes digest narratives (with an
//! OpenAI-compatible LLM when `VAKA_LLM_BASE_URL` and `VAKA_LLM_MODEL` are set, otherwise from templates).

use crate::auth::{ApiError, Principal};
use crate::db::RowExt;
use crate::query::{project_for, QResult};
use crate::state::{AppState, ProjectInfo};
use crate::util::{iso_time, now_ms};
use serde_json::{json, Value};
use std::time::Duration;

const WEEK: i64 = 7 * 86_400_000;
const BASE_WEEKS: i64 = 4;
/// Bucket 5 is the window right before the current one; used when there is no weekly history yet.
const PREV: i64 = 5;

/// Parse a window like 1h, 24h or 7d (1h to 7d).
pub fn parse_window(s: Option<&str>) -> QResult<i64> {
    let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) else { return Ok(86_400_000) };
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num.parse().map_err(|_| ApiError::bad(format!("bad window `{s}`; use e.g. 6h, 24h or 7d")))?;
    let ms = match unit {
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        _ => return Err(ApiError::bad(format!("bad window `{s}`; use e.g. 6h, 24h or 7d"))),
    };
    if !(3_600_000..=WEEK).contains(&ms) {
        return Err(ApiError::bad("window must be between 1h and 7d"));
    }
    Ok(ms)
}

pub fn window_label(w: i64) -> String {
    if w % 86_400_000 == 0 && w > 86_400_000 {
        format!("{}d", w / 86_400_000)
    } else {
        format!("{}h", w / 3_600_000)
    }
}

fn period_phrase(w: i64) -> String {
    match w {
        86_400_000 => "the last 24 hours".into(),
        WEEK => "the last 7 days".into(),
        _ => format!("the last {}", window_label(w)),
    }
}

/// Bucket bounds: 0 = [now-w, now], k = [now-k·7d-w, now-k·7d] for k in 1..=4, 5 = [now-2w, now-w].
#[derive(Clone, Copy)]
struct Buckets {
    now: i64,
    w: i64,
}

impl Buckets {
    fn lo(&self, k: i64) -> i64 {
        if k == PREV {
            self.now - 2 * self.w
        } else {
            self.now - k * WEEK - self.w
        }
    }
    fn hi(&self, k: i64) -> i64 {
        if k == PREV {
            self.now - self.w
        } else {
            self.now - k * WEEK
        }
    }
    fn earliest(&self) -> i64 {
        self.lo(BASE_WEEKS).min(self.lo(PREV))
    }
    fn params(&self, project_id: &str) -> Vec<(&'static str, String)> {
        let mut v = vec![("p", project_id.to_string()), ("lo", self.earliest().to_string()), ("now", self.now.to_string()), ("b0", (self.now - self.w).to_string())];
        const NAMES: [(&str, &str); 5] = [("l1", "h1"), ("l2", "h2"), ("l3", "h3"), ("l4", "h4"), ("l5", "h5")];
        for (i, (l, h)) in NAMES.iter().enumerate() {
            let k = i as i64 + 1;
            v.push((l, self.lo(k).to_string()));
            v.push((h, self.hi(k).to_string()));
        }
        v
    }
}

/// SQL for the bucket number of a time column (-1 when outside every bucket).
fn bucket_sql(col: &str) -> String {
    let t = format!("toUnixTimestamp64Milli({col})");
    let mut s = format!("multiIf({t} >= {{b0:Int64}} AND {t} <= {{now:Int64}}, 0");
    for k in 1..=5 {
        s.push_str(&format!(", {t} >= {{l{k}:Int64}} AND {t} < {{h{k}:Int64}}, {k}"));
    }
    s.push_str(", -1)");
    s
}

fn time_where(col: &str) -> String {
    format!("project_id = {{p:String}} AND {col} >= fromUnixTimestamp64Milli({{lo:Int64}}) AND {col} <= fromUnixTimestamp64Milli({{now:Int64}})")
}

fn num(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Count,
    Rate,
    Duration,
}

/// Values per bucket for one metric.
#[derive(Default, Clone, Debug)]
pub struct Series {
    pub vals: [f64; 6],
    /// Sample size behind the current value (sessions for a rate, spans for a duration).
    pub n_cur: f64,
    /// Buckets with too few samples to use.
    pub missing: [bool; 6],
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub z: f64,
    pub baseline: f64,
    pub current: f64,
    pub change: Option<f64>,
}

/// Robust z-score of the current value against the baseline buckets. `min_abs` is the smallest
/// absolute change worth reporting.
pub fn score(kind: Kind, cur: f64, base: &[f64], n_cur: f64, min_abs: f64) -> Option<Score> {
    if base.is_empty() {
        return None;
    }
    let mut b = base.to_vec();
    let m = median(&mut b);
    let mut dev: Vec<f64> = base.iter().map(|x| (x - m).abs()).collect();
    let mad = 1.4826 * median(&mut dev);
    let sigma = match kind {
        Kind::Count => mad.max(m.max(1.0).sqrt()).max(0.1 * m),
        Kind::Rate => mad.max((m * (1.0 - m) / n_cur.max(1.0)).sqrt()).max(0.002),
        Kind::Duration => mad.max(0.1 * m).max(5.0),
    };
    let z = (cur - m) / sigma;
    let change = if m > 0.0 { Some((cur - m) / m) } else { None };
    let big_enough = (cur - m).abs() >= min_abs && change.map(|c| kind == Kind::Rate || c.abs() >= 0.25).unwrap_or(true);
    if z.abs() >= 3.0 && big_enough {
        Some(Score { z, baseline: m, current: cur, change })
    } else {
        None
    }
}

struct Ctx<'a> {
    proj: &'a ProjectInfo,
    b: Buckets,
    /// Buckets that make up the baseline.
    base: Vec<i64>,
    release: Option<(String, i64)>,
    out: Vec<Value>,
}

impl Ctx<'_> {
    fn baseline(&self, s: &Series) -> Vec<f64> {
        self.base.iter().filter(|k| !s.missing[**k as usize]).map(|k| s.vals[*k as usize]).collect()
    }

    fn evidence(&self, s: &Series) -> Value {
        json!({
            "current": round(s.vals[0]),
            "baseline": self.base.iter().map(|k| json!({
                "bucket": if *k == PREV { "previous window".to_string() } else { format!("{k}w ago") },
                "since": iso_time(self.b.lo(*k)), "value": round(s.vals[*k as usize]),
            })).collect::<Vec<_>>(),
        })
    }

    fn release_suffix(&self) -> String {
        match &self.release {
            Some((v, t)) => format!(", since release {v} shipped {} ago", ago(self.b.now - t)),
            None => String::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(&mut self, metric: &str, subject: Value, kind: Kind, s: &Series, min_abs: f64, bad_when_up: Option<bool>, sentence: impl Fn(&Score) -> String, next: Value) {
        let base = self.baseline(s);
        let Some(sc) = score(kind, s.vals[0], &base, s.n_cur, min_abs) else { return };
        let up = sc.z > 0.0;
        if let Some(bad_up) = bad_when_up {
            // Only report the harmful direction (fewer errors is not an anomaly worth a page).
            if bad_up != up {
                return;
            }
        }
        let severity = if sc.z.abs() >= 6.0 || sc.change.map(|c| c.abs() >= 2.0).unwrap_or(true) { "high" } else { "medium" };
        self.out.push(json!({
            "metric": metric, "subject": subject, "direction": if up { "up" } else { "down" },
            "severity": severity, "z": (sc.z * 10.0).round() / 10.0,
            "current": round(sc.current), "baseline": round(sc.baseline),
            "change_pct": sc.change.map(|c| (c * 1000.0).round() / 10.0),
            "sentence": sentence(&sc), "evidence": self.evidence(s), "next_tool": next,
        }));
    }
}

fn round(x: f64) -> f64 {
    if x.abs() < 10.0 {
        (x * 10_000.0).round() / 10_000.0
    } else {
        x.round()
    }
}

fn ago(ms: i64) -> String {
    let m = ms / 60_000;
    if m < 60 {
        format!("{}m", m.max(1))
    } else if m < 48 * 60 {
        format!("{}h", m / 60)
    } else {
        format!("{}d", m / 1440)
    }
}

fn pct_change(sc: &Score) -> String {
    match sc.change {
        Some(c) => format!("{} {:.0}%", if c >= 0.0 { "up" } else { "down" }, c.abs() * 100.0),
        None => "new".into(),
    }
}

fn fmt_n(x: f64) -> String {
    let n = x.round() as i64;
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn fmt_ms(x: f64) -> String {
    if x >= 1000.0 {
        format!("{:.1} s", x / 1000.0)
    } else {
        format!("{x:.0} ms")
    }
}

/// Group `rows` (with `bk` and a key column) into series per key.
fn series_by(rows: &[Value], key: &str, val: &str, n: Option<&str>) -> Vec<(String, Series)> {
    let mut map: Vec<(String, Series)> = Vec::new();
    for r in rows {
        let k = r[key].as_str().map(str::to_string).unwrap_or_else(|| r[key].to_string());
        let bk = num(&r["bk"]) as i64;
        if !(0..=5).contains(&bk) {
            continue;
        }
        let idx = match map.iter().position(|(x, _)| *x == k) {
            Some(i) => i,
            None => {
                // With a sample-size column, buckets without a row (or with few samples) are unknown, not zero.
                map.push((k, Series { missing: [n.is_some(); 6], ..Default::default() }));
                map.len() - 1
            }
        };
        if let Some(n) = n {
            map[idx].1.missing[bk as usize] = num(&r[n]) < 10.0;
        }
        map[idx].1.vals[bk as usize] = num(&r[val]);
        if bk == 0 {
            if let Some(n) = n {
                map[idx].1.n_cur = num(&r[n]);
            }
        }
    }
    map
}

/// Detect anomalies for one project. `w` is the window in ms.
pub async fn detect_for(state: &AppState, proj: &ProjectInfo, now: i64, w: i64) -> anyhow::Result<Value> {
    let b = Buckets { now, w };
    let ps = b.params(&proj.id);
    let psr: Vec<(&str, String)> = ps.iter().map(|(k, v)| (*k, v.clone())).collect();

    // Which baseline buckets have data behind them: the project must have been sending by then.
    let first = state
        .ch
        .query(
            "SELECT min(m) AS first FROM (\
             SELECT toUnixTimestamp64Milli(min(timestamp)) AS m, count() AS c FROM errors WHERE project_id = {p:String} \
             UNION ALL SELECT toUnixTimestamp64Milli(min(timestamp)) AS m, count() AS c FROM events WHERE project_id = {p:String} \
             UNION ALL SELECT toUnixTimestamp64Milli(min(started)) AS m, count() AS c FROM sessions WHERE project_id = {p:String} \
             UNION ALL SELECT toUnixTimestamp64Milli(min(start)) AS m, count() AS c FROM spans WHERE project_id = {p:String}) WHERE c > 0",
            &psr,
        )
        .await?;
    let first_seen = first.first().map(|r| num(&r["first"]) as i64).filter(|t| *t > 0);
    let slack = w / 4;
    let mut base: Vec<i64> = match first_seen {
        Some(f) => (1..=BASE_WEEKS).filter(|k| f <= b.lo(*k) + slack).collect(),
        None => Vec::new(),
    };
    if base.is_empty() && first_seen.map(|f| f <= b.lo(PREV) + slack).unwrap_or(false) {
        base.push(PREV);
    }
    let release = sqlx::query("SELECT version, created_at FROM releases WHERE project_id = $1 AND created_at >= $2 ORDER BY created_at DESC LIMIT 1")
        .bind(&proj.id)
        .bind(now - w)
        .fetch_optional(&state.db)
        .await?
        .map(|r| (r.s("version"), r.i("created_at")));
    let mut cx = Ctx { proj, b, base, release, out: Vec::new() };
    let period = period_phrase(w);
    let slug = proj.slug.clone();

    if !cx.base.is_empty() {
        let bk = |col: &str| bucket_sql(col);
        // Errors overall and per issue.
        let rows = state.ch.query(&format!("SELECT {} AS bk, issue_id, count() AS n_errors FROM errors WHERE {} GROUP BY bk, issue_id", bk("timestamp"), time_where("timestamp")), &psr).await?;
        let mut total = Series::default();
        for r in &rows {
            let k = num(&r["bk"]) as i64;
            if (0..=5).contains(&k) {
                total.vals[k as usize] += num(&r["n_errors"]);
            }
        }
        let rs = cx.release_suffix();
        cx.push(
            "errors",
            json!(null),
            Kind::Count,
            &total,
            10.0,
            Some(true),
            |sc| format!("Errors are {} ({} in {period} vs a usual {}){rs}.", pct_change(sc), fmt_n(sc.current), fmt_n(sc.baseline)),
            json!({"tool": "explain_spike", "args": {"project": slug, "window": window_label(w)}}),
        );
        let mut issues = series_by(&rows, "issue_id", "n_errors", None);
        issues.retain(|(_, s)| s.vals[0] >= 5.0);
        issues.sort_by(|a, b| b.1.vals[0].partial_cmp(&a.1.vals[0]).unwrap_or(std::cmp::Ordering::Equal));
        let mut n_issue = 0;
        for (id, s) in issues.into_iter().take(50) {
            let before = cx.out.len();
            let Some(issue) = crate::query::get_issue_row(state, &id).await? else { continue };
            let short = issue["short_id"].as_str().unwrap_or(&id).to_string();
            let title = crate::util::truncate(issue["title"].as_str().unwrap_or(""), 80);
            let is_new = cx.baseline(&s).iter().all(|v| *v == 0.0);
            cx.push(
                "issue_errors",
                json!({"issue": short, "title": title, "status": issue["status"], "url": issue["url"]}),
                Kind::Count,
                &s,
                5.0,
                Some(true),
                |sc| {
                    if is_new {
                        format!("{short} `{title}` is new: {} events in {period}.", fmt_n(sc.current))
                    } else {
                        format!("{short} `{title}` jumped to {} events (usually {}).", fmt_n(sc.current), fmt_n(sc.baseline))
                    }
                },
                json!({"tool": "get_issue", "args": {"issue": short}}),
            );
            if cx.out.len() > before {
                n_issue += 1;
                if n_issue >= 10 {
                    break;
                }
            }
        }

        // Crash-free sessions, overall and per release.
        let rows = state
            .ch
            .query(
                &format!(
                    "SELECT {} AS bk, s_release, count() AS n_sessions, countIf(s_status = 'crashed') AS n_crashed FROM \
                     (SELECT session_id, argMax(status, timestamp) AS s_status, min(started) AS s_started, any(release) AS s_release FROM sessions WHERE {} GROUP BY session_id) \
                     GROUP BY bk, s_release",
                    bk("s_started"),
                    time_where("started")
                ),
                &psr,
            )
            .await?;
        let mut sess = [0.0f64; 6];
        let mut crashed = [0.0f64; 6];
        for r in &rows {
            let k = num(&r["bk"]) as i64;
            if (0..=5).contains(&k) {
                sess[k as usize] += num(&r["n_sessions"]);
                crashed[k as usize] += num(&r["n_crashed"]);
            }
        }
        let rate = |s: f64, c: f64| if s > 0.0 { 1.0 - c / s } else { 1.0 };
        let mut cf = Series { n_cur: sess[0], ..Default::default() };
        for k in 0..6 {
            cf.vals[k] = rate(sess[k], crashed[k]);
        }
        let usual = {
            let mut v = cx.baseline(&cf);
            median(&mut v)
        };
        if sess[0] >= 20.0 && cx.base.iter().any(|k| sess[*k as usize] >= 20.0) {
            let rs = cx.release_suffix();
            cx.push(
                "crash_free_sessions",
                json!(null),
                Kind::Rate,
                &cf,
                0.005,
                Some(false),
                |sc| format!("Crash-free sessions fell to {:.1}% (usually {:.1}%){rs}.", sc.current * 100.0, sc.baseline * 100.0),
                json!({"tool": "release_health", "args": {"project": slug}}),
            );
            // A release with a clearly worse crash rate than the project's usual one.
            for r in rows.iter().filter(|r| num(&r["bk"]) as i64 == 0 && num(&r["n_sessions"]) >= 20.0) {
                let rel = r["s_release"].as_str().unwrap_or("").to_string();
                if rel.is_empty() {
                    continue;
                }
                let mut s = cf.clone();
                s.vals[0] = rate(num(&r["n_sessions"]), num(&r["n_crashed"]));
                s.n_cur = num(&r["n_sessions"]);
                // A release with nearly all sessions is already covered by the overall rate.
                if s.n_cur >= 0.9 * sess[0] {
                    continue;
                }
                cx.push(
                    "release_crash_free",
                    json!({"release": rel}),
                    Kind::Rate,
                    &s,
                    0.005,
                    Some(false),
                    |sc| format!("Release {rel} is at {:.1}% crash-free sessions vs a usual {:.1}%.", sc.current * 100.0, usual * 100.0),
                    json!({"tool": "release_health", "args": {"project": slug, "release": rel}}),
                );
            }
        }

        // Traffic and custom events.
        let rows = state
            .ch
            .query(
                &format!(
                    "SELECT {} AS bk, countIf(type = 'pageview') AS n_pageviews, uniqIf(visitor_id, type IN ('pageview', 'screen_view')) AS n_visitors FROM events WHERE {} GROUP BY bk",
                    bk("timestamp"),
                    time_where("timestamp")
                ),
                &psr,
            )
            .await?;
        let mut pv = Series::default();
        let mut vis = Series::default();
        for r in &rows {
            let k = num(&r["bk"]) as i64;
            if (0..=5).contains(&k) {
                pv.vals[k as usize] = num(&r["n_pageviews"]);
                vis.vals[k as usize] = num(&r["n_visitors"]);
            }
        }
        let next_traffic = json!({"tool": "analytics_breakdown", "args": {"project": slug, "dimension": "referrer", "since": window_label(w)}});
        cx.push("pageviews", json!(null), Kind::Count, &pv, 20.0, None, |sc| format!("Pageviews are {} ({} vs a usual {}).", pct_change(sc), fmt_n(sc.current), fmt_n(sc.baseline)), next_traffic.clone());
        cx.push("visitors", json!(null), Kind::Count, &vis, 20.0, None, |sc| format!("Visitors are {} ({} vs a usual {}).", pct_change(sc), fmt_n(sc.current), fmt_n(sc.baseline)), next_traffic);
        let rows = state
            .ch
            .query(
                &format!(
                    "SELECT {} AS bk, name AS ev_name, count() AS n_events FROM events WHERE {} AND type = 'custom' AND name IN \
                     (SELECT name FROM events WHERE {} AND type = 'custom' GROUP BY name ORDER BY count() DESC LIMIT 30) GROUP BY bk, ev_name",
                    bk("timestamp"),
                    time_where("timestamp"),
                    time_where("timestamp")
                ),
                &psr,
            )
            .await?;
        let mut n_ev = 0;
        for (name, s) in series_by(&rows, "ev_name", "n_events", None) {
            let before = cx.out.len();
            cx.push(
                "event_count",
                json!({"event": name}),
                Kind::Count,
                &s,
                20.0,
                None,
                |sc| format!("`{name}` events are {} ({} vs a usual {}).", pct_change(sc), fmt_n(sc.current), fmt_n(sc.baseline)),
                json!({"tool": "analytics_timeseries", "args": {"project": slug, "type": "custom", "since": "7d"}}),
            );
            if cx.out.len() > before {
                n_ev += 1;
                if n_ev >= 5 {
                    break;
                }
            }
        }

        // p95 of the busiest transactions.
        let rows = state
            .ch
            .query(
                &format!(
                    "SELECT {} AS bk, transaction AS tx, quantile(0.95)(duration_ms) AS p95, count() AS n_spans FROM spans WHERE {} AND is_root = 1 AND op != 'web.vitals' \
                     AND transaction IN (SELECT transaction FROM spans WHERE {} AND is_root = 1 AND op != 'web.vitals' GROUP BY transaction ORDER BY count() DESC LIMIT 20) \
                     GROUP BY bk, tx",
                    bk("start"),
                    time_where("start"),
                    time_where("start")
                ),
                &psr,
            )
            .await?;
        let mut n_tx = 0;
        for (tx, s) in series_by(&rows, "tx", "p95", Some("n_spans")) {
            if s.missing[0] {
                continue;
            }
            let before = cx.out.len();
            cx.push(
                "transaction_p95",
                json!({"transaction": tx}),
                Kind::Duration,
                &s,
                50.0,
                Some(true),
                |sc| format!("p95 of `{tx}` rose to {} (usually {}).", fmt_ms(sc.current), fmt_ms(sc.baseline)),
                json!({"tool": "list_slow_traces", "args": {"project": slug, "transaction": tx, "since": window_label(w)}}),
            );
            if cx.out.len() > before {
                n_tx += 1;
                if n_tx >= 5 {
                    break;
                }
            }
        }

        // Web Vitals p75.
        let rows = state
            .ch
            .query(
                &format!(
                    "SELECT {} AS bk, k AS vital, quantile(0.75)(v) AS p75, count() AS n_samples FROM spans ARRAY JOIN mapKeys(measurements) AS k, mapValues(measurements) AS v \
                     WHERE {} AND op = 'web.vitals' AND k IN ('lcp', 'inp', 'cls') GROUP BY bk, vital",
                    bk("start"),
                    time_where("start")
                ),
                &psr,
            )
            .await?;
        for (vital, s) in series_by(&rows, "vital", "p75", Some("n_samples")) {
            if s.missing[0] {
                continue;
            }
            let cls = vital == "cls";
            let label = vital.to_uppercase();
            let mut s2 = s.clone();
            if cls {
                // Score CLS in thousandths so the duration floors make sense.
                for v in s2.vals.iter_mut() {
                    *v *= 1000.0;
                }
            }
            cx.push(
                "web_vital_p75",
                json!({"vital": vital}),
                Kind::Duration,
                &s2,
                if cls { 20.0 } else { 100.0 },
                Some(true),
                |sc| {
                    if cls {
                        format!("CLS p75 rose to {:.2} (usually {:.2}).", sc.current / 1000.0, sc.baseline / 1000.0)
                    } else {
                        format!("{label} p75 rose to {} (usually {}).", fmt_ms(sc.current), fmt_ms(sc.baseline))
                    }
                },
                json!({"tool": "web_vitals", "args": {"project": slug, "since": window_label(w)}}),
            );
        }
    }

    let mut out = std::mem::take(&mut cx.out);
    out.sort_by(|a, b| {
        let rank = |v: &Value| (v["severity"] == "high") as i32;
        rank(b).cmp(&rank(a)).then(num(&b["z"]).abs().partial_cmp(&num(&a["z"]).abs()).unwrap_or(std::cmp::Ordering::Equal))
    });
    let baseline_desc = if cx.base.is_empty() {
        "not enough history yet (needs data from before the previous window)".to_string()
    } else if cx.base == [PREV] {
        "the previous window (less than a week of history)".to_string()
    } else {
        format!("the same {} window {} week{} earlier (median)", window_label(w), cx.base.len(), if cx.base.len() == 1 { "" } else { "s" })
    };
    Ok(json!({
        "project": cx.proj.slug,
        "window": window_label(w),
        "since": iso_time(now - w),
        "until": iso_time(now),
        "baseline": baseline_desc,
        "recent_release": cx.release.as_ref().map(|(v, t)| json!({"version": v, "created_at": iso_time(*t)})),
        "anomalies": out,
        "method": "Each metric is compared with the same window 1 to 4 weeks earlier (so weekly seasonality cancels out), using a robust z-score: (current - median) / max(1.4826 x MAD, noise floor). Reported when |z| >= 3 and the change is large enough to matter (25% for counts and latency).",
    }))
}

pub async fn detect(state: &AppState, p: &Principal, project: &str, window: Option<&str>) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let w = parse_window(window)?;
    Ok(detect_for(state, &proj, now_ms(), w).await?)
}

// ---------------------------------------------------------------------------
// Narratives
// ---------------------------------------------------------------------------

/// A short summary of a digest payload from templates.
pub fn template_narrative(payload: &Value) -> String {
    let period = payload["period"]["label"].as_str().unwrap_or("the last 24 hours").to_string();
    let projects = payload["projects"].as_array().cloned().unwrap_or_default();
    let mut found: Vec<(String, String)> = Vec::new();
    for p in &projects {
        let name = p["project"]["name"].as_str().unwrap_or("").to_string();
        for a in p["anomalies"].as_array().into_iter().flatten() {
            found.push((name.clone(), a["sentence"].as_str().unwrap_or("").to_string()));
        }
    }
    if found.is_empty() {
        return format!("Nothing unusual in {period}: errors, crash rate, traffic and latency are within their normal weekly range.");
    }
    let multi = projects.len() > 1;
    let lead = match found.len() {
        1 => "One thing looks unusual.".to_string(),
        n => format!("{n} things look unusual."),
    };
    let top: Vec<String> = found.iter().take(3).map(|(p, s)| if multi { format!("{p}: {s}") } else { s.clone() }).collect();
    format!("{lead} {}", top.join(" "))
}

/// Write the digest narrative with the configured LLM, falling back to templates.
/// Returns (text, source) where source is "llm" or "template".
pub async fn narrative(state: &AppState, payload: &Value) -> (String, &'static str) {
    let fallback = template_narrative(payload);
    let (Some(base), Some(model)) = (&state.config.llm_base_url, &state.config.llm_model) else { return (fallback, "template") };
    if payload["projects"].as_array().map(|p| p.is_empty()).unwrap_or(true) {
        return (fallback, "template");
    }
    match llm_narrative(state, base, model, payload).await {
        Ok(t) if !t.trim().is_empty() => (t.trim().to_string(), "llm"),
        Ok(_) => (fallback, "template"),
        Err(e) => {
            tracing::warn!("digest narrative via LLM failed, using template: {e:#}");
            (fallback, "template")
        }
    }
}

const NARRATIVE_SYSTEM: &str = "You write the opening paragraph of a scheduled monitoring digest that engineers read in Slack or email. \
You get the digest data as JSON: per project, totals for the period and a list of detected anomalies with plain-English sentences, \
baselines and evidence, plus user feedback counts with a sentiment split and quotes when there is any. Write 2 to 4 sentences of plain prose (no headings, no bullet points, no markdown) that say what matters most \
and why: lead with the most severe or user-facing change, connect related findings (for example a new release and a crash-rate drop), \
and say when nothing needs attention. Use only facts in the data, keep issue ids like WEB-12 and release names exactly as given, and do not \
speculate about causes the data does not show.";

/// The chat completions request body (OpenAI-compatible).
pub fn narrative_request(model: &str, payload: &Value) -> anyhow::Result<Value> {
    // Keep the prompt small: drop evidence series and links.
    let mut slim = payload.clone();
    if let Some(ps) = slim["projects"].as_array_mut() {
        for p in ps {
            for a in p["anomalies"].as_array_mut().into_iter().flatten() {
                if let Some(o) = a.as_object_mut() {
                    o.remove("evidence");
                    o.remove("next_tool");
                }
            }
        }
    }
    Ok(json!({
        "model": model,
        "max_tokens": 2000,
        "messages": [
            {"role": "system", "content": NARRATIVE_SYSTEM},
            {"role": "user", "content": format!("Digest data:\n{}", serde_json::to_string(&slim)?)},
        ],
    }))
}

/// Text of the first choice of a chat completions response.
pub fn narrative_text(v: &Value) -> anyhow::Result<String> {
    let choice = &v["choices"][0];
    if choice["finish_reason"] == "content_filter" {
        anyhow::bail!("the model declined to write the narrative");
    }
    let content = &choice["message"]["content"];
    // Some servers return content as an array of parts.
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => anyhow::bail!("response has no choices[0].message.content"),
    };
    Ok(text)
}

async fn llm_narrative(state: &AppState, base: &str, model: &str, payload: &Value) -> anyhow::Result<String> {
    let mut req = state.http.post(format!("{base}/chat/completions")).json(&narrative_request(model, payload)?).timeout(Duration::from_secs(90));
    if let Some(key) = &state.config.llm_api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let v: Value = resp.json().await?;
    if !status.is_success() {
        let msg = v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| v["error"].to_string());
        anyhow::bail!("LLM API returned {status}: {msg}");
    }
    Ok(crate::util::defang_llm_text(&narrative_text(&v)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robust_score() {
        // A burst over a steady baseline is flagged; normal noise is not.
        assert!(score(Kind::Count, 300.0, &[100.0, 110.0, 95.0, 105.0], 0.0, 10.0).is_some());
        assert!(score(Kind::Count, 115.0, &[100.0, 110.0, 95.0, 105.0], 0.0, 10.0).is_none());
        // Small counts need a real absolute change.
        assert!(score(Kind::Count, 6.0, &[1.0, 2.0, 1.0], 0.0, 10.0).is_none());
        let s = score(Kind::Count, 40.0, &[0.0, 0.0, 0.0], 0.0, 5.0).unwrap();
        assert_eq!(s.change, None);
        // Crash-free dropping from ~99.5% to 90% over 200 sessions.
        let s = score(Kind::Rate, 0.90, &[0.995, 0.994, 0.996], 200.0, 0.005).unwrap();
        assert!(s.z < -3.0);
        assert!(score(Kind::Rate, 0.993, &[0.995, 0.994, 0.996], 200.0, 0.005).is_none());
        // Latency doubling is flagged; 10% is not.
        assert!(score(Kind::Duration, 640.0, &[300.0, 320.0, 310.0], 50.0, 50.0).is_some());
        assert!(score(Kind::Duration, 340.0, &[300.0, 320.0, 310.0], 50.0, 50.0).is_none());
        assert!(score(Kind::Count, 1.0, &[], 0.0, 0.0).is_none());
    }

    #[test]
    fn windows_and_buckets() {
        assert_eq!(parse_window(None).unwrap(), 86_400_000);
        assert_eq!(parse_window(Some("7d")).unwrap(), WEEK);
        assert!(parse_window(Some("8d")).is_err());
        assert!(parse_window(Some("30m")).is_err());
        let b = Buckets { now: 100 * WEEK, w: 86_400_000 };
        assert_eq!(b.hi(1), 99 * WEEK);
        assert_eq!(b.lo(1), 99 * WEEK - 86_400_000);
        assert_eq!(b.lo(PREV), 100 * WEEK - 2 * 86_400_000);
        assert_eq!(b.earliest(), 96 * WEEK - 86_400_000);
        assert!(bucket_sql("timestamp").contains("{l5:Int64}"));
        assert_eq!(fmt_n(1234567.0), "1,234,567");
        assert_eq!(fmt_ms(1530.0), "1.5 s");
    }

    #[test]
    fn llm_request_and_response() {
        let payload = json!({"projects": [{"project": {"name": "Shop"}, "anomalies": [{"sentence": "Errors are up 200%.", "evidence": {"x": 1}, "next_tool": {}}]}]});
        let r = narrative_request("gpt-test", &payload).unwrap();
        assert_eq!(r["model"], "gpt-test");
        assert_eq!(r["messages"][0]["role"], "system");
        let user = r["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("Errors are up 200%.") && !user.contains("evidence"));
        assert_eq!(narrative_text(&json!({"choices": [{"message": {"content": "All good."}, "finish_reason": "stop"}]})).unwrap(), "All good.");
        assert_eq!(narrative_text(&json!({"choices": [{"message": {"content": [{"type": "text", "text": "A"}, {"type": "text", "text": "B"}]}}]})).unwrap(), "AB");
        assert!(narrative_text(&json!({"choices": [{"message": {"content": ""}, "finish_reason": "content_filter"}]})).is_err());
        assert!(narrative_text(&json!({"choices": []})).is_err());
    }

    #[test]
    fn template_text() {
        let quiet = json!({"period": {"label": "the last 24 hours"}, "projects": [{"project": {"name": "Shop"}, "anomalies": []}]});
        assert!(template_narrative(&quiet).starts_with("Nothing unusual in the last 24 hours"));
        let busy = json!({"projects": [{"project": {"name": "Shop"}, "anomalies": [{"sentence": "Errors are up 200%."}, {"sentence": "WEB-1 is new."}]}]});
        assert_eq!(template_narrative(&busy), "2 things look unusual. Errors are up 200%. WEB-1 is new.");
    }
}
