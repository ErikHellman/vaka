//! Health monitoring: heartbeat (cron), HTTP and TLS monitors, their state machine, incidents
//! and alerts.
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
pub const KINDS: &[&str] = &["heartbeat", "http", "tls"];
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
/// HTTP and TLS checks: default interval, minimum interval, and parallel checks per tick.
const DEFAULT_HTTP_EVERY_S: i64 = 60;
const DEFAULT_TLS_EVERY_S: i64 = 6 * 3600;
const MIN_PROBE_EVERY_S: i64 = 30;
const PROBE_CONCURRENCY: usize = 16;
/// Probe monitors go down after 2 failed checks in a row and up after 2 good ones.
const PROBE_THRESHOLD: i64 = 2;

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
    pub tls_not_after: Option<i64>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLS: &str = "id, project_id, slug, name, kind, enabled, environment, config, fail_threshold, recover_threshold, state, state_since, flapping, \
    last_transition_at, transitions, paused_until, consecutive_fail, consecutive_ok, last_check_at, last_result, last_error, next_check_at, expected_at, \
    run_id, run_started_at, tls_not_after, created_by, created_at, updated_at";

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
        tls_not_after: oi(r, "tls_not_after"),
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

    fn probe(&self) -> Result<ProbeConfig, String> {
        ProbeConfig::from_config(&self.kind, &self.config)
    }

    /// "every 1h" or "cron in UTC" for heartbeats, "every 1m" for checks.
    pub fn schedule_text(&self) -> Option<String> {
        match self.kind.as_str() {
            "heartbeat" => self.heartbeat().ok().map(|h| h.describe()),
            _ => self.probe().ok().map(|p| format!("every {}", human_secs(p.every_s))),
        }
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
        } else {
            let pc = self.probe().ok();
            v["schedule"] = json!(self.schedule_text());
            v["target"] = json!(pc.as_ref().map(|p| p.target()));
            v["next_check_at"] = json!(if self.paused(now) { None } else { opt(self.next_check_at) });
            v["tls_expires_at"] = json!(opt(self.tls_not_after));
            v["tls_days_left"] = json!(self.tls_not_after.map(|t| (t - now).div_euclid(86_400_000)));
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

/// An HTTP or TLS monitor's check, parsed from its config.
#[derive(Clone, Debug)]
pub struct ProbeConfig {
    pub every_s: i64,
    pub timeout_s: i64,
    /// Days before certificate expiry to fire `tls_expiring` (0 = never).
    pub tls_warn_days: i64,
    pub http: Option<crate::probes::HttpCheck>,
    /// TLS: host and port.
    pub tls: Option<(String, u16)>,
}

impl ProbeConfig {
    pub fn from_config(kind: &str, c: &Value) -> Result<ProbeConfig, String> {
        let secs = |k: &str, default: i64| -> Result<i64, String> {
            match c.get(k) {
                None | Some(Value::Null) => Ok(default),
                Some(Value::Number(n)) => n.as_i64().filter(|n| *n > 0).ok_or(format!("{k} must be a positive number of seconds")),
                Some(Value::String(s)) => parse_duration_s(s).ok_or(format!("{k} must be a duration like \"5m\"")),
                _ => Err(format!("{k} must be a number of seconds")),
            }
        };
        let every_s = secs("every", if kind == "tls" { DEFAULT_TLS_EVERY_S } else { DEFAULT_HTTP_EVERY_S })?;
        if every_s < MIN_PROBE_EVERY_S || every_s > 7 * 86_400 {
            return Err(format!("every must be between {MIN_PROBE_EVERY_S} seconds and 7 days"));
        }
        let timeout_s = secs("timeout_s", crate::probes::DEFAULT_TIMEOUT_S)?;
        if timeout_s > crate::probes::MAX_TIMEOUT_S {
            return Err(format!("timeout_s must be at most {}", crate::probes::MAX_TIMEOUT_S));
        }
        let tls_warn_days = match c.get("tls_warn_days") {
            None | Some(Value::Null) => crate::probes::DEFAULT_TLS_WARN_DAYS,
            Some(v) => v.as_i64().filter(|d| (0..=365).contains(d)).ok_or("tls_warn_days must be 0 to 365")?,
        };
        let guard = crate::probes::guard();
        let mut pc = ProbeConfig { every_s, timeout_s, tls_warn_days, http: None, tls: None };
        match kind {
            "http" => {
                let url = c.get("url").and_then(|v| v.as_str()).ok_or("http monitors need `url`")?.trim().to_string();
                guard.check_url(&url)?;
                let method = match c.get("method").and_then(|v| v.as_str()).map(|m| m.to_ascii_uppercase()) {
                    None => reqwest::Method::GET,
                    Some(m) if m == "GET" => reqwest::Method::GET,
                    Some(m) if m == "HEAD" => reqwest::Method::HEAD,
                    Some(_) => return Err("method must be GET or HEAD".into()),
                };
                let body_contains = match c.get("body_contains") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) if !s.is_empty() && s.len() <= 200 => Some(s.clone()),
                    _ => return Err("body_contains must be text of 1 to 200 characters".into()),
                };
                if body_contains.is_some() && method == reqwest::Method::HEAD {
                    return Err("body_contains needs method GET".into());
                }
                let mut headers = Vec::new();
                if let Some(h) = c.get("headers").filter(|v| !v.is_null()) {
                    let obj = h.as_object().ok_or("headers must be an object of name: value")?;
                    if obj.len() > 20 {
                        return Err("at most 20 headers".into());
                    }
                    for (k, v) in obj {
                        let v = v.as_str().ok_or("header values must be strings")?;
                        reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|_| format!("`{k}` is not a valid header name"))?;
                        reqwest::header::HeaderValue::from_str(v).map_err(|_| format!("header `{k}` has an invalid value"))?;
                        headers.push((k.clone(), v.to_string()));
                    }
                }
                pc.http = Some(crate::probes::HttpCheck {
                    url,
                    method,
                    timeout: Duration::from_secs(timeout_s as u64),
                    expect: crate::probes::parse_expect(c.get("expect_status"))?,
                    body_contains,
                    headers,
                    follow_redirects: c.get("follow_redirects").and_then(|v| v.as_bool()).unwrap_or(true),
                });
            }
            "tls" => {
                let host = c.get("host").and_then(|v| v.as_str()).ok_or("tls monitors need `host` (and optionally `port`, default 443)")?.trim().trim_end_matches('.').to_ascii_lowercase();
                let port = match c.get("port") {
                    None | Some(Value::Null) => 443,
                    Some(v) => v.as_u64().filter(|p| (1..=65535).contains(p)).ok_or("port must be 1 to 65535")? as u16,
                };
                if host.is_empty() || host.contains('/') || host.contains(':') && !host.starts_with('[') {
                    return Err("host must be a host name like example.com, without scheme or path".into());
                }
                guard.check_url(&format!("https://{host}:{port}/"))?;
                pc.tls = Some((host, port));
            }
            _ => return Err(format!("{kind} monitors have no check")),
        }
        Ok(pc)
    }

    pub fn target(&self) -> String {
        match (&self.http, &self.tls) {
            (Some(h), _) => format!("{} {}", h.method, h.url),
            (_, Some((h, p))) => format!("{h}:{p}"),
            _ => String::new(),
        }
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
    pub tls_days_left: i64,
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
        tls_days_left: c.tls_days_left.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
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
            // The snapshot is quick (a few indexed queries) and goes into the alert payload.
            let mut ctx = correlate(state, &m, now, &c.check_in_id).await;
            if !c.trace_id.is_empty() {
                ctx["check_trace_id"] = json!(c.trace_id);
                if let Some(h) = ctx["hints"].as_array_mut() {
                    h.push(json!(format!("the failed check sent traceparent trace id {}; if the target reports traces to Vaka, get_trace shows where it failed", c.trace_id)));
                }
            }
            let _ = sqlx::query("UPDATE monitor_incidents SET context = $1 WHERE id = $2").bind(ctx.to_string()).bind(&incident).execute(&state.db).await;
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
        "monitor": {"id": m.id, "slug": m.slug, "name": m.name, "kind": m.kind, "environment": m.environment, "schedule": m.schedule_text(), "target": m.probe().ok().map(|p| p.target()), "tls_expires_at": m.tls_not_after.map(iso_time), "last_error": m.last_error},
        "context": incident["context"].clone(),
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
// Correlation
// ---------------------------------------------------------------------------

/// Error tag the SDK monitor helpers put on errors captured during a run.
pub const CHECK_IN_TAG: &str = "monitor.check_in_id";
const WINDOW_MS: i64 = 15 * 60_000;

/// What else happened around `at` in the monitor's project: recent releases, the issues with
/// the most errors (new ones flagged), the error rate against its 24 h baseline, top error
/// logs, and errors tagged with the failed run. Stored on the incident so an agent can see the
/// likely cause in one call.
pub async fn correlate(state: &AppState, m: &Monitor, at: i64, check_in_id: &str) -> Value {
    let mut ctx = serde_json::Map::new();
    ctx.insert("computed_at".into(), json!(iso_time(now_ms())));
    ctx.insert("window".into(), json!({"from": iso_time(at - WINDOW_MS), "to": iso_time(at + WINDOW_MS)}));

    if let Ok(rows) = sqlx::query("SELECT version, created_at FROM releases WHERE project_id = $1 AND created_at >= $2 AND created_at <= $3 ORDER BY created_at DESC LIMIT 5")
        .bind(&m.project_id)
        .bind(at - 2 * 3_600_000)
        .bind(at + WINDOW_MS)
        .fetch_all(&state.db)
        .await
    {
        let releases: Vec<Value> = rows.iter().map(|r| json!({"version": r.s("version"), "created_at": iso_time(r.i("created_at")), "minutes_before": (at - r.i("created_at")) / 60_000})).collect();
        ctx.insert("recent_releases".into(), json!(releases));
    }

    let env = m.environment.clone();
    let p = |extra: Vec<(&'static str, String)>| {
        let mut v = vec![("p", m.project_id.clone()), ("from", (at - WINDOW_MS).to_string()), ("to", (at + WINDOW_MS).to_string()), ("env", env.clone())];
        v.extend(extra);
        v
    };
    let issues = state
        .ch
        .query(
            "SELECT issue_id, count() AS events FROM errors WHERE project_id = {p:String} AND timestamp >= fromUnixTimestamp64Milli({from:Int64}) \
             AND timestamp <= fromUnixTimestamp64Milli({to:Int64}) AND ({env:String} = '' OR environment = {env:String}) GROUP BY issue_id ORDER BY events DESC LIMIT 5",
            &p(vec![]),
        )
        .await
        .unwrap_or_default();
    let mut top = Vec::new();
    for i in issues {
        let id = i["issue_id"].as_str().unwrap_or("").to_string();
        if let Ok(Some(r)) = sqlx::query("SELECT i.short_id, i.title, i.first_seen, i.regressed_at, i.status, p.slug FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
            .bind(&id)
            .fetch_optional(&state.db)
            .await
        {
            let first = r.i("first_seen");
            let regressed = oi(&r, "regressed_at");
            top.push(json!({
                "id": id,
                "short_id": crate::query::short_id(&r.s("slug"), r.i("short_id")),
                "title": r.s("title"),
                "status": r.s("status"),
                "events_in_window": i["events"],
                "new": (first - at).abs() <= WINDOW_MS,
                "regressed": regressed.map(|t| (t - at).abs() <= WINDOW_MS).unwrap_or(false),
            }));
        }
    }
    ctx.insert("issues".into(), json!(top));

    if let Ok(rows) = state
        .ch
        .query(
            "SELECT countIf(timestamp >= fromUnixTimestamp64Milli({from:Int64})) AS recent, countIf(timestamp < fromUnixTimestamp64Milli({from:Int64})) AS baseline \
             FROM errors WHERE project_id = {p:String} AND timestamp >= fromUnixTimestamp64Milli({base:Int64}) AND timestamp <= fromUnixTimestamp64Milli({at:Int64}) \
             AND ({env:String} = '' OR environment = {env:String})",
            &[("p", m.project_id.clone()), ("from", (at - 30 * 60_000).to_string()), ("base", (at - 86_400_000).to_string()), ("at", at.to_string()), ("env", env.clone())],
        )
        .await
    {
        if let Some(r) = rows.first() {
            let n = |v: &Value| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
            let recent = n(&r["recent"]);
            // The baseline covers 23.5 hours: 47 half-hour periods.
            let per_30m = n(&r["baseline"]) / 47.0;
            ctx.insert(
                "error_rate".into(),
                json!({"last_30m": recent as i64, "baseline_per_30m": (per_30m * 10.0).round() / 10.0, "ratio": if per_30m > 0.0 { Some((recent / per_30m * 10.0).round() / 10.0) } else { None }}),
            );
        }
    }

    let logs = state
        .ch
        .query(
            "SELECT substring(body, 1, 200) AS message, count() AS n, any(trace_id) AS trace_id FROM logs WHERE project_id = {p:String} \
             AND timestamp >= fromUnixTimestamp64Milli({from:Int64}) AND timestamp <= fromUnixTimestamp64Milli({to:Int64}) AND severity IN ('error', 'fatal') \
             AND ({env:String} = '' OR environment = {env:String}) GROUP BY message ORDER BY n DESC LIMIT 5",
            &p(vec![]),
        )
        .await
        .unwrap_or_default();
    ctx.insert("error_logs".into(), json!(logs));

    if !check_in_id.is_empty() {
        let run_errors = state
            .ch
            .query(
                "SELECT issue_id, any(exception_type) AS type, any(exception_value) AS value, count() AS events FROM errors WHERE project_id = {p:String} \
                 AND timestamp >= fromUnixTimestamp64Milli({from:Int64}) - INTERVAL 1 DAY AND tags[{tag:String}] = {run:String} GROUP BY issue_id LIMIT 5",
                &p(vec![("tag", CHECK_IN_TAG.to_string()), ("run", check_in_id.to_string())]),
            )
            .await
            .unwrap_or_default();
        ctx.insert("run_errors".into(), json!(run_errors));
    }

    let mut hints = Vec::new();
    if let Some(r) = ctx.get("recent_releases").and_then(|v| v.as_array()).and_then(|a| a.first()) {
        hints.push(format!("release {} was created {} min before the incident", r["version"].as_str().unwrap_or(""), r["minutes_before"]));
    }
    if let Some(i) = ctx.get("issues").and_then(|v| v.as_array()).and_then(|a| a.iter().find(|i| i["new"] == true || i["regressed"] == true)) {
        hints.push(format!("issue {} ({}) is {} around the same time", i["short_id"].as_str().unwrap_or(""), i["title"].as_str().unwrap_or(""), if i["new"] == true { "new" } else { "regressed" }));
    }
    if ctx.get("run_errors").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false) {
        hints.push("the failed run reported errors (run_errors); start with get_fix_context on that issue".into());
    }
    ctx.insert("hints".into(), json!(hints));
    Value::Object(ctx)
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
    let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(PROBE_CONCURRENCY));
    let mut probes = tokio::task::JoinSet::new();
    for r in due {
        let id = r.s("id");
        if !claim(state, &id, now).await? {
            continue;
        }
        match load(state, &id).await? {
            Some(m) if m.kind == "heartbeat" => {
                let res = evaluate_heartbeat(state, m, now).await;
                release(state, &id).await;
                res?;
            }
            Some(m) => {
                // HTTP and TLS checks wait on the network, so they run in parallel.
                let st = state.clone();
                let permit = limit.clone().acquire_owned().await?;
                probes.spawn(async move {
                    if let Err(e) = evaluate_probe(&st, m, None).await {
                        tracing::warn!("monitor check failed: {e:#}");
                    }
                    release(&st, &id).await;
                    drop(permit);
                });
            }
            None => release(state, &id).await,
        }
    }
    while probes.join_next().await.is_some() {}
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

/// Run an HTTP or TLS monitor's check now and apply the result. The caller holds the lease.
async fn evaluate_probe(state: &AppState, m: Monitor, secrets: Option<Vec<(String, String)>>) -> anyhow::Result<(Monitor, CheckResult)> {
    let pc = match m.probe() {
        Ok(pc) => pc,
        Err(e) => {
            // The config no longer validates (for example the allowlist changed): fail visibly.
            let mut c = CheckResult::new("fail");
            c.error = e;
            let next = Some(now_ms() + DEFAULT_HTTP_EVERY_S * 1000);
            let m = apply(state, m, &c, now_ms(), (next, None)).await?;
            return Ok((m, c));
        }
    };
    let probe = if let Some(mut http) = pc.http.clone() {
        let secrets = match secrets {
            Some(s) => s,
            None => load_secret_headers(state, &m.id).await,
        };
        http.headers.extend(secrets);
        crate::probes::run_http(&http).await
    } else if let Some((host, port)) = &pc.tls {
        crate::probes::run_tls(host, *port, Duration::from_secs(pc.timeout_s as u64)).await
    } else {
        anyhow::bail!("monitor {} has no check", m.slug);
    };
    let now = now_ms();
    let mut c = CheckResult::new(&probe.result);
    c.duration_ms = probe.duration_ms;
    c.http_status = probe.http_status;
    c.error = probe.error.clone();
    c.trace_id = probe.trace_id.clone();
    c.environment = m.environment.clone();
    if let Some(na) = probe.tls_not_after {
        c.tls_days_left = (na - now).div_euclid(86_400_000);
    }
    let next = Some(now + pc.every_s * 1000);
    let m = apply(state, m, &c, now, (next, None)).await?;
    if let Some(na) = probe.tls_not_after {
        tls_expiry(state, &m, &pc, na, now).await?;
    }
    Ok((m, c))
}

/// Track the certificate's expiry and fire `tls_expiring` once per certificate when it is
/// within `tls_warn_days`. The warning is an incident of kind `tls_expiring` that closes when
/// a renewed certificate shows up.
async fn tls_expiry(state: &AppState, m: &Monitor, pc: &ProbeConfig, not_after: i64, now: i64) -> anyhow::Result<()> {
    if m.tls_not_after != Some(not_after) {
        sqlx::query("UPDATE monitors SET tls_not_after = $1 WHERE id = $2").bind(not_after).bind(&m.id).execute(&state.db).await?;
    }
    let open = sqlx::query("SELECT id, first_error FROM monitor_incidents WHERE monitor_id = $1 AND kind = 'tls_expiring' AND ended_at IS NULL LIMIT 1")
        .bind(&m.id)
        .fetch_optional(&state.db)
        .await?;
    let days_left = (not_after - now).div_euclid(86_400_000);
    let expiring = pc.tls_warn_days > 0 && days_left < pc.tls_warn_days;
    match open {
        Some(r) if !expiring => {
            // Renewed (or the warning was turned off).
            sqlx::query("UPDATE monitor_incidents SET ended_at = $1 WHERE id = $2").bind(now).bind(r.s("id")).execute(&state.db).await?;
        }
        None if expiring => {
            let id = new_id();
            let msg = if days_left < 0 {
                format!("the certificate expired on {}", iso_time(not_after))
            } else {
                format!("the certificate expires in {days_left} days ({})", iso_time(not_after))
            };
            sqlx::query("INSERT INTO monitor_incidents (id, monitor_id, project_id, kind, started_at, first_error, failed_checks) VALUES ($1, $2, $3, 'tls_expiring', $4, $5, 0)")
                .bind(&id)
                .bind(&m.id)
                .bind(&m.project_id)
                .bind(now)
                .bind(&msg)
                .execute(&state.db)
                .await?;
            let mut m2 = m.clone();
            m2.tls_not_after = Some(not_after);
            notify(state, &m2, "tls_expiring", &id, json!({"days_left": days_left, "expires_at": iso_time(not_after), "warn_days": pc.tls_warn_days})).await;
        }
        _ => {}
    }
    Ok(())
}

async fn load_secret_headers(state: &AppState, monitor_id: &str) -> Vec<(String, String)> {
    let rows = sqlx::query("SELECT name, value FROM monitor_secrets WHERE monitor_id = $1 ORDER BY name").bind(monitor_id).fetch_all(&state.db).await.unwrap_or_default();
    rows.iter()
        .filter_map(|r| match crate::secretbox::open(&r.s("value")) {
            Ok(v) => Some((r.s("name"), v)),
            Err(e) => {
                tracing::warn!("monitor secret header {} unreadable: {e:#}", r.s("name"));
                None
            }
        })
        .collect()
}

async fn secret_header_names(state: &AppState, monitor_id: &str) -> Vec<String> {
    sqlx::query("SELECT name FROM monitor_secrets WHERE monitor_id = $1 ORDER BY name").bind(monitor_id).fetch_all(&state.db).await.unwrap_or_default().iter().map(|r| r.s("name")).collect()
}

/// Replace a monitor's secret headers (values sealed with `VAKA_SECRET_KEY`).
async fn store_secret_headers(state: &AppState, monitor_id: &str, headers: &HashMap<String, String>) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM monitor_secrets WHERE monitor_id = $1").bind(monitor_id).execute(&state.db).await?;
    for (k, v) in headers {
        sqlx::query("INSERT INTO monitor_secrets (monitor_id, name, value) VALUES ($1, $2, $3)").bind(monitor_id).bind(k).bind(crate::secretbox::seal(v)).execute(&state.db).await?;
    }
    Ok(())
}

fn validate_secret_headers(h: &HashMap<String, String>) -> QResult<()> {
    if h.len() > 10 {
        return Err(ApiError::bad("at most 10 secret_headers"));
    }
    for (k, v) in h {
        reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|_| ApiError::bad(format!("`{k}` is not a valid header name")))?;
        let mut hv = reqwest::header::HeaderValue::from_str(v).map_err(|_| ApiError::bad(format!("secret header `{k}` has an invalid value")))?;
        hv.set_sensitive(true);
    }
    Ok(())
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
    /// HTTP monitors: headers whose values are stored encrypted and never returned (for
    /// example Authorization). Replaces the stored set; `{}` removes them.
    pub secret_headers: Option<HashMap<String, String>>,
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
    } else {
        ProbeConfig::from_config(kind, &config).map_err(ApiError::bad)?;
    }
    if let Some(h) = &input.secret_headers {
        if kind != "http" {
            return Err(ApiError::bad("secret_headers are for http monitors"));
        }
        validate_secret_headers(h)?;
    }
    // Keep only known keys so stored config stays predictable.
    let keep: &[&str] = match kind {
        "heartbeat" => &["schedule", "grace_s", "max_runtime_s"],
        "http" => &["url", "method", "every", "timeout_s", "expect_status", "body_contains", "headers", "follow_redirects", "tls_warn_days"],
        "tls" => &["host", "port", "every", "timeout_s", "tls_warn_days"],
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
    let default_threshold = if kind == "heartbeat" { 1 } else { PROBE_THRESHOLD };
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
    if let Some(h) = &input.secret_headers {
        store_secret_headers(state, &id, h).await?;
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
    let mut v = full_json(state, &m).await;
    v["stats"] = stats.get(&m.id).cloned().unwrap_or(json!({}));
    Ok(json!({"project": proj.slug, "monitor": v, "recent_checks": checks, "incidents": incidents.iter().map(incident_from_row).collect::<Vec<_>>()}))
}

async fn recent_checks(state: &AppState, m: &Monitor, since: Option<i64>, limit: i64) -> anyhow::Result<Vec<Value>> {
    state
        .ch
        .query(
            "SELECT timestamp, result, duration_ms, http_status, error, tls_days_left, check_in_id, release, environment, trace_id FROM monitor_checks \
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

/// A monitor's JSON with the names (never the values) of its secret headers.
async fn full_json(state: &AppState, m: &Monitor) -> Value {
    let mut v = m.to_json(state);
    if m.kind == "http" {
        v["secret_headers"] = json!(secret_header_names(state, &m.id).await);
    }
    v
}

pub async fn create(state: &AppState, p: &Principal, project: &str, input: MonitorIn) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let m = insert_monitor(state, &proj, input, &p.label()).await?;
    Ok(json!({"monitor": full_json(state, &m).await}))
}

/// Run an HTTP or TLS monitor's check now (`run_monitor_check`), applying the result like a
/// scheduled check. Returns the check and the monitor's new state.
pub async fn run_now(state: &AppState, p: &Principal, project: &str, slug: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let m = get_row(state, &proj.id, slug).await?;
    if m.kind == "heartbeat" {
        return Err(ApiError::bad("heartbeat monitors are checked by their job's check-ins; run the job, or send a check-in"));
    }
    let mut claimed = false;
    for _ in 0..30 {
        if claim(state, &m.id, now_ms()).await? {
            claimed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !claimed {
        return Err(ApiError::bad("the monitor is being checked right now; try again in a moment"));
    }
    let res = async {
        let Some(m) = load(state, &m.id).await? else { anyhow::bail!("monitor deleted") };
        evaluate_probe(state, m, None).await
    }
    .await;
    release(state, &m.id).await;
    let (m, c) = res?;
    Ok(json!({
        "monitor": full_json(state, &m).await,
        "check": {"result": c.result, "duration_ms": c.duration_ms, "http_status": if c.http_status > 0 { Some(c.http_status) } else { None }, "error": c.error, "trace_id": c.trace_id, "tls_days_left": m.tls_not_after.map(|_| c.tls_days_left)},
    }))
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
        secret_headers: input.secret_headers.clone(),
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
        _ if schedule_changed && cur.paused_until.is_none() => {
            // A changed target or interval is checked right away.
            sqlx::query("UPDATE monitors SET next_check_at = $1 WHERE id = $2").bind(now).bind(&cur.id).execute(&state.db).await?;
        }
        _ => {}
    }
    if let Some(h) = &input.secret_headers {
        store_secret_headers(state, &cur.id, h).await?;
    }
    crate::auth::audit(state, &p.label(), "update_monitor", &cur.id, &cur.slug).await;
    let m = load(state, &cur.id).await?.ok_or_else(|| ApiError::not_found("monitor"))?;
    Ok(json!({"monitor": full_json(state, &m).await}))
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
    if v["open"] == true {
        // Ongoing: refresh the context so it includes what happened since the start.
        let mid = v["monitor_id"].as_str().unwrap_or("").to_string();
        let started = sqlx::query("SELECT started_at FROM monitor_incidents WHERE id = $1").bind(id).fetch_one(&state.db).await?.i("started_at");
        if let Some(m) = load(state, &mid).await? {
            let mut ctx = correlate(state, &m, started, "").await;
            if let Some(run) = v["context"].get("run_errors") {
                ctx["run_errors"] = run.clone();
            }
            sqlx::query("UPDATE monitor_incidents SET context = $1 WHERE id = $2").bind(ctx.to_string()).bind(id).execute(&state.db).await?;
            v["context"] = ctx;
        }
    }
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
    fn probe_config() {
        let pc = ProbeConfig::from_config("http", &json!({"url": "https://example.com/health", "every": "5m", "expect_status": [200]})).unwrap();
        assert_eq!(pc.every_s, 300);
        assert_eq!(pc.target(), "GET https://example.com/health");
        assert!(ProbeConfig::from_config("http", &json!({})).is_err());
        assert!(ProbeConfig::from_config("http", &json!({"url": "http://192.168.0.1/"})).is_err());
        assert!(ProbeConfig::from_config("http", &json!({"url": "https://example.com", "every": "10s"})).is_err());
        assert!(ProbeConfig::from_config("http", &json!({"url": "https://example.com", "method": "POST"})).is_err());
        assert!(ProbeConfig::from_config("http", &json!({"url": "https://example.com", "method": "HEAD", "body_contains": "ok"})).is_err());
        assert!(ProbeConfig::from_config("http", &json!({"url": "https://example.com", "headers": {"bad name": "x"}})).is_err());
        let tls = ProbeConfig::from_config("tls", &json!({"host": "Example.com."})).unwrap();
        assert_eq!(tls.tls, Some(("example.com".into(), 443)));
        assert_eq!(tls.every_s, DEFAULT_TLS_EVERY_S);
        assert!(ProbeConfig::from_config("tls", &json!({"host": "https://example.com"})).is_err());
        assert!(ProbeConfig::from_config("tls", &json!({"host": "10.0.0.1"})).is_err());
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Nightly Backup (prod)"), "nightly-backup-prod");
        assert!(valid_slug("nightly-backup_1"));
        assert!(!valid_slug("Nightly"));
        assert!(!valid_slug(""));
    }
}
