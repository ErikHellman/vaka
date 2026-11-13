//! Feature flags, flag evaluation for SDKs, release health by flag variant, and experiments
//! (variant conversion with a two-proportion z-test).

use crate::auth::{ApiError, Principal};
use crate::db::RowExt;
use crate::product::Step;
use crate::query::{project_for, QResult};
use crate::state::{AppState, ProjectInfo};
use crate::util::{iso_time, now_ms, parse_time_expr};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Variant {
    pub key: String,
    #[serde(default = "one")]
    pub weight: u32,
}

fn one() -> u32 {
    1
}

/// A targeting rule. The first rule that matches decides: `variant` forces a variant, otherwise
/// `rollout` (or the flag's rollout) applies to the people it matches.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_ids: Vec<String>,
    /// Every property must equal (or with a `!` prefix differ from) the identity's property.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Flag {
    pub id: String,
    pub key: String,
    pub description: String,
    pub enabled: bool,
    pub rollout: u32,
    pub variants: Vec<Variant>,
    pub rules: Vec<Rule>,
    /// Per-environment switch; environments not listed follow `enabled`.
    pub environments: BTreeMap<String, bool>,
    pub created_at: String,
    pub updated_at: String,
    pub created_by: String,
}

const COLS: &str = "id, key, description, enabled, rollout, variants, rules, environments, created_at, updated_at, created_by";

fn from_row(r: &sqlx::any::AnyRow) -> Flag {
    Flag {
        id: r.s("id"),
        key: r.s("key"),
        description: r.s("description"),
        enabled: r.i("enabled") != 0,
        rollout: r.i("rollout").clamp(0, 100) as u32,
        variants: serde_json::from_str(&r.s("variants")).unwrap_or_default(),
        rules: serde_json::from_str(&r.s("rules")).unwrap_or_default(),
        environments: serde_json::from_str(&r.s("environments")).unwrap_or_default(),
        created_at: iso_time(r.i("created_at")),
        updated_at: iso_time(r.i("updated_at")),
        created_by: r.s("created_by"),
    }
}

/// Position of an identity in [0, 1) for a flag: sha256(flag_key + ":" + distinct_id).
pub fn bucket(flag_key: &str, distinct_id: &str) -> f64 {
    let h = Sha256::digest(format!("{flag_key}:{distinct_id}").as_bytes());
    let n = u64::from_be_bytes(h[..8].try_into().expect("8 bytes"));
    (n >> 11) as f64 / (1u64 << 53) as f64
}

pub struct Identity<'a> {
    pub distinct_id: &'a str,
    pub user_id: Option<&'a str>,
    pub properties: &'a BTreeMap<String, String>,
    pub environment: &'a str,
}

/// The evaluated value: `None` = off, `Some("on")` for boolean flags, or the variant key.
pub fn evaluate(flag: &Flag, id: &Identity) -> (Option<String>, &'static str) {
    let enabled = flag.environments.get(id.environment).copied().unwrap_or(flag.enabled);
    if !enabled {
        return (None, "disabled");
    }
    let mut rollout = flag.rollout;
    let mut reason = "rollout";
    for rule in &flag.rules {
        let user_ok = rule.user_ids.is_empty() || id.user_id.map(|u| rule.user_ids.iter().any(|x| x == u)).unwrap_or(false) || rule.user_ids.iter().any(|x| x == id.distinct_id);
        let props_ok = rule.properties.iter().all(|(k, v)| match v.strip_prefix('!') {
            Some(neg) => id.properties.get(k).map(|p| p != neg).unwrap_or(true),
            None => id.properties.get(k) == Some(v),
        });
        if user_ok && props_ok {
            if let Some(v) = &rule.variant {
                return (Some(v.clone()), "rule");
            }
            if let Some(r) = rule.rollout {
                rollout = r.min(100);
                reason = "rule";
            }
            break;
        }
    }
    let b = bucket(&flag.key, id.distinct_id);
    let share = rollout as f64 / 100.0;
    if b >= share {
        return (None, "rollout");
    }
    if flag.variants.is_empty() {
        return (Some("on".into()), reason);
    }
    // Spread the rolled-out part over the variants by weight.
    let pos = if share > 0.0 { b / share } else { 0.0 };
    let total: u32 = flag.variants.iter().map(|v| v.weight).sum::<u32>().max(1);
    let mut acc = 0.0;
    for v in &flag.variants {
        acc += v.weight as f64 / total as f64;
        if pos < acc {
            return (Some(v.key.clone()), reason);
        }
    }
    (flag.variants.last().map(|v| v.key.clone()), reason)
}

fn flag_value(flag: &Flag, v: &Option<String>) -> Value {
    match (v, flag.variants.is_empty()) {
        (None, _) => json!(false),
        (Some(_), true) => json!(true),
        (Some(k), false) => json!(k),
    }
}

async fn flags_for(state: &AppState, project_id: &str) -> anyhow::Result<Vec<Flag>> {
    let rows = sqlx::query(&format!("SELECT {COLS} FROM flags WHERE project_id = $1 ORDER BY key")).bind(project_id).fetch_all(&state.db).await?;
    Ok(rows.iter().map(from_row).collect())
}

#[derive(Debug, Default, Deserialize)]
pub struct EvalRequest {
    #[serde(default)]
    pub distinct_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
    #[serde(default)]
    pub environment: Option<String>,
}

/// Evaluate every flag of a project for one identity (SDK endpoint).
pub async fn evaluate_all(state: &AppState, project: &ProjectInfo, req: &EvalRequest) -> anyhow::Result<Value> {
    let distinct = req.user_id.as_deref().filter(|u| !u.is_empty()).or(req.distinct_id.as_deref()).unwrap_or("");
    if distinct.is_empty() {
        anyhow::bail!("`distinct_id` or `user_id` is required");
    }
    let props: BTreeMap<String, String> = req.properties.iter().map(|(k, v)| (k.clone(), crate::models::value_to_string(v))).collect();
    let id = Identity { distinct_id: distinct, user_id: req.user_id.as_deref(), properties: &props, environment: req.environment.as_deref().unwrap_or("production") };
    let mut flags = serde_json::Map::new();
    let mut details = serde_json::Map::new();
    for f in flags_for(state, &project.id).await? {
        let (v, reason) = evaluate(&f, &id);
        flags.insert(f.key.clone(), flag_value(&f, &v));
        details.insert(f.key.clone(), json!({"variant": v, "reason": reason}));
    }
    Ok(json!({"flags": flags, "details": details}))
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FlagIn {
    pub key: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub rollout: Option<u32>,
    pub variants: Option<Vec<Variant>>,
    pub rules: Option<Vec<Rule>>,
    pub environments: Option<BTreeMap<String, bool>>,
}

fn validate(f: &FlagIn) -> QResult<()> {
    if let Some(k) = &f.key {
        if k.is_empty() || k.len() > 64 || !k.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) {
            return Err(ApiError::bad("flag key must be 1 to 64 characters of letters, digits, _, - and ."));
        }
    }
    if f.rollout.unwrap_or(0) > 100 {
        return Err(ApiError::bad("rollout is a percentage (0 to 100)"));
    }
    if let Some(vs) = &f.variants {
        if vs.len() > 10 || vs.len() == 1 {
            return Err(ApiError::bad("give no variants (a boolean flag) or 2 to 10"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for v in vs {
            if v.key.is_empty() || v.key.len() > 64 || !seen.insert(v.key.clone()) || v.weight > 10_000 {
                return Err(ApiError::bad("variant keys must be unique, 1 to 64 characters, weights 0 to 10000"));
            }
        }
        if !vs.is_empty() && vs.iter().all(|v| v.weight == 0) {
            return Err(ApiError::bad("at least one variant needs a weight above 0"));
        }
    }
    if let Some(rs) = &f.rules {
        if rs.len() > 20 {
            return Err(ApiError::bad("at most 20 rules"));
        }
        for r in rs {
            if let (Some(v), Some(vs)) = (&r.variant, &f.variants) {
                if !vs.iter().any(|x| &x.key == v) {
                    return Err(ApiError::bad(format!("rule variant `{v}` is not one of the flag's variants")));
                }
            }
            if r.rollout.unwrap_or(0) > 100 {
                return Err(ApiError::bad("rule rollout is a percentage (0 to 100)"));
            }
        }
    }
    Ok(())
}

async fn get_row(state: &AppState, project_id: &str, key: &str) -> QResult<Flag> {
    let row = sqlx::query(&format!("SELECT {COLS} FROM flags WHERE project_id = $1 AND (key = $2 OR id = $2)"))
        .bind(project_id)
        .bind(key)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("flag"))?;
    Ok(from_row(&row))
}

pub async fn list_flags(state: &AppState, p: &Principal, project: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    Ok(json!({"project": proj.slug, "flags": flags_for(state, &proj.id).await?}))
}

pub async fn get_flag(state: &AppState, p: &Principal, project: &str, key: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let f = get_row(state, &proj.id, key).await?;
    let exposure = state
        .ch
        .query(
            "SELECT flags[{k:String}] AS variant, uniq(if(user_id != '', user_id, visitor_id)) AS people, count() AS n_events FROM events \
             WHERE project_id = {p:String} AND timestamp > now64(3) - INTERVAL 7 DAY AND mapContains(flags, {k:String}) GROUP BY variant ORDER BY variant",
            &[("p", proj.id.clone()), ("k", f.key.clone())],
        )
        .await?;
    Ok(json!({"project": proj.slug, "flag": f, "exposure_last_7d": exposure,
        "hint": "flag_health compares crash-free sessions and errors per variant; experiment_results compares conversion."}))
}

pub async fn create_flag(state: &AppState, p: &Principal, project: &str, f: FlagIn) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let key = f.key.clone().ok_or_else(|| ApiError::bad("`key` is required"))?;
    validate(&f)?;
    let id = crate::util::new_id();
    let now = now_ms();
    let res = sqlx::query(
        "INSERT INTO flags (id, project_id, key, description, enabled, rollout, variants, rules, environments, created_at, updated_at, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT (project_id, key) DO NOTHING",
    )
    .bind(&id)
    .bind(&proj.id)
    .bind(&key)
    .bind(f.description.clone().unwrap_or_default())
    .bind(f.enabled.unwrap_or(true) as i64)
    .bind(f.rollout.unwrap_or(100) as i64)
    .bind(serde_json::to_string(&f.variants.clone().unwrap_or_default()).map_err(anyhow::Error::from)?)
    .bind(serde_json::to_string(&f.rules.clone().unwrap_or_default()).map_err(anyhow::Error::from)?)
    .bind(serde_json::to_string(&f.environments.clone().unwrap_or_default()).map_err(anyhow::Error::from)?)
    .bind(now)
    .bind(now)
    .bind(p.label())
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::bad(format!("a flag `{key}` already exists; use update_flag")));
    }
    crate::auth::audit(state, &p.label(), "create_flag", &id, &key).await;
    Ok(json!(get_row(state, &proj.id, &key).await?))
}

pub async fn update_flag(state: &AppState, p: &Principal, project: &str, key: &str, f: FlagIn) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let cur = get_row(state, &proj.id, key).await?;
    let merged = FlagIn {
        key: None,
        description: f.description.or(Some(cur.description)),
        enabled: f.enabled.or(Some(cur.enabled)),
        rollout: f.rollout.or(Some(cur.rollout)),
        variants: f.variants.or(Some(cur.variants)),
        rules: f.rules.or(Some(cur.rules)),
        environments: f.environments.or(Some(cur.environments)),
    };
    validate(&merged)?;
    sqlx::query("UPDATE flags SET description = $1, enabled = $2, rollout = $3, variants = $4, rules = $5, environments = $6, updated_at = $7 WHERE id = $8")
        .bind(merged.description.unwrap_or_default())
        .bind(merged.enabled.unwrap_or(true) as i64)
        .bind(merged.rollout.unwrap_or(100) as i64)
        .bind(serde_json::to_string(&merged.variants.unwrap_or_default()).map_err(anyhow::Error::from)?)
        .bind(serde_json::to_string(&merged.rules.unwrap_or_default()).map_err(anyhow::Error::from)?)
        .bind(serde_json::to_string(&merged.environments.unwrap_or_default()).map_err(anyhow::Error::from)?)
        .bind(now_ms())
        .bind(&cur.id)
        .execute(&state.db)
        .await?;
    crate::auth::audit(state, &p.label(), "update_flag", &cur.id, &cur.key).await;
    Ok(json!(get_row(state, &proj.id, &cur.key).await?))
}

pub async fn delete_flag(state: &AppState, p: &Principal, project: &str, key: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let res = sqlx::query("DELETE FROM flags WHERE project_id = $1 AND (key = $2 OR id = $2)").bind(&proj.id).bind(key).execute(&state.db).await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("flag"));
    }
    crate::auth::audit(state, &p.label(), "delete_flag", key, "").await;
    Ok(json!({"deleted": key}))
}

// ---------------------------------------------------------------------------
// Release health by flag
// ---------------------------------------------------------------------------

fn n(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)
}

fn r4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Crash-free sessions, error rate and over-represented issues per variant of a flag.
pub async fn flag_health(state: &AppState, p: &Principal, project: &str, key: &str, since: Option<&str>, release: Option<&str>) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let now = now_ms();
    let since_ms = since.and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 7 * 86_400_000);
    let mut params = vec![("p", proj.id.clone()), ("k", key.to_string()), ("since", since_ms.to_string())];
    let mut rel = String::new();
    if let Some(r) = release.filter(|r| !r.is_empty()) {
        rel = " AND release = {rel:String}".into();
        params.push(("rel", r.to_string()));
    }
    let sessions = state
        .ch
        .query(
            &format!(
                "SELECT variant, count() AS sessions, countIf(status = 'crashed') AS crashed, countIf(status IN ('crashed', 'errored', 'abnormal')) AS unhealthy, uniq(distinct_id) AS users \
                 FROM (SELECT session_id, argMax(flags[{{k:String}}], timestamp) AS variant, argMax(status, timestamp) AS status, any(distinct_id) AS distinct_id FROM sessions \
                       WHERE project_id = {{p:String}} AND timestamp >= fromUnixTimestamp64Milli({{since:Int64}}) AND mapContains(flags, {{k:String}}){rel} GROUP BY session_id) \
                 GROUP BY variant ORDER BY variant"
            ),
            &params,
        )
        .await?;
    let errors = state
        .ch
        .query(
            &format!(
                "SELECT flags[{{k:String}}] AS variant, count() AS n_errors, countIf(handled = 0) AS unhandled, uniq(issue_id) AS issues FROM errors \
                 WHERE project_id = {{p:String}} AND timestamp >= fromUnixTimestamp64Milli({{since:Int64}}) AND mapContains(flags, {{k:String}}){rel} GROUP BY variant"
            ),
            &params,
        )
        .await?;
    let by_issue = state
        .ch
        .query(
            &format!(
                "SELECT issue_id, flags[{{k:String}}] AS variant, count() AS n FROM errors \
                 WHERE project_id = {{p:String}} AND timestamp >= fromUnixTimestamp64Milli({{since:Int64}}) AND mapContains(flags, {{k:String}}){rel} \
                 GROUP BY issue_id, variant ORDER BY n DESC LIMIT 500"
            ),
            &params,
        )
        .await?;
    let mut variants: BTreeMap<String, Value> = BTreeMap::new();
    let total_sessions: f64 = sessions.iter().map(|s| n(&s["sessions"])).sum();
    for s in &sessions {
        let v = s["variant"].as_str().unwrap_or("").to_string();
        let count = n(&s["sessions"]);
        variants.insert(
            v.clone(),
            json!({"variant": v, "sessions": count as i64, "users": s["users"], "crashed_sessions": s["crashed"],
                "crash_free_sessions": if count > 0.0 { r4(1.0 - n(&s["crashed"]) / count) } else { 1.0 },
                "healthy_sessions": if count > 0.0 { r4(1.0 - n(&s["unhealthy"]) / count) } else { 1.0 },
                "session_share": if total_sessions > 0.0 { r4(count / total_sessions) } else { 0.0 }, "errors": 0, "errors_per_session": null}),
        );
    }
    for e in &errors {
        let v = e["variant"].as_str().unwrap_or("").to_string();
        let entry = variants.entry(v.clone()).or_insert_with(|| json!({"variant": v, "sessions": 0, "session_share": 0.0}));
        entry["errors"] = e["n_errors"].clone();
        entry["unhandled_errors"] = e["unhandled"].clone();
        entry["issues"] = e["issues"].clone();
        let s = n(&entry["sessions"]);
        entry["errors_per_session"] = if s > 0.0 { json!(r4(n(&e["n_errors"]) / s)) } else { Value::Null };
    }
    // Issues whose errors come from one variant much more than its share of sessions.
    let mut issue_tot: BTreeMap<String, f64> = BTreeMap::new();
    for r in &by_issue {
        *issue_tot.entry(r["issue_id"].as_str().unwrap_or("").to_string()).or_default() += n(&r["n"]);
    }
    let mut over = Vec::new();
    for r in &by_issue {
        let issue = r["issue_id"].as_str().unwrap_or("");
        let v = r["variant"].as_str().unwrap_or("");
        let tot = issue_tot.get(issue).copied().unwrap_or(0.0);
        let share = n(&variants.get(v).map(|x| x["session_share"].clone()).unwrap_or(json!(0)));
        if tot < 3.0 || share <= 0.0 {
            if tot >= 3.0 && share <= 0.0 && n(&r["n"]) == tot && total_sessions > 0.0 {
                over.push(json!({"issue_id": issue, "variant": v, "events": r["n"], "share_of_issue_events": 1.0, "session_share": 0.0, "lift": null}));
            }
            continue;
        }
        let ev_share = n(&r["n"]) / tot;
        let lift = ev_share / share;
        if lift >= 1.5 && ev_share >= 0.5 {
            over.push(json!({"issue_id": issue, "variant": v, "events": r["n"], "share_of_issue_events": r4(ev_share), "session_share": r4(share), "lift": (lift * 100.0).round() / 100.0}));
        }
    }
    over.sort_by(|a, b| n(&b["events"]).partial_cmp(&n(&a["events"])).unwrap_or(std::cmp::Ordering::Equal));
    over.truncate(10);
    for o in &mut over {
        if let Ok(Some(row)) = sqlx::query("SELECT short_id, title FROM issues WHERE id = $1").bind(o["issue_id"].as_str().unwrap_or("")).fetch_optional(&state.db).await {
            o["short_id"] = json!(row.s("short_id"));
            o["title"] = json!(row.s("title"));
        }
    }
    Ok(json!({
        "project": proj.slug, "flag": key, "since": iso_time(since_ms), "release": release,
        "variants": variants.into_values().collect::<Vec<_>>(),
        "over_represented_issues": over,
        "hint": "A variant with clearly lower crash_free_sessions or issues concentrated in it is a candidate for rollback: update_flag with rollout 0, enabled false, or a rule forcing the safe variant.",
    }))
}

// ---------------------------------------------------------------------------
// Experiments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentIn {
    pub name: String,
    pub flag: String,
    /// Goal event in step syntax (`signup`, `purchase|prop:plan:pro`).
    pub goal: String,
    pub control: Option<String>,
    pub description: Option<String>,
}

pub async fn create_experiment(state: &AppState, p: &Principal, project: &str, e: ExperimentIn) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let flag = get_row(state, &proj.id, &e.flag).await?;
    Step::parse(&e.goal)?;
    let name = e.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(ApiError::bad("experiment name must be 1 to 100 characters"));
    }
    let control = e.control.clone().or_else(|| flag.variants.first().map(|v| v.key.clone())).unwrap_or_else(|| "off".into());
    let id = crate::util::new_id();
    let res = sqlx::query(
        "INSERT INTO experiments (id, project_id, name, flag_key, goal, control, description, started_at, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (project_id, name) DO NOTHING",
    )
    .bind(&id)
    .bind(&proj.id)
    .bind(name)
    .bind(&flag.key)
    .bind(e.goal.trim())
    .bind(&control)
    .bind(e.description.unwrap_or_default())
    .bind(now_ms())
    .bind(p.label())
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::bad(format!("an experiment named `{name}` already exists")));
    }
    Ok(json!({"id": id, "name": name, "flag": flag.key, "goal": e.goal.trim(), "control": control}))
}

pub async fn list_experiments(state: &AppState, p: &Principal, project: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let rows = sqlx::query("SELECT id, name, flag_key, goal, control, description, started_at, created_by FROM experiments WHERE project_id = $1 ORDER BY started_at DESC")
        .bind(&proj.id)
        .fetch_all(&state.db)
        .await?;
    Ok(json!({"project": proj.slug, "experiments": rows.iter().map(|r| json!({"id": r.s("id"), "name": r.s("name"), "flag": r.s("flag_key"), "goal": r.s("goal"),
        "control": r.s("control"), "description": r.s("description"), "started_at": iso_time(r.i("started_at")), "created_by": r.s("created_by")})).collect::<Vec<_>>()}))
}

/// Standard normal CDF (Abramowitz-Stegun 7.1.26 erf approximation, error < 1.5e-7).
pub fn norm_cdf(z: f64) -> f64 {
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-x * x).exp();
    if z >= 0.0 {
        0.5 * (1.0 + y)
    } else {
        0.5 * (1.0 - y)
    }
}

/// Two-proportion z-test: (uplift, z, two-sided p, 95% CI of the difference).
pub fn two_proportion(c_conv: f64, c_n: f64, t_conv: f64, t_n: f64) -> Option<(f64, f64, f64, (f64, f64))> {
    if c_n <= 0.0 || t_n <= 0.0 {
        return None;
    }
    let (pc, pt) = (c_conv / c_n, t_conv / t_n);
    let pooled = (c_conv + t_conv) / (c_n + t_n);
    let se_pooled = (pooled * (1.0 - pooled) * (1.0 / c_n + 1.0 / t_n)).sqrt();
    let z = if se_pooled > 0.0 { (pt - pc) / se_pooled } else { 0.0 };
    let p = 2.0 * (1.0 - norm_cdf(z.abs()));
    let se = (pc * (1.0 - pc) / c_n + pt * (1.0 - pt) / t_n).sqrt();
    let uplift = if pc > 0.0 { (pt - pc) / pc } else { 0.0 };
    Some((uplift, z, p, (pt - pc - 1.96 * se, pt - pc + 1.96 * se)))
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ResultsParams {
    pub project: String,
    pub experiment: Option<String>,
    pub flag: Option<String>,
    pub goal: Option<String>,
    pub control: Option<String>,
    pub since: Option<String>,
    /// user, visitor or auto (default auto).
    pub identity: Option<String>,
}

/// Conversion per variant: people exposed (any event carrying the flag) and how many of them did
/// the goal event while exposed, with uplift against the control and significance.
pub async fn experiment_results(state: &AppState, p: &Principal, a: ResultsParams) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    let (flag_key, goal, control, started) = match a.experiment.as_deref().filter(|e| !e.is_empty()) {
        Some(name) => {
            let r = sqlx::query("SELECT flag_key, goal, control, started_at FROM experiments WHERE project_id = $1 AND (name = $2 OR id = $2)")
                .bind(&proj.id)
                .bind(name)
                .fetch_optional(&state.db)
                .await?
                .ok_or_else(|| ApiError::not_found("experiment"))?;
            (r.s("flag_key"), r.s("goal"), a.control.clone().unwrap_or(r.s("control")), Some(r.i("started_at")))
        }
        None => {
            let flag = a.flag.clone().ok_or_else(|| ApiError::bad("give `experiment`, or `flag` and `goal`"))?;
            let goal = a.goal.clone().ok_or_else(|| ApiError::bad("`goal` is required with `flag`"))?;
            (flag, goal, a.control.clone().unwrap_or_default(), None)
        }
    };
    let step = Step::parse(&goal)?;
    let now = now_ms();
    let since = a.since.as_deref().and_then(|s| parse_time_expr(s, now)).or(started).unwrap_or(now - 14 * 86_400_000);
    let actor = match a.identity.as_deref().unwrap_or("auto") {
        "user" => "user_id",
        "visitor" => "visitor_id",
        _ => "if(user_id != '', user_id, visitor_id)",
    };
    let mut params = vec![("p", proj.id.clone()), ("k", flag_key.clone()), ("since", since.to_string()), ("g", step.event.clone())];
    const GN: [&str; 4] = ["g0", "g1", "g2", "g3"];
    let mut goal_conds = vec!["name = {g:String}".to_string()];
    for (i, (d, v)) in step.filters.iter().enumerate() {
        let name = *GN.get(i).ok_or_else(|| ApiError::bad("at most 4 goal filters"))?;
        let (c, val) = crate::query::filter_cond(d, v, name)?;
        goal_conds.push(c);
        params.push((name, val));
    }
    let rows = state
        .ch
        .query(
            &format!(
                "SELECT flags[{{k:String}}] AS variant, uniqExact({actor}) AS exposed, uniqExactIf({actor}, {}) AS converted, countIf({}) AS conversions FROM events \
                 WHERE project_id = {{p:String}} AND timestamp >= fromUnixTimestamp64Milli({{since:Int64}}) AND mapContains(flags, {{k:String}}) AND {actor} != '' \
                 GROUP BY variant ORDER BY variant",
                goal_conds.join(" AND "),
                goal_conds.join(" AND ")
            ),
            &params,
        )
        .await?;
    let control = if control.is_empty() || !rows.iter().any(|r| r["variant"] == control.as_str()) {
        rows.iter().find(|r| matches!(r["variant"].as_str(), Some("control" | "off" | "false"))).or(rows.first()).and_then(|r| r["variant"].as_str()).unwrap_or("").to_string()
    } else {
        control
    };
    let base = rows.iter().find(|r| r["variant"] == control.as_str()).cloned().unwrap_or_default();
    let (cc, cn) = (n(&base["converted"]), n(&base["exposed"]));
    let mut variants = Vec::new();
    let mut best: Option<(String, f64)> = None;
    for r in &rows {
        let v = r["variant"].as_str().unwrap_or("").to_string();
        let (conv, exp) = (n(&r["converted"]), n(&r["exposed"]));
        let rate = if exp > 0.0 { conv / exp } else { 0.0 };
        let mut out = json!({"variant": v, "exposed": exp as i64, "converted": conv as i64, "conversions": r["conversions"], "conversion_rate": r4(rate), "is_control": v == control});
        if v != control {
            if let Some((uplift, z, pv, (lo, hi))) = two_proportion(cc, cn, conv, exp) {
                let enough = cn >= 30.0 && exp >= 30.0 && cc + conv >= 10.0;
                out["uplift"] = json!(r4(uplift));
                out["difference_ci95"] = json!([r4(lo), r4(hi)]);
                out["z"] = json!((z * 100.0).round() / 100.0);
                out["p_value"] = json!(r4(pv));
                out["significant"] = json!(enough && pv < 0.05);
                out["enough_data"] = json!(enough);
                if enough && pv < 0.05 && uplift > 0.0 && best.as_ref().map(|b| uplift > b.1).unwrap_or(true) {
                    best = Some((v.clone(), uplift));
                }
            }
        }
        variants.push(out);
    }
    let verdict = match &best {
        Some((v, u)) => format!("`{v}` converts {:.1}% better than `{control}` (significant at 95%).", u * 100.0),
        None if variants.iter().any(|v| v["significant"] == true) => format!("No variant beats `{control}`; at least one is significantly worse."),
        None if variants.iter().any(|v| v["enough_data"] == false) => "Not enough data yet (at least 30 exposed people per variant and 10 conversions).".into(),
        None => format!("No significant difference from `{control}` yet."),
    };
    Ok(json!({
        "project": proj.slug, "experiment": a.experiment, "flag": flag_key, "goal": goal, "control": control, "since": iso_time(since),
        "variants": variants, "winner": best.map(|b| b.0), "verdict": verdict,
        "method": "Exposed = distinct people with any event carrying the flag; converted = those who did the goal event while the flag was set. Two-sided two-proportion z-test against the control; 95% Wald interval for the difference in conversion rate.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(rollout: u32, variants: Vec<Variant>, rules: Vec<Rule>) -> Flag {
        Flag { id: "f".into(), key: "checkout".into(), description: String::new(), enabled: true, rollout, variants, rules, environments: BTreeMap::new(),
            created_at: String::new(), updated_at: String::new(), created_by: String::new() }
    }

    #[test]
    fn bucketing_is_stable_and_spread() {
        assert_eq!(bucket("checkout", "u1"), bucket("checkout", "u1"));
        assert_ne!(bucket("checkout", "u1"), bucket("other", "u1"));
        let on = (0..10_000).filter(|i| bucket("checkout", &format!("user-{i}")) < 0.3).count();
        assert!((2700..3300).contains(&on), "{on}");
        let props = BTreeMap::new();
        let f = flag(100, vec![Variant { key: "a".into(), weight: 1 }, Variant { key: "b".into(), weight: 3 }], vec![]);
        let b = (0..10_000)
            .filter(|i| evaluate(&f, &Identity { distinct_id: &format!("user-{i}"), user_id: None, properties: &props, environment: "production" }).0.as_deref() == Some("b"))
            .count();
        assert!((7200..7800).contains(&b), "{b}");
    }

    #[test]
    fn rules_environments_and_rollout() {
        let mut props = BTreeMap::new();
        props.insert("plan".to_string(), "pro".to_string());
        let rules = vec![Rule { user_ids: vec!["vip".into()], variant: Some("b".into()), ..Default::default() }, Rule { properties: [("plan".to_string(), "pro".to_string())].into(), rollout: Some(100), ..Default::default() }];
        let mut f = flag(0, vec![Variant { key: "a".into(), weight: 1 }, Variant { key: "b".into(), weight: 1 }], rules);
        let id = |d: &'static str, p: &'static BTreeMap<String, String>| Identity { distinct_id: d, user_id: Some(d), properties: p, environment: "production" };
        let empty: &'static BTreeMap<String, String> = Box::leak(Box::new(BTreeMap::new()));
        let pro: &'static BTreeMap<String, String> = Box::leak(Box::new(props));
        assert_eq!(evaluate(&f, &id("vip", empty)).0.as_deref(), Some("b"));
        assert_eq!(evaluate(&f, &id("someone", empty)).0, None, "rollout 0");
        assert!(evaluate(&f, &id("someone", pro)).0.is_some(), "pro users get 100%");
        f.environments.insert("production".into(), false);
        assert_eq!(evaluate(&f, &id("vip", empty)), (None, "disabled"));
        let b = flag(100, vec![], vec![]);
        assert_eq!(flag_value(&b, &evaluate(&b, &id("x", empty)).0), json!(true));
    }

    #[test]
    fn z_test() {
        // 10% vs 12% with 5000 each: z ≈ 3.2, p ≈ 0.0014.
        let (uplift, z, p, (lo, hi)) = two_proportion(500.0, 5000.0, 600.0, 5000.0).unwrap();
        assert!((uplift - 0.2).abs() < 1e-9);
        assert!((z - 3.18).abs() < 0.05, "{z}");
        assert!(p < 0.002 && p > 0.001, "{p}");
        assert!(lo > 0.0 && hi < 0.04);
        assert!((norm_cdf(1.96) - 0.975).abs() < 1e-4);
        assert!(two_proportion(0.0, 0.0, 1.0, 1.0).is_none());
    }
}
