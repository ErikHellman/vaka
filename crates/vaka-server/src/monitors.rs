//! Health monitoring: heartbeat (cron) monitors, their state machine, incidents and alerts.
//!
//! Config and state live in the metadata database (`monitors`, `monitor_incidents`); every
//! check result is also written to ClickHouse (`monitor_checks`) for uptime and history.
//! A scheduler claims due monitors with a short lease, so two Vaka instances sharing one
//! database never evaluate the same monitor twice.

use crate::auth::{ApiError, Principal};
use crate::cron::Cron;
use crate::db::RowExt;
use crate::query::{project_for, QResult};
use crate::state::{AppState, ProjectInfo};
use crate::util::{iso_time, new_id, now_ms, truncate};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Monitor kinds this build can run.
pub const KINDS: &[&str] = &["heartbeat"];
pub const CHECK_IN_STATUSES: &[&str] = &["in_progress", "ok", "error"];
const LEASE_MS: i64 = 120_000;
/// A monitor is flapping after this many state changes within `FLAP_WINDOW_MS`.
const FLAP_CHANGES: usize = 4;
const FLAP_WINDOW_MS: i64 = 30 * 60_000;
/// Flapping ends after this long without a state change.
const FLAP_SETTLE_MS: i64 = 15 * 60_000;
const DEFAULT_GRACE_S: i64 = 120;
const DEFAULT_MAX_RUNTIME_S: i64 = 1800;
const MIN_EVERY_S: i64 = 60;

static INSTANCE: Lazy<String> = Lazy::new(|| crate::util::random_token(8));

fn max_per_project() -> i64 {
    std::env::var("VAKA_MONITOR_MAX_PER_PROJECT").ok().and_then(|v| v.parse().ok()).unwrap_or(50)
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Monitor {
    pub id: String,
    pub project_id: String,
    pub slug: String,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub environment: String,
    pub config: Value,
    pub fail_threshold: i64,
    pub recover_threshold: i64,
    pub state: String,
    pub state_since: i64,
    pub flapping: bool,
    pub last_transition_at: Option<i64>,
    pub transitions: Vec<i64>,
    pub paused_until: Option<i64>,
    pub consecutive_fail: i64,
    pub consecutive_ok: i64,
    pub last_check_at: Option<i64>,
    pub last_result: String,
    pub last_error: String,
    pub next_check_at: Option<i64>,
    pub expected_at: Option<i64>,
    pub run_id: String,
    pub run_started_at: Option<i64>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLS: &str = "id, project_id, slug, name, kind, enabled, environment, config, fail_threshold, recover_threshold, state, state_since, flapping, \
    last_transition_at, transitions, paused_until, consecutive_fail, consecutive_ok, last_check_at, last_result, last_error, next_check_at, expected_at, \
    run_id, run_started_at, created_by, created_at, updated_at";

fn oi(r: &sqlx::any::AnyRow, c: &str) -> Option<i64> {
    use sqlx::Row;
    r.try_get::<Option<i64>, _>(c).ok().flatten()
}

fn from_row(r: &sqlx::any::AnyRow) -> Monitor {
    Monitor {
        id: r.s("id"),
        project_id: r.s("project_id"),
        slug: r.s("slug"),
        name: r.s("name"),
        kind: r.s("kind"),
        enabled: r.i("enabled") != 0,
        environment: r.s("environment"),
        config: serde_json::from_str(&r.s("config")).unwrap_or_else(|_| json!({})),
        fail_threshold: r.i("fail_threshold").max(1),
        recover_threshold: r.i("recover_threshold").max(1),
        state: r.s("state"),
        state_since: r.i("state_since"),
        flapping: r.i("flapping") != 0,
        last_transition_at: oi(r, "last_transition_at"),
        transitions: serde_json::from_str(&r.s("transitions")).unwrap_or_default(),
        paused_until: oi(r, "paused_until"),
        consecutive_fail: r.i("consecutive_fail"),
        consecutive_ok: r.i("consecutive_ok"),
        last_check_at: oi(r, "last_check_at"),
        last_result: r.s("last_result"),
        last_error: r.s("last_error"),
        next_check_at: oi(r, "next_check_at"),
        expected_at: oi(r, "expected_at"),
        run_id: r.s("run_id"),
        run_started_at: oi(r, "run_started_at"),
        created_by: r.s("created_by"),
        created_at: r.i("created_at"),
        updated_at: r.i("updated_at"),
    }
}

impl Monitor {
    pub fn paused(&self, now: i64) -> bool {
        self.paused_until.map(|p| p > now).unwrap_or(false)
    }

    fn heartbeat(&self) -> Result<Heartbeat, String> {
        Heartbeat::from_config(&self.config)
    }

    pub fn to_json(&self, state: &AppState) -> Value {
        let now = now_ms();
        let opt = |v: Option<i64>| v.map(iso_time);
        let mut v = json!({
            "id": self.id,
            "slug": self.slug,
            "name": self.name,
            "kind": self.kind,
            "enabled": self.enabled,
            "environment": self.environment,
            "config": self.config,
            "state": if self.paused(now) { "paused" } else { self.state.as_str() },
            "state_since": iso_time(self.state_since),
            "flapping": self.flapping,
            "paused_until": opt(self.paused_until),
            "fail_threshold": self.fail_threshold,
            "recover_threshold": self.recover_threshold,
            "consecutive_failures": self.consecutive_fail,
            "consecutive_successes": self.consecutive_ok,
            "last_check_at": opt(self.last_check_at),
            "last_result": self.last_result,
            "last_error": self.last_error,
            "created_by": self.created_by,
            "created_at": iso_time(self.created_at),
            "updated_at": iso_time(self.updated_at),
        });
        if self.kind == "heartbeat" {
            let hb = self.heartbeat().ok();
            v["schedule"] = json!(hb.as_ref().map(|h| h.describe()));
            v["next_expected_at"] = json!(opt(self.expected_at));
            v["deadline"] = json!(opt(self.expected_at.map(|e| e + hb.as_ref().map(|h| h.grace_s).unwrap_or(DEFAULT_GRACE_S) * 1000)));
            v["running_since"] = json!(if self.run_id.is_empty() { None } else { opt(self.run_started_at) });
            v["check_in_url"] = json!(format!("{}/ingest/v1/monitors/{}/check-ins", state.config.public_url, self.slug));
        }
        v
    }
}

/// When a heartbeat monitor expects to hear from its job.
#[derive(Clone, Debug, PartialEq)]
pub enum Schedule {
    Every(i64),
    Cron(Cron, String),
}

#[derive(Clone, Debug)]
pub struct Heartbeat {
    pub schedule: Schedule,
    pub grace_s: i64,
    pub max_runtime_s: i64,
}

/// Parse a duration like `90s`, `15m`, `1h`, `1d` or a plain number of seconds.
pub fn parse_duration_s(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, mult) = match s.char_indices().last()? {
        (i, 's') => (&s[..i], 1),
        (i, 'm') => (&s[..i], 60),
        (i, 'h') => (&s[..i], 3600),
        (i, 'd') => (&s[..i], 86_400),
        _ => (s, 1),
    };
    num.trim().parse::<i64>().ok().filter(|n| *n > 0).map(|n| n * mult)
}

impl Heartbeat {
    pub fn from_config(c: &Value) -> Result<Heartbeat, String> {
        let sched = c.get("schedule").ok_or("heartbeat monitors need `schedule`: {\"every\": \"1h\"} or {\"cron\": \"0 3 * * *\", \"timezone\": \"UTC\"}")?;
        let schedule = if let Some(e) = sched.get("every") {
            let secs = match e {
                Value::Number(n) => n.as_i64().filter(|n| *n > 0),
                Value::String(s) => parse_duration_s(s),
                _ => None,
            }
            .ok_or("schedule.every must be a duration like \"15m\", \"1h\" or a number of seconds")?;
            if secs < MIN_EVERY_S {
                return Err(format!("schedule.every must be at least {MIN_EVERY_S} seconds"));
            }
            Schedule::Every(secs)
        } else if let Some(expr) = sched.get("cron").and_then(|v| v.as_str()) {
            let cron = Cron::parse(expr)?;
            let tz = sched.get("timezone").and_then(|v| v.as_str()).unwrap_or("UTC").to_string();
            jiff::tz::TimeZone::get(&tz).map_err(|_| format!("unknown time zone `{tz}`; use an IANA name like Europe/Stockholm"))?;
            Schedule::Cron(cron, tz)
        } else {
            return Err("schedule needs `every` or `cron`".into());
        };
        let secs = |k: &str, default: i64| -> Result<i64, String> {
            match c.get(k) {
                None | Some(Value::Null) => Ok(default),
                Some(Value::Number(n)) => n.as_i64().filter(|n| *n >= 0).ok_or(format!("{k} must be a number of seconds")),
                Some(Value::String(s)) => parse_duration_s(s).ok_or(format!("{k} must be a duration like \"5m\"")),
                _ => Err(format!("{k} must be a number of seconds")),
            }
        };
        let grace_s = secs("grace_s", DEFAULT_GRACE_S)?;
        let max_runtime_s = secs("max_runtime_s", DEFAULT_MAX_RUNTIME_S)?;
        if grace_s > 86_400 * 7 || max_runtime_s > 86_400 * 7 {
            return Err("grace_s and max_runtime_s must be at most 7 days".into());
        }
        Ok(Heartbeat { schedule, grace_s, max_runtime_s })
    }

    /// The first expected run strictly after `t`.
    pub fn next_after(&self, t: i64) -> i64 {
        match &self.schedule {
            Schedule::Every(s) => t + s * 1000,
            Schedule::Cron(c, tz) => {
                let tz = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
                c.next_after(t, &tz).unwrap_or(t + 366 * 86_400_000)
            }
        }
    }

    /// The next expected run after a check-in at `now`, given the run that was expected.
    /// A job that reports just before or around its expected time covers that run.
    pub fn after_check_in(&self, now: i64, expected: Option<i64>) -> i64 {
        match (&self.schedule, expected) {
            (Schedule::Every(_), _) => self.next_after(now),
            (Schedule::Cron(..), Some(e)) if (e - now).abs() <= self.grace_s * 1000 || (e > now && e - now <= 60_000) => self.next_after(e.max(now)),
            _ => self.next_after(now),
        }
    }

    pub fn describe(&self) -> String {
        match &self.schedule {
            Schedule::Every(s) => format!("every {}", human_secs(*s)),
            Schedule::Cron(_, tz) => format!("cron in {tz}"),
        }
    }
}

fn human_secs(s: i64) -> String {
    if s % 86_400 == 0 {
        format!("{}d", s / 86_400)
    } else if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// When the scheduler should next look at a heartbeat monitor: the deadline of the expected
/// run, or earlier when a running job would exceed its maximum runtime.
fn heartbeat_next_check(hb: &Heartbeat, expected: Option<i64>, run_started: Option<i64>) -> Option<i64> {
    let deadline = expected.map(|e| e + hb.grace_s * 1000);
    let timeout = match (run_started, hb.max_runtime_s) {
        (Some(s), m) if m > 0 => Some(s + m * 1000),
        _ => None,
    };
    match (deadline, timeout) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

// ---------------------------------------------------------------------------
// Check results and the state machine
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CheckRow {
    project_id: String,
    monitor_id: String,
    timestamp: String,
    result: String,
    duration_ms: u32,
    http_status: u16,
    error: String,
    tls_days_left: i32,
    check_in_id: String,
    release: String,
    environment: String,
    trace_id: String,
    retention_days: u16,
}

#[derive(Default, Clone, Debug)]
pub struct CheckResult {
    /// ok, fail, timeout, missed, error, in_progress or inconclusive.
    pub result: String,
    pub duration_ms: i64,
    pub http_status: i64,
    pub error: String,
    pub check_in_id: String,
    pub release: String,
    pub environment: String,
    pub trace_id: String,
}

impl CheckResult {
    fn new(result: &str) -> Self {
        CheckResult { result: result.into(), ..Default::default() }
    }

    pub fn is_ok(&self) -> bool {
        self.result == "ok"
    }

    pub fn is_failure(&self) -> bool {
        matches!(self.result.as_str(), "fail" | "timeout" | "missed" | "error")
    }
}

async fn record_check(state: &AppState, m: &Monitor, c: &CheckResult, at: i64) {
    let retention = crate::projects::find(state, &m.project_id).await.ok().flatten().map(|p| p.retention_days).unwrap_or(90);
    let row = CheckRow {
        project_id: m.project_id.clone(),
        monitor_id: m.id.clone(),
        timestamp: crate::util::ch_time(at),
        result: c.result.clone(),
        duration_ms: c.duration_ms.clamp(0, u32::MAX as i64) as u32,
        http_status: c.http_status.clamp(0, 999) as u16,
        error: truncate(&c.error, 500),
        tls_days_left: 0,
        check_in_id: c.check_in_id.clone(),
        release: c.release.clone(),
        environment: if c.environment.is_empty() { m.environment.clone() } else { c.environment.clone() },
        trace_id: c.trace_id.clone(),
        retention_days: retention.clamp(1, u16::MAX as i64) as u16,
    };
    if let Err(e) = state.ch.insert("monitor_checks", &[row]).await {
        tracing::warn!("recording monitor check failed: {e:#}");
    }
}

enum Transition {
    Down,
    Up,
}

/// Apply a finished check to a monitor: counters, state, incidents and alerts. `m` holds the
/// monitor as loaded; the schedule fields (`next_check_at`, `expected_at`) are written by the
/// caller in `sched`.
async fn apply(state: &AppState, mut m: Monitor, c: &CheckResult, now: i64, sched: (Option<i64>, Option<i64>)) -> anyhow::Result<Monitor> {
    record_check(state, &m, c, now).await;
    let mut transition = None;
    if c.is_ok() {
        m.consecutive_ok += 1;
        m.consecutive_fail = 0;
        if m.state == "unknown" || (m.state == "down" && m.consecutive_ok >= m.recover_threshold) {
            if m.state == "down" {
                transition = Some(Transition::Up);
            }
            m.state = "up".into();
            m.state_since = now;
        }
        m.last_error.clear();
    } else if c.is_failure() {
        m.consecutive_fail += 1;
        m.consecutive_ok = 0;
        m.last_error = truncate(&c.error, 500);
        if m.state != "down" && m.consecutive_fail >= m.fail_threshold {
            transition = Some(Transition::Down);
            m.state = "down".into();
            m.state_since = now;
        }
    }
    let was_flapping = m.flapping;
    if transition.is_some() {
        m.transitions.retain(|t| now - t < FLAP_WINDOW_MS);
        m.transitions.push(now);
        m.last_transition_at = Some(now);
        if !m.flapping && m.transitions.len() >= FLAP_CHANGES {
            m.flapping = true;
        }
    }
    if c.result != "in_progress" {
        m.last_check_at = Some(now);
        m.last_result = c.result.clone();
    }
    m.next_check_at = sched.0;
    m.expected_at = sched.1;
    m.updated_at = now;
    sqlx::query(
        "UPDATE monitors SET state = $1, state_since = $2, flapping = $3, last_transition_at = $4, transitions = $5, consecutive_fail = $6, consecutive_ok = $7, \
         last_check_at = $8, last_result = $9, last_error = $10, next_check_at = $11, expected_at = $12, run_id = $13, run_started_at = $14, updated_at = $15 WHERE id = $16",
    )
    .bind(&m.state)
    .bind(m.state_since)
    .bind(m.flapping as i64)
    .bind(m.last_transition_at)
    .bind(serde_json::to_string(&m.transitions)?)
    .bind(m.consecutive_fail)
    .bind(m.consecutive_ok)
    .bind(m.last_check_at)
    .bind(&m.last_result)
    .bind(&m.last_error)
    .bind(m.next_check_at)
    .bind(m.expected_at)
    .bind(&m.run_id)
    .bind(m.run_started_at)
    .bind(now)
    .bind(&m.id)
    .execute(&state.db)
    .await?;

    match transition {
        Some(Transition::Down) => {
            let incident = new_id();
            sqlx::query("INSERT INTO monitor_incidents (id, monitor_id, project_id, kind, started_at, first_error, failed_checks) VALUES ($1, $2, $3, 'down', $4, $5, $6)")
                .bind(&incident)
                .bind(&m.id)
                .bind(&m.project_id)
                .bind(now)
                .bind(truncate(&c.error, 500))
                .bind(m.consecutive_fail)
                .execute(&state.db)
                .await?;
            if !was_flapping {
                notify(state, &m, "monitor_down", &incident, json!({"flapping": m.flapping})).await;
            }
        }
        Some(Transition::Up) => {
            let open = open_incident(state, &m.id).await?;
            if let Some((id, started)) = &open {
                sqlx::query("UPDATE monitor_incidents SET ended_at = $1 WHERE id = $2").bind(now).bind(id).execute(&state.db).await?;
                if !was_flapping {
                    notify(state, &m, "monitor_up", id, json!({"downtime_ms": now - started, "flapping": m.flapping})).await;
                }
            }
        }
        None if c.is_failure() && m.state == "down" => {
            sqlx::query("UPDATE monitor_incidents SET failed_checks = failed_checks + 1 WHERE monitor_id = $1 AND ended_at IS NULL").bind(&m.id).execute(&state.db).await?;
        }
        None => {}
    }
    Ok(m)
}

async fn open_incident(state: &AppState, monitor_id: &str) -> anyhow::Result<Option<(String, i64)>> {
    Ok(sqlx::query("SELECT id, started_at FROM monitor_incidents WHERE monitor_id = $1 AND kind = 'down' AND ended_at IS NULL ORDER BY started_at DESC LIMIT 1")
        .bind(monitor_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| (r.s("id"), r.i("started_at"))))
}

async fn incident_json(state: &AppState, id: &str) -> Option<Value> {
    let r = sqlx::query("SELECT id, monitor_id, kind, started_at, ended_at, first_error, failed_checks, context, acknowledged_by, acknowledged_at FROM monitor_incidents WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .ok()??;
    Some(incident_from_row(&r))
}

fn incident_from_row(r: &sqlx::any::AnyRow) -> Value {
    let started = r.i("started_at");
    let ended = oi(r, "ended_at");
    json!({
        "id": r.s("id"),
        "monitor_id": r.s("monitor_id"),
        "kind": r.s("kind"),
        "started_at": iso_time(started),
        "ended_at": ended.map(iso_time),
        "duration_ms": ended.unwrap_or_else(now_ms) - started,
        "open": ended.is_none(),
        "first_error": r.s("first_error"),
        "failed_checks": r.i("failed_checks"),
        "context": serde_json::from_str::<Value>(&r.s("context")).unwrap_or(json!({})),
        "acknowledged_by": Some(r.s("acknowledged_by")).filter(|s| !s.is_empty()),
        "acknowledged_at": oi(r, "acknowledged_at").map(iso_time),
    })
}

/// Build the alert payload for a monitor event and hand it to the alert rules.
async fn notify(state: &AppState, m: &Monitor, trigger: &str, incident_id: &str, details: Value) {
    let Ok(Some(project)) = crate::projects::find(state, &m.project_id).await else { return };
    let incident = incident_json(state, incident_id).await.unwrap_or(Value::Null);
    let payload = json!({
        "trigger": trigger,
        "project": {"id": project.id, "slug": project.slug, "name": project.name},
        "monitor": {"id": m.id, "slug": m.slug, "name": m.name, "kind": m.kind, "environment": m.environment, "schedule": m.heartbeat().ok().map(|h| h.describe()), "last_error": m.last_error},
        "incident": incident,
        "details": details,
        "url": format!("{}/ui/projects/{}/monitors/{}", state.config.public_url, project.slug, m.slug),
        "api_url": format!("{}/api/v1/projects/{}/monitors/{}", state.config.public_url, project.slug, m.slug),
        "sent_at": iso_time(now_ms()),
    });
    if let Err(e) = crate::alerts::notify_monitor(state, &project, m, trigger, incident_id, payload).await {
        tracing::warn!("monitor alert failed: {e:#}");
    }
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            if crate::readonly::requested() {
                continue; // metadata database migration in progress
            }
            if let Err(e) = run_due(&state).await {
                tracing::warn!("monitor scheduler failed: {e:#}");
            }
        }
    });
}

/// Claim a monitor for evaluation. Returns false if another instance holds it.
async fn claim(state: &AppState, id: &str, now: i64) -> anyhow::Result<bool> {
    let res = sqlx::query("UPDATE monitors SET lease_owner = $1, lease_until = $2 WHERE id = $3 AND (lease_until IS NULL OR lease_until < $4)")
        .bind(INSTANCE.as_str())
        .bind(now + LEASE_MS)
        .bind(id)
        .bind(now)
        .execute(&state.db)
        .await?;
    Ok(res.rows_affected() == 1)
}

async fn release(state: &AppState, id: &str) {
    let _ = sqlx::query("UPDATE monitors SET lease_owner = '', lease_until = NULL WHERE id = $1 AND lease_owner = $2").bind(id).bind(INSTANCE.as_str()).execute(&state.db).await;
}

async fn load(state: &AppState, id: &str) -> anyhow::Result<Option<Monitor>> {
    Ok(sqlx::query(&format!("SELECT {COLS} FROM monitors WHERE id = $1")).bind(id).fetch_optional(&state.db).await?.map(|r| from_row(&r)))
}

pub async fn run_due(state: &AppState) -> anyhow::Result<()> {
    let now = now_ms();
    // Pauses that ran out: start the schedule again from now.
    let resumed = sqlx::query(&format!("SELECT {COLS} FROM monitors WHERE paused_until IS NOT NULL AND paused_until <= $1")).bind(now).fetch_all(&state.db).await?;
    for r in resumed {
        let m = from_row(&r);
        resume(state, &m, now).await?;
    }
    let due = sqlx::query(
        "SELECT id FROM monitors WHERE enabled = 1 AND next_check_at IS NOT NULL AND next_check_at <= $1 AND paused_until IS NULL \
         AND (lease_until IS NULL OR lease_until < $1) ORDER BY next_check_at LIMIT 200",
    )
    .bind(now)
    .fetch_all(&state.db)
    .await?;
    for r in due {
        let id = r.s("id");
        if !claim(state, &id, now).await? {
            continue;
        }
        let res = match load(state, &id).await? {
            Some(m) if m.kind == "heartbeat" => evaluate_heartbeat(state, m, now).await.map(|_| ()),
            _ => Ok(()),
        };
        release(state, &id).await;
        res?;
    }
    settle_flapping(state, now).await
}

/// A heartbeat monitor reached its deadline: the job hung past its maximum runtime, or the
/// expected check-in never came.
async fn evaluate_heartbeat(state: &AppState, mut m: Monitor, now: i64) -> anyhow::Result<Monitor> {
    let Ok(hb) = m.heartbeat() else {
        sqlx::query("UPDATE monitors SET next_check_at = NULL WHERE id = $1").bind(&m.id).execute(&state.db).await?;
        return Ok(m);
    };
    if let Some(started) = m.run_started_at.filter(|_| !m.run_id.is_empty()) {
        if hb.max_runtime_s > 0 && now >= started + hb.max_runtime_s * 1000 {
            let mut c = CheckResult::new("timeout");
            c.check_in_id = m.run_id.clone();
            c.duration_ms = now - started;
            c.error = format!("the run started at {} did not finish within {}", iso_time(started), human_secs(hb.max_runtime_s));
            m.run_id.clear();
            m.run_started_at = None;
            let next = heartbeat_next_check(&hb, m.expected_at, None);
            return apply(state, m.clone(), &c, now, (next, m.expected_at)).await;
        }
    }
    let Some(expected) = m.expected_at else {
        let e = hb.next_after(now);
        let next = heartbeat_next_check(&hb, Some(e), m.run_started_at);
        sqlx::query("UPDATE monitors SET expected_at = $1, next_check_at = $2 WHERE id = $3").bind(e).bind(next).bind(&m.id).execute(&state.db).await?;
        return Ok(m);
    };
    if now < expected + hb.grace_s * 1000 {
        // Woken for a run timeout that is not due yet.
        let next = heartbeat_next_check(&hb, Some(expected), m.run_started_at.filter(|_| !m.run_id.is_empty()));
        sqlx::query("UPDATE monitors SET next_check_at = $1 WHERE id = $2").bind(next).bind(&m.id).execute(&state.db).await?;
        return Ok(m);
    }
    // Missed. Skip ahead past every run whose deadline has also passed (after downtime of
    // Vaka itself), recording a single miss.
    let mut e = expected;
    for _ in 0..10_000 {
        e = hb.next_after(e);
        if e + hb.grace_s * 1000 > now {
            break;
        }
    }
    let mut c = CheckResult::new("missed");
    c.error = format!("no check-in for the run expected at {} (grace {})", iso_time(expected), human_secs(hb.grace_s.max(1)));
    let next = heartbeat_next_check(&hb, Some(e), m.run_started_at.filter(|_| !m.run_id.is_empty()));
    let m2 = m.clone();
    m = apply(state, m2, &c, now, (next, Some(e))).await?;
    Ok(m)
}

/// End flapping for monitors that have been stable for `FLAP_SETTLE_MS`, alerting once with
/// the state they settled in.
async fn settle_flapping(state: &AppState, now: i64) -> anyhow::Result<()> {
    let rows = sqlx::query(&format!("SELECT {COLS} FROM monitors WHERE flapping = 1 AND (last_transition_at IS NULL OR last_transition_at < $1)"))
        .bind(now - FLAP_SETTLE_MS)
        .fetch_all(&state.db)
        .await?;
    for r in rows {
        let m = from_row(&r);
        let res = sqlx::query("UPDATE monitors SET flapping = 0, transitions = '[]' WHERE id = $1 AND flapping = 1").bind(&m.id).execute(&state.db).await?;
        if res.rows_affected() == 0 {
            continue;
        }
        let changes = m.transitions.len();
        if m.state == "down" {
            if let Some((id, _)) = open_incident(state, &m.id).await? {
                notify(state, &m, "monitor_down", &id, json!({"settled_after_flapping": true, "state_changes": changes})).await;
            }
        } else if m.state == "up" {
            let last = sqlx::query("SELECT id FROM monitor_incidents WHERE monitor_id = $1 ORDER BY started_at DESC LIMIT 1").bind(&m.id).fetch_optional(&state.db).await?;
            if let Some(l) = last {
                notify(state, &m, "monitor_up", &l.s("id"), json!({"settled_after_flapping": true, "state_changes": changes})).await;
            }
        }
    }
    Ok(())
}

async fn resume(state: &AppState, m: &Monitor, now: i64) -> anyhow::Result<()> {
    let (next, expected) = match m.heartbeat() {
        Ok(hb) if m.kind == "heartbeat" => {
            let e = hb.next_after(now);
            (heartbeat_next_check(&hb, Some(e), None), Some(e))
        }
        _ => (Some(now), None),
    };
    sqlx::query(
        "UPDATE monitors SET paused_until = NULL, state = 'unknown', state_since = $1, consecutive_fail = 0, consecutive_ok = 0, flapping = 0, transitions = '[]', \
         next_check_at = $2, expected_at = $3, run_id = '', run_started_at = NULL, updated_at = $1 WHERE id = $4",
    )
    .bind(now)
    .bind(next)
    .bind(expected)
    .bind(&m.id)
    .execute(&state.db)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Check-ins (ingest)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct CheckIn {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub check_in_id: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Create the monitor on first check-in: {name?, schedule, grace_s?, max_runtime_s?,
    /// environment?, fail_threshold?, recover_threshold?}.
    #[serde(default)]
    pub monitor: Option<Value>,
}

/// Per-monitor check-in rate limit (one per second), keyed by project and slug.
static LAST_CHECK_IN: Lazy<Mutex<HashMap<String, (i64, String)>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn rate_limited(key: &str, status: &str, now: i64) -> bool {
    let mut map = LAST_CHECK_IN.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() > 100_000 {
        map.retain(|_, (t, _)| now - *t < 60_000);
    }
    // A start and its finish within the same second are both allowed.
    if let Some((t, s)) = map.get(key) {
        if now - t < 1000 && s == status {
            return true;
        }
    }
    map.insert(key.to_string(), (now, status.to_string()));
    false
}

pub enum CheckInError {
    NotFound(String),
    Bad(String),
    RateLimited,
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for CheckInError {
    fn from(e: anyhow::Error) -> Self {
        CheckInError::Internal(e)
    }
}

impl From<sqlx::Error> for CheckInError {
    fn from(e: sqlx::Error) -> Self {
        CheckInError::Internal(e.into())
    }
}

pub async fn check_in(state: &AppState, project: &ProjectInfo, slug: &str, ci: CheckIn) -> Result<Value, CheckInError> {
    let now = now_ms();
    let status = ci.status.clone().unwrap_or_else(|| "ok".into());
    if !CHECK_IN_STATUSES.contains(&status.as_str()) {
        return Err(CheckInError::Bad(format!("status must be one of {}", CHECK_IN_STATUSES.join(", "))));
    }
    if rate_limited(&format!("{}:{slug}", project.id), &status, now) {
        return Err(CheckInError::RateLimited);
    }
    let row = sqlx::query(&format!("SELECT {COLS} FROM monitors WHERE project_id = $1 AND slug = $2")).bind(&project.id).bind(slug).fetch_optional(&state.db).await?;
    let m = match row {
        Some(r) => from_row(&r),
        None => {
            let Some(cfg) = ci.monitor.clone() else {
                return Err(CheckInError::NotFound(format!(
                    "no monitor `{slug}` in this project: create it first, or send `monitor` with a schedule in the check-in to create it"
                )));
            };
            let input = MonitorIn {
                slug: Some(slug.to_string()),
                name: cfg.get("name").and_then(|v| v.as_str()).map(String::from),
                kind: Some("heartbeat".into()),
                environment: cfg.get("environment").and_then(|v| v.as_str()).map(String::from),
                fail_threshold: cfg.get("fail_threshold").and_then(|v| v.as_i64()),
                recover_threshold: cfg.get("recover_threshold").and_then(|v| v.as_i64()),
                config: Some(json!({"schedule": cfg.get("schedule"), "grace_s": cfg.get("grace_s"), "max_runtime_s": cfg.get("max_runtime_s")})),
                ..Default::default()
            };
            match insert_monitor(state, project, input, "check-in").await {
                Ok(m) => m,
                Err(e) if e.0 == axum::http::StatusCode::BAD_REQUEST => return Err(CheckInError::Bad(e.1)),
                Err(e) => return Err(CheckInError::Internal(anyhow::anyhow!(e.1))),
            }
        }
    };
    if m.kind != "heartbeat" {
        return Err(CheckInError::Bad(format!("`{slug}` is a {} monitor; only heartbeat monitors take check-ins", m.kind)));
    }
    let paused = m.paused(now) || !m.enabled;
    let hb = m.heartbeat().map_err(CheckInError::Bad)?;
    let mut c = CheckResult::new(&status);
    c.check_in_id = ci.check_in_id.clone().map(|s| truncate(&s, 64)).unwrap_or_default();
    c.release = ci.release.clone().map(|s| truncate(&s, 200)).unwrap_or_default();
    c.environment = ci.environment.clone().map(|s| truncate(&s, 64)).unwrap_or_default();
    c.error = ci.message.clone().map(|s| truncate(&s, 500)).unwrap_or_default();
    if paused {
        record_check(state, &m, &c, now).await;
        return Ok(json!({"ok": true, "monitor": m.slug, "state": "paused"}));
    }
    // Claim the monitor so a check-in and the scheduler never race on the same row. A
    // check-in that loses retries briefly, then proceeds: losing a check-in is worse.
    let mut claimed = false;
    for _ in 0..20 {
        if claim(state, &m.id, now_ms()).await? {
            claimed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let result = check_in_locked(state, &m.id, &hb, c, ci.duration_ms, now).await;
    if claimed {
        release(state, &m.id).await;
    }
    let m = result?;
    Ok(json!({"ok": true, "monitor": m.slug, "state": m.state, "next_expected_at": m.expected_at.map(iso_time)}))
}

async fn check_in_locked(state: &AppState, id: &str, hb: &Heartbeat, mut c: CheckResult, duration_ms: Option<i64>, now: i64) -> Result<Monitor, CheckInError> {
    let Some(mut m) = load(state, id).await? else { return Err(CheckInError::NotFound("monitor deleted".into())) };
    if c.result == "in_progress" {
        let expected = hb.after_check_in(now, m.expected_at);
        m.run_id = if c.check_in_id.is_empty() { new_id() } else { c.check_in_id.clone() };
        m.run_started_at = Some(now);
        c.check_in_id = m.run_id.clone();
        let next = heartbeat_next_check(hb, Some(expected), Some(now));
        return Ok(apply(state, m, &c, now, (next, Some(expected))).await?);
    }
    // A finish for a run that was announced with in_progress keeps that run's expected slot.
    let finishes_run = !m.run_id.is_empty() && (c.check_in_id.is_empty() || c.check_in_id == m.run_id);
    if finishes_run {
        let started = m.run_started_at.unwrap_or(now);
        c.duration_ms = duration_ms.unwrap_or(now - started);
        c.check_in_id = m.run_id.clone();
        m.run_id.clear();
        m.run_started_at = None;
        let next = heartbeat_next_check(hb, m.expected_at, None);
        let expected = m.expected_at;
        return Ok(apply(state, m, &c, now, (next, expected)).await?);
    }
    c.duration_ms = duration_ms.unwrap_or(0);
    let expected = hb.after_check_in(now, m.expected_at);
    let running = m.run_started_at.filter(|_| !m.run_id.is_empty());
    let next = heartbeat_next_check(hb, Some(expected), running);
    Ok(apply(state, m, &c, now, (next, Some(expected))).await?)
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MonitorIn {
    pub slug: Option<String>,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub enabled: Option<bool>,
    pub environment: Option<String>,
    pub config: Option<Value>,
    pub fail_threshold: Option<i64>,
    pub recover_threshold: Option<i64>,
    /// RFC 3339 time or relative (`2h`) to pause until; `"forever"` pauses indefinitely;
    /// null resumes.
    #[serde(default, deserialize_with = "double_option")]
    pub paused_until: Option<Option<String>>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    truncate(out.trim_end_matches('-'), 64)
}

fn parse_pause(v: &str, now: i64) -> QResult<i64> {
    if v == "forever" {
        return Ok(i64::MAX / 2);
    }
    if let Some(s) = parse_duration_s(v) {
        return Ok(now + s * 1000);
    }
    crate::util::parse_time_expr(v, now).filter(|t| *t > now).ok_or_else(|| ApiError::bad("paused_until must be a future RFC 3339 time, a duration like \"2h\", or \"forever\""))
}

fn validate(kind: &str, input: &MonitorIn) -> QResult<Value> {
    if !KINDS.contains(&kind) {
        return Err(ApiError::bad(format!("kind must be one of {}", KINDS.join(", "))));
    }
    for (k, v) in [("fail_threshold", input.fail_threshold), ("recover_threshold", input.recover_threshold)] {
        if let Some(v) = v {
            if !(1..=10).contains(&v) {
                return Err(ApiError::bad(format!("{k} must be 1 to 10")));
            }
        }
    }
    if let Some(n) = &input.name {
        if n.trim().is_empty() || n.len() > 200 {
            return Err(ApiError::bad("name must be 1 to 200 characters"));
        }
    }
    let config = input.config.clone().unwrap_or_else(|| json!({}));
    if !config.is_object() {
        return Err(ApiError::bad("config must be an object"));
    }
    if kind == "heartbeat" {
        Heartbeat::from_config(&config).map_err(ApiError::bad)?;
    }
    // Keep only known keys so stored config stays predictable.
    let keep: &[&str] = match kind {
        "heartbeat" => &["schedule", "grace_s", "max_runtime_s"],
        _ => &[],
    };
    let clean: serde_json::Map<String, Value> = config.as_object().unwrap().iter().filter(|(k, v)| keep.contains(&k.as_str()) && !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect();
    Ok(Value::Object(clean))
}

async fn insert_monitor(state: &AppState, project: &ProjectInfo, input: MonitorIn, actor: &str) -> QResult<Monitor> {
    let kind = input.kind.clone().unwrap_or_else(|| "heartbeat".into());
    let config = validate(&kind, &input)?;
    let slug = match (&input.slug, &input.name) {
        (Some(s), _) => s.clone(),
        (None, Some(n)) => slugify(n),
        (None, None) => return Err(ApiError::bad("`slug` or `name` is required")),
    };
    if !valid_slug(&slug) {
        return Err(ApiError::bad("slug must be 1 to 64 characters of lowercase letters, digits, - and _"));
    }
    let count = sqlx::query("SELECT COUNT(*) AS n FROM monitors WHERE project_id = $1").bind(&project.id).fetch_one(&state.db).await?.i("n");
    if count >= max_per_project() {
        return Err(ApiError::bad(format!("this project already has {count} monitors, the limit (VAKA_MONITOR_MAX_PER_PROJECT)")));
    }
    let now = now_ms();
    let (next, expected) = if kind == "heartbeat" {
        let hb = Heartbeat::from_config(&config).map_err(ApiError::bad)?;
        let e = hb.next_after(now);
        (heartbeat_next_check(&hb, Some(e), None), Some(e))
    } else {
        (Some(now), None)
    };
    let paused = match input.paused_until.clone().flatten() {
        Some(p) => Some(parse_pause(&p, now)?),
        None => None,
    };
    let id = new_id();
    let default_threshold = 1;
    let res = sqlx::query(
        "INSERT INTO monitors (id, project_id, slug, name, kind, enabled, environment, config, fail_threshold, recover_threshold, state, state_since, \
         next_check_at, expected_at, paused_until, created_by, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'unknown', $11, $12, $13, $14, $15, $11, $11) ON CONFLICT (project_id, slug) DO NOTHING",
    )
    .bind(&id)
    .bind(&project.id)
    .bind(&slug)
    .bind(input.name.clone().unwrap_or_else(|| slug.clone()))
    .bind(&kind)
    .bind(input.enabled.unwrap_or(true) as i64)
    .bind(input.environment.clone().unwrap_or_default())
    .bind(config.to_string())
    .bind(input.fail_threshold.unwrap_or(default_threshold))
    .bind(input.recover_threshold.unwrap_or(default_threshold))
    .bind(now)
    .bind(next)
    .bind(expected)
    .bind(paused)
    .bind(actor)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::bad(format!("a monitor `{slug}` already exists in this project; use update_monitor")));
    }
    crate::auth::audit(state, actor, "create_monitor", &id, &slug).await;
    load(state, &id).await?.ok_or_else(|| ApiError::not_found("monitor"))
}

async fn get_row(state: &AppState, project_id: &str, slug: &str) -> QResult<Monitor> {
    let row = sqlx::query(&format!("SELECT {COLS} FROM monitors WHERE project_id = $1 AND (slug = $2 OR id = $2)"))
        .bind(project_id)
        .bind(slug)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("monitor"))?;
    Ok(from_row(&row))
}

/// Uptime and run counts per monitor over 24 h, 7 d and 30 d.
async fn uptime(state: &AppState, project_id: &str) -> HashMap<String, Value> {
    let rows = state
        .ch
        .query(
            "SELECT monitor_id, \
               countIf(result = 'ok' AND timestamp > now64(3) - INTERVAL 1 DAY) AS ok_1d, countIf(result IN ('ok','fail','timeout','missed','error') AND timestamp > now64(3) - INTERVAL 1 DAY) AS n_1d, \
               countIf(result = 'ok' AND timestamp > now64(3) - INTERVAL 7 DAY) AS ok_7d, countIf(result IN ('ok','fail','timeout','missed','error') AND timestamp > now64(3) - INTERVAL 7 DAY) AS n_7d, \
               countIf(result = 'ok') AS ok_30d, countIf(result IN ('ok','fail','timeout','missed','error')) AS n_30d, \
               avgIf(duration_ms, result = 'ok' AND duration_ms > 0) AS avg_ms \
             FROM monitor_checks WHERE project_id = {p:String} AND timestamp > now64(3) - INTERVAL 30 DAY GROUP BY monitor_id",
            &[("p", project_id.to_string())],
        )
        .await
        .unwrap_or_default();
    let pct = |ok: f64, n: f64| if n > 0.0 { Some((ok / n * 10000.0).round() / 100.0) } else { None };
    let f = |v: &Value| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
    rows.into_iter()
        .map(|r| {
            let avg = f(&r["avg_ms"]);
            (
                r["monitor_id"].as_str().unwrap_or("").to_string(),
                json!({
                    "uptime_24h": pct(f(&r["ok_1d"]), f(&r["n_1d"])),
                    "uptime_7d": pct(f(&r["ok_7d"]), f(&r["n_7d"])),
                    "uptime_30d": pct(f(&r["ok_30d"]), f(&r["n_30d"])),
                    "checks_30d": f(&r["n_30d"]) as i64,
                    "avg_duration_ms": if avg.is_finite() && avg > 0.0 { Some(avg.round() as i64) } else { None },
                }),
            )
        })
        .collect()
}

pub async fn list(state: &AppState, p: &Principal, project: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let rows = sqlx::query(&format!("SELECT {COLS} FROM monitors WHERE project_id = $1 ORDER BY slug")).bind(&proj.id).fetch_all(&state.db).await?;
    let stats = uptime(state, &proj.id).await;
    let monitors: Vec<Value> = rows
        .iter()
        .map(|r| {
            let m = from_row(r);
            let mut v = m.to_json(state);
            v["stats"] = stats.get(&m.id).cloned().unwrap_or(json!({}));
            v
        })
        .collect();
    let down = monitors.iter().filter(|m| m["state"] == "down").count();
    Ok(json!({"project": proj.slug, "monitors": monitors, "down": down}))
}

pub async fn get(state: &AppState, p: &Principal, project: &str, slug: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let m = get_row(state, &proj.id, slug).await?;
    let stats = uptime(state, &proj.id).await;
    let checks = recent_checks(state, &m, None, 50).await?;
    let incidents = sqlx::query("SELECT id, monitor_id, kind, started_at, ended_at, first_error, failed_checks, context, acknowledged_by, acknowledged_at FROM monitor_incidents WHERE monitor_id = $1 ORDER BY started_at DESC LIMIT 10")
        .bind(&m.id)
        .fetch_all(&state.db)
        .await?;
    let mut v = m.to_json(state);
    v["stats"] = stats.get(&m.id).cloned().unwrap_or(json!({}));
    Ok(json!({"project": proj.slug, "monitor": v, "recent_checks": checks, "incidents": incidents.iter().map(incident_from_row).collect::<Vec<_>>()}))
}

async fn recent_checks(state: &AppState, m: &Monitor, since: Option<i64>, limit: i64) -> anyhow::Result<Vec<Value>> {
    state
        .ch
        .query(
            "SELECT timestamp, result, duration_ms, http_status, error, check_in_id, release, environment, trace_id FROM monitor_checks \
             WHERE project_id = {p:String} AND monitor_id = {m:String} AND timestamp >= fromUnixTimestamp64Milli({since:Int64}) ORDER BY timestamp DESC LIMIT {n:UInt32}",
            &[("p", m.project_id.clone()), ("m", m.id.clone()), ("since", since.unwrap_or(0).to_string()), ("n", limit.to_string())],
        )
        .await
}

#[derive(Debug, Default, Deserialize)]
pub struct ChecksQuery {
    pub since: Option<String>,
    pub limit: Option<i64>,
}

pub async fn checks(state: &AppState, p: &Principal, project: &str, slug: &str, q: ChecksQuery) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let m = get_row(state, &proj.id, slug).await?;
    let now = now_ms();
    let since = match q.since.as_deref() {
        Some(s) => Some(crate::util::parse_time_expr(s, now).ok_or_else(|| ApiError::bad("since must be relative (24h, 7d) or RFC 3339"))?),
        None => Some(now - 86_400_000),
    };
    let checks = recent_checks(state, &m, since, q.limit.unwrap_or(200).clamp(1, 1000)).await?;
    Ok(json!({"project": proj.slug, "monitor": m.slug, "checks": checks}))
}

pub async fn create(state: &AppState, p: &Principal, project: &str, input: MonitorIn) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let m = insert_monitor(state, &proj, input, &p.label()).await?;
    Ok(json!({"monitor": m.to_json(state)}))
}

pub async fn update(state: &AppState, p: &Principal, project: &str, slug: &str, input: MonitorIn) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let cur = get_row(state, &proj.id, slug).await?;
    if input.kind.as_deref().map(|k| k != cur.kind).unwrap_or(false) {
        return Err(ApiError::bad("a monitor's kind can't change; create a new monitor"));
    }
    if input.slug.as_deref().map(|s| s != cur.slug).unwrap_or(false) {
        return Err(ApiError::bad("a monitor's slug can't change (check-ins use it); create a new monitor"));
    }
    let merged = MonitorIn {
        slug: None,
        kind: None,
        name: input.name.clone().or(Some(cur.name.clone())),
        enabled: input.enabled.or(Some(cur.enabled)),
        environment: input.environment.clone().or(Some(cur.environment.clone())),
        config: input.config.clone().or(Some(cur.config.clone())),
        fail_threshold: input.fail_threshold.or(Some(cur.fail_threshold)),
        recover_threshold: input.recover_threshold.or(Some(cur.recover_threshold)),
        paused_until: None,
    };
    let config = validate(&cur.kind, &merged)?;
    let now = now_ms();
    let schedule_changed = config != cur.config;
    sqlx::query("UPDATE monitors SET name = $1, enabled = $2, environment = $3, config = $4, fail_threshold = $5, recover_threshold = $6, updated_at = $7 WHERE id = $8")
        .bind(merged.name.unwrap_or_default())
        .bind(merged.enabled.unwrap_or(true) as i64)
        .bind(merged.environment.unwrap_or_default())
        .bind(config.to_string())
        .bind(merged.fail_threshold.unwrap_or(1))
        .bind(merged.recover_threshold.unwrap_or(1))
        .bind(now)
        .bind(&cur.id)
        .execute(&state.db)
        .await?;
    match input.paused_until {
        Some(Some(v)) => {
            let until = parse_pause(&v, now)?;
            sqlx::query("UPDATE monitors SET paused_until = $1, next_check_at = NULL WHERE id = $2").bind(until).bind(&cur.id).execute(&state.db).await?;
        }
        Some(None) if cur.paused_until.is_some() => {
            let m = load(state, &cur.id).await?.ok_or_else(|| ApiError::not_found("monitor"))?;
            resume(state, &m, now).await?;
        }
        _ if schedule_changed && cur.paused_until.is_none() && cur.kind == "heartbeat" => {
            let hb = Heartbeat::from_config(&config).map_err(ApiError::bad)?;
            let e = hb.next_after(now);
            let next = heartbeat_next_check(&hb, Some(e), cur.run_started_at.filter(|_| !cur.run_id.is_empty()));
            sqlx::query("UPDATE monitors SET expected_at = $1, next_check_at = $2 WHERE id = $3").bind(e).bind(next).bind(&cur.id).execute(&state.db).await?;
        }
        _ => {}
    }
    crate::auth::audit(state, &p.label(), "update_monitor", &cur.id, &cur.slug).await;
    let m = load(state, &cur.id).await?.ok_or_else(|| ApiError::not_found("monitor"))?;
    Ok(json!({"monitor": m.to_json(state)}))
}

pub async fn delete(state: &AppState, p: &Principal, project: &str, slug: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let m = get_row(state, &proj.id, slug).await?;
    sqlx::query("DELETE FROM monitors WHERE id = $1").bind(&m.id).execute(&state.db).await?;
    sqlx::query("DELETE FROM monitor_secrets WHERE monitor_id = $1").bind(&m.id).execute(&state.db).await?;
    sqlx::query("DELETE FROM monitor_incidents WHERE monitor_id = $1").bind(&m.id).execute(&state.db).await?;
    crate::auth::audit(state, &p.label(), "delete_monitor", &m.id, &m.slug).await;
    Ok(json!({"deleted": m.slug}))
}

#[derive(Debug, Default, Deserialize)]
pub struct IncidentQuery {
    pub monitor: Option<String>,
    /// open, closed or all (default).
    pub status: Option<String>,
    pub since: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_incidents(state: &AppState, p: &Principal, project: &str, q: IncidentQuery) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let now = now_ms();
    let since = match q.since.as_deref() {
        Some(s) => crate::util::parse_time_expr(s, now).ok_or_else(|| ApiError::bad("since must be relative (24h, 7d) or RFC 3339"))?,
        None => now - 30 * 86_400_000,
    };
    let monitor_id = match q.monitor.as_deref() {
        Some(s) => get_row(state, &proj.id, s).await?.id,
        None => String::new(),
    };
    let status = q.status.unwrap_or_else(|| "all".into());
    if !matches!(status.as_str(), "open" | "closed" | "all") {
        return Err(ApiError::bad("status must be open, closed or all"));
    }
    let rows = sqlx::query(
        "SELECT i.id, i.monitor_id, i.kind, i.started_at, i.ended_at, i.first_error, i.failed_checks, i.context, i.acknowledged_by, i.acknowledged_at, m.slug, m.name \
         FROM monitor_incidents i JOIN monitors m ON m.id = i.monitor_id \
         WHERE i.project_id = $1 AND (i.started_at >= $2 OR i.ended_at IS NULL) AND ($3 = '' OR i.monitor_id = $3) \
         AND ($4 = 'all' OR ($4 = 'open' AND i.ended_at IS NULL) OR ($4 = 'closed' AND i.ended_at IS NOT NULL)) \
         ORDER BY i.started_at DESC LIMIT $5",
    )
    .bind(&proj.id)
    .bind(since)
    .bind(&monitor_id)
    .bind(&status)
    .bind(q.limit.unwrap_or(50).clamp(1, 500))
    .fetch_all(&state.db)
    .await?;
    let incidents: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = incident_from_row(r);
            v["monitor"] = json!({"slug": r.s("slug"), "name": r.s("name")});
            v
        })
        .collect();
    Ok(json!({"project": proj.slug, "incidents": incidents}))
}

/// Load an incident the principal may read: (project id, incident JSON with its monitor).
async fn incident_for(state: &AppState, p: &Principal, id: &str) -> QResult<(String, Value)> {
    let r = sqlx::query(
        "SELECT i.id, i.monitor_id, i.project_id, i.kind, i.started_at, i.ended_at, i.first_error, i.failed_checks, i.context, i.acknowledged_by, i.acknowledged_at, m.slug, m.name \
         FROM monitor_incidents i JOIN monitors m ON m.id = i.monitor_id WHERE i.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("incident"))?;
    let project_id = r.s("project_id");
    if !p.can_read(&project_id) {
        return Err(ApiError::not_found("incident"));
    }
    let mut v = incident_from_row(&r);
    v["monitor"] = json!({"slug": r.s("slug"), "name": r.s("name")});
    Ok((project_id, v))
}

pub async fn get_incident(state: &AppState, p: &Principal, id: &str) -> QResult<Value> {
    let (project_id, mut v) = incident_for(state, p, id).await?;
    if let Some(proj) = crate::projects::find(state, &project_id).await? {
        v["project"] = json!(proj.slug);
    }
    Ok(json!({"incident": v}))
}

#[derive(Debug, Default, Deserialize)]
pub struct IncidentUpdate {
    pub acknowledged: Option<bool>,
}

pub async fn update_incident(state: &AppState, p: &Principal, id: &str, u: IncidentUpdate) -> QResult<Value> {
    let (project_id, _) = incident_for(state, p, id).await?;
    if !p.can_write(&project_id) {
        return Err(ApiError::forbidden());
    }
    match u.acknowledged {
        Some(true) => {
            sqlx::query("UPDATE monitor_incidents SET acknowledged_by = $1, acknowledged_at = $2 WHERE id = $3").bind(p.label()).bind(now_ms()).bind(id).execute(&state.db).await?;
        }
        Some(false) => {
            sqlx::query("UPDATE monitor_incidents SET acknowledged_by = '', acknowledged_at = NULL WHERE id = $1").bind(id).execute(&state.db).await?;
        }
        None => {}
    }
    get_incident(state, p, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration_s("90"), Some(90));
        assert_eq!(parse_duration_s("15m"), Some(900));
        assert_eq!(parse_duration_s("1h"), Some(3600));
        assert_eq!(parse_duration_s("2d"), Some(172_800));
        assert_eq!(parse_duration_s("0"), None);
        assert_eq!(parse_duration_s("abc"), None);
    }

    #[test]
    fn heartbeat_config() {
        assert!(Heartbeat::from_config(&json!({})).is_err());
        assert!(Heartbeat::from_config(&json!({"schedule": {"every": "30s"}})).is_err());
        assert!(Heartbeat::from_config(&json!({"schedule": {"cron": "0 3 * * *", "timezone": "Mars/Base"}})).is_err());
        let hb = Heartbeat::from_config(&json!({"schedule": {"every": "1h"}, "grace_s": "5m"})).unwrap();
        assert_eq!(hb.schedule, Schedule::Every(3600));
        assert_eq!(hb.grace_s, 300);
        assert_eq!(hb.max_runtime_s, DEFAULT_MAX_RUNTIME_S);
        assert_eq!(hb.next_after(1000), 3_601_000);
    }

    #[test]
    fn cron_check_in_covers_expected_run() {
        let hb = Heartbeat::from_config(&json!({"schedule": {"cron": "0 3 * * *"}, "grace_s": 120})).unwrap();
        let day = 86_400_000;
        let three = 3 * 3_600_000;
        // Expected 03:00 today; the job reports at 02:59:58 and at 03:00:05: both cover today.
        assert_eq!(hb.after_check_in(three - 2000, Some(three)), three + day);
        assert_eq!(hb.after_check_in(three + 5000, Some(three)), three + day);
        // A manual run at 10:00 doesn't push the schedule past tomorrow's 03:00.
        assert_eq!(hb.after_check_in(10 * 3_600_000, Some(three + day)), three + day);
    }

    #[test]
    fn next_check_is_deadline_or_timeout() {
        let hb = Heartbeat { schedule: Schedule::Every(3600), grace_s: 60, max_runtime_s: 600 };
        assert_eq!(heartbeat_next_check(&hb, Some(10_000), None), Some(70_000));
        assert_eq!(heartbeat_next_check(&hb, Some(10_000_000), Some(0)), Some(600_000));
        let hb = Heartbeat { max_runtime_s: 0, ..hb };
        assert_eq!(heartbeat_next_check(&hb, Some(10_000), Some(0)), Some(70_000));
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Nightly Backup (prod)"), "nightly-backup-prod");
        assert!(valid_slug("nightly-backup_1"));
        assert!(!valid_slug("Nightly"));
        assert!(!valid_slug(""));
    }
}
