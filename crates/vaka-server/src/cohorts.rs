//! Cohorts (saved groups of people defined by what they did), plus stickiness and lifecycle
//! analytics. Cohorts are evaluated at query time against the `events` table and can be used as
//! a `cohort` filter in every analytics query, and as a funnel breakdown.

use crate::auth::{ApiError, Principal};
use crate::db::RowExt;
use crate::product::Step;
use crate::query::{analytics_where, filter_cond, project_for, AnalyticsParams, QResult};
use crate::state::AppState;
use crate::util::{iso_time, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MAX_CRITERIA: usize = 6;
const MAX_PROPERTIES: usize = 4;

/// One condition: did (or did not) do an event at least `min_count` times within `within`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Criterion {
    /// Event name (`pageview`, `screen_view` or a custom event). Or use `step`.
    #[serde(default)]
    pub event: String,
    /// Step shorthand: `purchase|prop:plan:pro`, `pageview|path:/pricing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filters: BTreeMap<String, String>,
    /// At least this many times (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_count: Option<i64>,
    /// Look-back window from now, e.g. `7d`, `30d` (default: the cohort's `within`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    /// true: people who did NOT do this in the window (but were active).
    #[serde(default, alias = "did_not", skip_serializing_if = "std::ops::Not::not")]
    pub negate: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CohortDef {
    /// `user` (identified users), `visitor` or `auto` (user id when known, default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    /// Property filters any of the person's events in the window must match, e.g. {"country": "SE", "prop:plan": "pro"}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
    /// Default look-back window for criteria and the activity window (default 30d, max 365d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
}

fn parse_secs(s: &str) -> QResult<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num.parse().map_err(|_| ApiError::bad(format!("bad duration `{s}`; use e.g. 7d, 24h, 30d")))?;
    let secs = match unit {
        "h" => n * 3600,
        "d" | "" => n * 86_400,
        "w" => n * 7 * 86_400,
        _ => return Err(ApiError::bad(format!("bad duration `{s}`; use h, d or w"))),
    };
    if !(3600..=365 * 86_400).contains(&secs) {
        return Err(ApiError::bad("cohort windows must be between 1h and 365d"));
    }
    Ok(secs)
}

impl CohortDef {
    /// Normalize step shorthands and validate.
    pub fn normalize(mut self) -> QResult<CohortDef> {
        if self.criteria.is_empty() && self.properties.is_empty() {
            return Err(ApiError::bad("a cohort needs at least one criterion or property filter"));
        }
        if self.criteria.len() > MAX_CRITERIA || self.properties.len() > MAX_PROPERTIES {
            return Err(ApiError::bad(format!("at most {MAX_CRITERIA} criteria and {MAX_PROPERTIES} property filters")));
        }
        match self.identity.as_deref().unwrap_or("auto") {
            "user" | "visitor" | "auto" => {}
            other => return Err(ApiError::bad(format!("identity must be user, visitor or auto, not `{other}`"))),
        }
        parse_secs(self.within.as_deref().unwrap_or("30d"))?;
        for c in &mut self.criteria {
            if let Some(step) = c.step.take().filter(|s| !s.trim().is_empty()) {
                let s = Step::parse(&step)?;
                c.event = s.event;
                c.filters.extend(s.filters);
            }
            if c.event.trim().is_empty() {
                return Err(ApiError::bad("each criterion needs `event` (or `step`)"));
            }
            if c.filters.len() > 4 {
                return Err(ApiError::bad("at most 4 filters per criterion"));
            }
            if let Some(w) = &c.within {
                parse_secs(w)?;
            }
            if c.min_count.unwrap_or(1) < 1 {
                return Err(ApiError::bad("min_count must be at least 1"));
            }
            for (d, v) in &c.filters {
                filter_cond(d, v, "x")?;
            }
        }
        for (d, v) in &self.properties {
            if d == "cohort" {
                return Err(ApiError::bad("cohorts can't refer to other cohorts"));
            }
            filter_cond(d, v, "x")?;
        }
        Ok(self)
    }

    pub fn actor(&self) -> (&'static str, &'static str) {
        match self.identity.as_deref().unwrap_or("auto") {
            "user" => ("user_id", "user_id != ''"),
            "visitor" => ("visitor_id", "1"),
            _ => ("if(user_id != '', user_id, visitor_id)", "1"),
        }
    }
}

/// Parameter names for cohort SQL. Separate pools so a cohort filter and a cohort breakdown can
/// appear in the same query.
pub const POOL_FILTER: &[&str] = &[
    "ck0", "ck1", "ck2", "ck3", "ck4", "ck5", "ck6", "ck7", "ck8", "ck9", "ck10", "ck11", "ck12", "ck13", "ck14", "ck15", "ck16", "ck17", "ck18", "ck19",
    "ck20", "ck21", "ck22", "ck23", "ck24", "ck25", "ck26", "ck27", "ck28", "ck29", "ck30", "ck31", "ck32", "ck33", "ck34", "ck35", "ck36", "ck37", "ck38",
    "ck39", "ck40", "ck41", "ck42", "ck43", "ck44", "ck45", "ck46", "ck47",
];
pub const POOL_BREAKDOWN: &[&str] = &[
    "cb0", "cb1", "cb2", "cb3", "cb4", "cb5", "cb6", "cb7", "cb8", "cb9", "cb10", "cb11", "cb12", "cb13", "cb14", "cb15", "cb16", "cb17", "cb18", "cb19",
    "cb20", "cb21", "cb22", "cb23", "cb24", "cb25", "cb26", "cb27", "cb28", "cb29", "cb30", "cb31", "cb32", "cb33", "cb34", "cb35", "cb36", "cb37", "cb38",
    "cb39", "cb40", "cb41", "cb42", "cb43", "cb44", "cb45", "cb46", "cb47",
];

pub struct CohortSql {
    /// `SELECT actor, last_seen, n_events FROM events ... GROUP BY actor HAVING ...`
    pub members: String,
    /// The actor expression on the `events` table for this cohort's identity.
    pub actor: &'static str,
    pub params: Vec<(&'static str, String)>,
}

impl CohortSql {
    /// SQL condition: the event's person is (or with `negate` is not) in the cohort.
    pub fn condition(&self, negate: bool) -> String {
        format!("{} {}IN (SELECT actor FROM ({}))", self.actor, if negate { "NOT " } else { "" }, self.members)
    }
}

/// Build the membership query for a definition.
pub fn cohort_sql(project_id: &str, def: &CohortDef, pool: &'static [&'static str]) -> QResult<CohortSql> {
    let mut b = Binder { names: pool.iter(), params: Vec::new() };
    let (actor, actor_ok) = def.actor();
    let default_within = parse_secs(def.within.as_deref().unwrap_or("30d"))?;
    let mut max_within = default_within;
    let mut having = Vec::new();
    for c in &def.criteria {
        let within = c.within.as_deref().map(parse_secs).transpose()?.unwrap_or(default_within);
        max_within = max_within.max(within);
        let mut conds = vec![format!("name = {{{}:String}}", b.bind(c.event.clone())?)];
        for (d, v) in &c.filters {
            conds.push(b.filter(d, v)?);
        }
        let w = b.bind(within.to_string())?;
        conds.push(format!("timestamp >= now64(3) - toIntervalSecond({{{w}:UInt32}})"));
        let cnt = format!("countIf({})", conds.join(" AND "));
        if c.negate {
            having.push(format!("{cnt} = 0"));
        } else {
            let m = b.bind(c.min_count.unwrap_or(1).to_string())?;
            having.push(format!("{cnt} >= {{{m}:UInt32}}"));
        }
    }
    if !def.properties.is_empty() {
        let mut conds = Vec::new();
        for (d, v) in &def.properties {
            conds.push(b.filter(d, v)?);
        }
        having.push(format!("countIf({}) > 0", conds.join(" AND ")));
    }
    let p = b.bind(project_id.to_string())?;
    let mw = b.bind(max_within.to_string())?;
    let members = format!(
        "SELECT {actor} AS actor, max(timestamp) AS last_seen, count() AS n_events FROM events \
         WHERE project_id = {{{p}:String}} AND timestamp >= now64(3) - toIntervalSecond({{{mw}:UInt32}}) AND {actor_ok} \
         GROUP BY actor HAVING {}",
        having.join(" AND ")
    );
    Ok(CohortSql { members, actor, params: b.params })
}

struct Binder {
    names: std::slice::Iter<'static, &'static str>,
    params: Vec<(&'static str, String)>,
}

impl Binder {
    fn name(&mut self) -> QResult<&'static str> {
        self.names.next().copied().ok_or_else(|| ApiError::bad("cohort definition is too large"))
    }
    fn bind(&mut self, v: String) -> QResult<&'static str> {
        let n = self.name()?;
        self.params.push((n, v));
        Ok(n)
    }
    fn filter(&mut self, dim: &str, val: &str) -> QResult<String> {
        let n = self.name()?;
        let (cond, value) = filter_cond(dim, val, n)?;
        self.params.push((n, value));
        Ok(cond)
    }
}

async fn load_def(state: &AppState, project_id: &str, name: &str) -> QResult<(String, CohortDef)> {
    let row = sqlx::query("SELECT id, name, definition FROM cohorts WHERE project_id = $1 AND (name = $2 OR id = $2)")
        .bind(project_id)
        .bind(name)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::bad(format!("unknown cohort `{name}` (see list_cohorts)")))?;
    let def: CohortDef = serde_json::from_str(&row.s("definition")).map_err(|e| ApiError::bad(format!("stored cohort is invalid: {e}")))?;
    Ok((row.s("name"), def))
}

/// Resolve a saved cohort by name or id into SQL.
pub async fn resolve(state: &AppState, project_id: &str, name: &str, pool: &'static [&'static str]) -> QResult<CohortSql> {
    let (_, def) = load_def(state, project_id, name).await?;
    cohort_sql(project_id, &def, pool)
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CohortIn {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(flatten)]
    pub definition: CohortDef,
}

async fn size(state: &AppState, sql: &CohortSql) -> QResult<i64> {
    let rows = state.ch.query(&format!("SELECT count() AS n FROM ({})", sql.members), &sql.params).await?;
    Ok(rows.first().and_then(|r| r["n"].as_i64()).unwrap_or(0))
}

pub async fn create_cohort(state: &AppState, p: &Principal, project: &str, c: CohortIn) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let name = c.name.trim().to_string();
    if name.is_empty() || name.len() > 100 || name.starts_with('!') {
        return Err(ApiError::bad("cohort name must be 1 to 100 characters and not start with `!`"));
    }
    let def = c.definition.normalize()?;
    let sql = cohort_sql(&proj.id, &def, POOL_FILTER)?;
    let n = size(state, &sql).await?;
    let id = crate::util::new_id();
    let res = sqlx::query(
        "INSERT INTO cohorts (id, project_id, name, description, definition, created_at, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (project_id, name) DO NOTHING",
    )
    .bind(&id)
    .bind(&proj.id)
    .bind(&name)
    .bind(c.description.unwrap_or_default())
    .bind(serde_json::to_string(&def).map_err(anyhow::Error::from)?)
    .bind(now_ms())
    .bind(p.label())
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::bad(format!("a cohort named `{name}` already exists (delete it first to redefine)")));
    }
    Ok(json!({"id": id, "project": proj.slug, "name": name, "definition": def, "size": n,
        "hint": format!("Use {{\"cohort\": \"{name}\"}} (or \"!{name}\" for everyone else) in the filters of any analytics query, or breakdown `cohort:{name}` in funnels.")}))
}

pub async fn list_cohorts(state: &AppState, p: &Principal, project: &str, with_sizes: bool) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let rows = sqlx::query("SELECT id, name, description, definition, created_at, created_by FROM cohorts WHERE project_id = $1 ORDER BY name LIMIT 200")
        .bind(&proj.id)
        .fetch_all(&state.db)
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let def: CohortDef = serde_json::from_str(&r.s("definition")).unwrap_or_default();
        let mut v = json!({"id": r.s("id"), "name": r.s("name"), "description": r.s("description"), "definition": def,
            "created_at": iso_time(r.i("created_at")), "created_by": r.s("created_by")});
        if with_sizes {
            if let Ok(sql) = cohort_sql(&proj.id, &def, POOL_FILTER) {
                v["size"] = json!(size(state, &sql).await?);
            }
        }
        out.push(v);
    }
    Ok(json!({"project": proj.slug, "cohorts": out}))
}

pub async fn get_cohort(state: &AppState, p: &Principal, project: &str, cohort: &str, limit: Option<i64>) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let (name, def) = load_def(state, &proj.id, cohort).await?;
    let sql = cohort_sql(&proj.id, &def, POOL_FILTER)?;
    let mut params = sql.params.clone();
    params.push(("lim", limit.unwrap_or(20).clamp(1, 200).to_string()));
    let members = state
        .ch
        .query(&format!("SELECT actor, toUnixTimestamp64Milli(last_seen) AS last_ms, n_events FROM ({}) ORDER BY last_seen DESC LIMIT {{lim:UInt32}}", sql.members), &params)
        .await?;
    let n = size(state, &sql).await?;
    Ok(json!({
        "project": proj.slug, "name": name, "definition": def, "size": n,
        "identity": def.identity.as_deref().unwrap_or("auto"),
        "sample_members": members.iter().map(|m| json!({"id": m["actor"], "last_seen": iso_time(m["last_ms"].as_i64().unwrap_or(0)), "events_in_window": m["n_events"]})).collect::<Vec<_>>(),
        "note": "Membership is evaluated now, over the cohort's look-back window. Ids are user ids for identified people and hashed visitor ids otherwise; get_user_timeline works for user ids.",
    }))
}

pub async fn delete_cohort(state: &AppState, p: &Principal, project: &str, cohort: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let res = sqlx::query("DELETE FROM cohorts WHERE project_id = $1 AND (id = $2 OR name = $2)").bind(&proj.id).bind(cohort).execute(&state.db).await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("cohort"));
    }
    Ok(json!({"deleted": cohort}))
}

// ---------------------------------------------------------------------------
// Stickiness and lifecycle
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Deserialize)]
pub struct EngagementParams {
    #[serde(flatten)]
    pub base: AnalyticsParams,
    /// Only count this event (step syntax, e.g. `checkout` or `pageview|path:/app`). Default: any event.
    pub event: Option<String>,
    /// user, visitor or auto (default auto).
    pub identity: Option<String>,
    /// day or week (lifecycle only, default week).
    pub period: Option<String>,
}

fn actor_for(identity: Option<&str>) -> QResult<(&'static str, &'static str)> {
    Ok(match identity.unwrap_or("auto") {
        "visitor" => ("visitor_id", "1"),
        "user" => ("user_id", "user_id != ''"),
        "auto" => ("if(user_id != '', user_id, visitor_id)", "1"),
        other => return Err(ApiError::bad(format!("identity must be visitor, user or auto, not `{other}`"))),
    })
}

/// Static names for the optional event step.
const EVENT_NAMES: &[&str] = &["ev0", "ev1", "ev2", "ev3", "ev4", "ev5"];

fn event_cond(event: Option<&str>, params: &mut Vec<(&'static str, String)>) -> QResult<String> {
    let Some(e) = event.map(str::trim).filter(|e| !e.is_empty()) else { return Ok("1".into()) };
    let step = Step::parse(e)?;
    if step.filters.len() > EVENT_NAMES.len() - 1 {
        return Err(ApiError::bad("too many event filters"));
    }
    params.push((EVENT_NAMES[0], step.event.clone()));
    let mut conds = vec![format!("name = {{{}:String}}", EVENT_NAMES[0])];
    for (i, (d, v)) in step.filters.iter().enumerate() {
        let (cond, value) = filter_cond(d, v, EVENT_NAMES[i + 1])?;
        params.push((EVENT_NAMES[i + 1], value));
        conds.push(cond);
    }
    Ok(format!("({})", conds.join(" AND ")))
}

fn f(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)
}

/// DAU / WAU / MAU, DAU/MAU, and how many days a week people are active.
pub async fn stickiness(state: &AppState, p: &Principal, mut a: EngagementParams) -> QResult<Value> {
    let proj = project_for(state, p, &a.base.project).await?;
    if a.base.since.is_none() {
        a.base.since = Some("30d".into());
    }
    let w = analytics_where(state, &proj.id, &a.base).await?;
    let (actor, actor_ok) = actor_for(a.identity.as_deref())?;
    let mut params = w.params.clone();
    let ev = event_cond(a.event.as_deref(), &mut params)?;
    let base = format!("FROM events WHERE {} AND {actor_ok} AND {ev}", w.sql);
    let totals = state
        .ch
        .query(
            &format!(
                "SELECT uniqExact({actor}) AS mau, uniqExactIf({actor}, timestamp >= least(now64(3), fromUnixTimestamp64Milli({{until:Int64}})) - INTERVAL 7 DAY) AS wau, \
                 uniqExactIf({actor}, timestamp >= least(now64(3), fromUnixTimestamp64Milli({{until:Int64}})) - INTERVAL 1 DAY) AS last_day {base}"
            ),
            &params,
        )
        .await?;
    let daily = state.ch.query(&format!("SELECT toDate(timestamp) AS day, uniqExact({actor}) AS dau {base} GROUP BY day ORDER BY day"), &params).await?;
    let weekly = state
        .ch
        .query(
            &format!(
                "SELECT days, count() AS people FROM (SELECT {actor} AS actor, toMonday(timestamp) AS wk, uniqExact(toDate(timestamp)) AS days {base} GROUP BY actor, wk) \
                 GROUP BY days ORDER BY days"
            ),
            &params,
        )
        .await?;
    let t = totals.first().cloned().unwrap_or_default();
    let avg_dau = if daily.is_empty() { 0.0 } else { daily.iter().map(|d| f(&d["dau"])).sum::<f64>() / daily.len() as f64 };
    let mau = f(&t["mau"]);
    let person_weeks: f64 = weekly.iter().map(|r| f(&r["people"])).sum();
    let r3 = |x: f64| (x * 1000.0).round() / 1000.0;
    Ok(json!({
        "project": proj.slug,
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "identity": a.identity.as_deref().unwrap_or("auto"),
        "event": a.event.as_deref().unwrap_or("any event"),
        "active_in_window": mau as i64,
        "wau": f(&t["wau"]) as i64,
        "dau_last_24h": f(&t["last_day"]) as i64,
        "avg_dau": r3(avg_dau),
        "dau_over_active": if mau > 0.0 { r3(avg_dau / mau) } else { 0.0 },
        "days_active_per_week": weekly.iter().map(|r| json!({"days": r["days"], "people_weeks": r["people"], "share": if person_weeks > 0.0 { r3(f(&r["people"]) / person_weeks) } else { 0.0 }})).collect::<Vec<_>>(),
        "daily": daily,
        "how_to_read": "active_in_window is MAU for the default 30-day window. dau_over_active (DAU/MAU) above 0.2 is good for most apps. days_active_per_week counts, per person and calendar week, on how many days they were active.",
        "note": if proj.platform == "javascript" && a.identity.as_deref() != Some("user") { "Anonymous web visitor ids rotate daily, so multi-day numbers mostly reflect identified users; use identity=user for precise numbers." } else { "" },
    }))
}

/// Per period: new, returning, resurrecting and dormant people.
pub async fn lifecycle(state: &AppState, p: &Principal, mut a: EngagementParams) -> QResult<Value> {
    let proj = project_for(state, p, &a.base.project).await?;
    let (period, bucket, interval, default_span) = match a.period.as_deref().unwrap_or("week") {
        "day" => ("day", "toDate", "toIntervalDay(1)", "14d"),
        "week" => ("week", "toMonday", "toIntervalWeek(1)", "56d"),
        other => return Err(ApiError::bad(format!("period must be day or week, not `{other}`"))),
    };
    if a.base.since.is_none() {
        a.base.since = Some(default_span.into());
    }
    let w = analytics_where(state, &proj.id, &a.base).await?;
    let (actor, actor_ok) = actor_for(a.identity.as_deref())?;
    let mut params = w.params.clone();
    let ev = event_cond(a.event.as_deref(), &mut params)?;
    // Activity includes the period before the window so the first period has a "previous".
    let act_where = w.sql.replace("timestamp >= fromUnixTimestamp64Milli({since:Int64})", &format!("timestamp >= {bucket}(fromUnixTimestamp64Milli({{since:Int64}})) - {interval}"));
    let sql = format!(
        "WITH act AS (SELECT DISTINCT {actor} AS actor, {bucket}(timestamp) AS period FROM events WHERE {act_where} AND {actor_ok} AND {ev}), \
         firsts AS (SELECT {actor} AS actor, {bucket}(min(timestamp)) AS first_period FROM events \
                    WHERE project_id = {{p:String}} AND {actor_ok} AND {ev} AND {actor} IN (SELECT actor FROM act) GROUP BY actor) \
         SELECT toString(pd) AS label, countIf(fp >= pd) AS new, countIf(fp < pd AND had_prev) AS returning, \
                countIf(fp < pd AND NOT had_prev) AS resurrecting \
         FROM (SELECT a.actor AS who, a.period AS pd, f.first_period AS fp, (a.actor, a.period - {interval}) IN (SELECT actor, period FROM act) AS had_prev \
               FROM act AS a INNER JOIN firsts AS f ON a.actor = f.actor) \
         WHERE pd >= {bucket}(fromUnixTimestamp64Milli({{since:Int64}})) GROUP BY pd ORDER BY pd"
    );
    let rows = state.ch.query(&sql, &params).await?;
    let dormant = state
        .ch
        .query(
            &format!(
                "WITH act AS (SELECT DISTINCT {actor} AS actor, {bucket}(timestamp) AS period FROM events WHERE {act_where} AND {actor_ok} AND {ev}) \
                 SELECT toString(period + {interval}) AS label, count() AS dormant FROM act \
                 WHERE (actor, period + {interval}) NOT IN (SELECT actor, period FROM act) AND period + {interval} <= {bucket}(least(now64(3), fromUnixTimestamp64Milli({{until:Int64}}))) \
                 AND period + {interval} >= {bucket}(fromUnixTimestamp64Milli({{since:Int64}})) GROUP BY label ORDER BY label"
            ),
            &params,
        )
        .await?;
    let mut by: BTreeMap<String, Value> = BTreeMap::new();
    for r in &rows {
        by.insert(r["label"].as_str().unwrap_or("").to_string(), json!({"period": r["label"], "new": r["new"], "returning": r["returning"], "resurrecting": r["resurrecting"], "dormant": 0}));
    }
    for d in &dormant {
        let k = d["label"].as_str().unwrap_or("").to_string();
        let e = by.entry(k.clone()).or_insert_with(|| json!({"period": k, "new": 0, "returning": 0, "resurrecting": 0, "dormant": 0}));
        e["dormant"] = json!(-(f(&d["dormant"]) as i64));
    }
    for v in by.values_mut() {
        let active = f(&v["new"]) + f(&v["returning"]) + f(&v["resurrecting"]);
        v["active"] = json!(active as i64);
        v["quick_ratio"] = if f(&v["dormant"]) < 0.0 { json!(((f(&v["new"]) + f(&v["resurrecting"])) / -f(&v["dormant"]) * 100.0).round() / 100.0) } else { Value::Null };
    }
    Ok(json!({
        "project": proj.slug,
        "period": period,
        "identity": a.identity.as_deref().unwrap_or("auto"),
        "event": a.event.as_deref().unwrap_or("any event"),
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "periods": by.into_values().collect::<Vec<_>>(),
        "how_to_read": "new = first activity ever in this period; returning = also active the period before; resurrecting = active before, but not the period before; dormant (negative) = active the period before, not this one. quick_ratio = (new + resurrecting) / dormant; above 1 means growing.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_membership_sql() {
        let def: CohortDef = serde_json::from_value(json!({
            "identity": "user",
            "criteria": [{"step": "purchase|prop:plan:pro", "min_count": 2, "within": "14d"}, {"event": "churn_survey", "negate": true}],
            "properties": {"country": "SE"}
        }))
        .unwrap();
        let def = def.normalize().unwrap();
        assert_eq!(def.criteria[0].event, "purchase");
        assert_eq!(def.criteria[0].filters.get("prop:plan").unwrap(), "pro");
        let sql = cohort_sql("proj", &def, POOL_FILTER).unwrap();
        assert!(sql.members.contains("GROUP BY actor HAVING countIf(name = {ck0:String} AND props['plan'] = {ck1:String}"), "{}", sql.members);
        assert!(sql.members.contains(">= {ck3:UInt32}"), "{}", sql.members);
        assert!(sql.members.contains(") = 0"), "{}", sql.members);
        assert!(sql.members.contains("countIf(country = {ck6:String}) > 0"), "{}", sql.members);
        assert!(sql.members.contains("user_id != ''"));
        let vals: Vec<&str> = sql.params.iter().map(|(_, v)| v.as_str()).collect();
        assert_eq!(vals, vec!["purchase", "pro", "1209600", "2", "churn_survey", "2592000", "SE", "proj", "2592000"]);
        assert!(sql.condition(true).starts_with("user_id NOT IN (SELECT actor FROM (SELECT user_id AS actor"));
        assert!(CohortDef::default().normalize().is_err());
        assert!(serde_json::from_value::<CohortDef>(json!({"criteria": [{"event": "x", "within": "2y"}]})).unwrap().normalize().is_err());
    }
}
