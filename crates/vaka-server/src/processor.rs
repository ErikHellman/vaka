//! Background pipeline: symbolicate and group errors, maintain issues, and
//! write all rows to ClickHouse in batches.

use crate::db::RowExt;
use crate::models::{ErrorItem, ExceptionData, SdkInfo};
use crate::scrub::Scrubber;
use crate::state::{AppState, ProjectInfo};
use crate::util::{ch_time, new_id, now_ms, parse_timestamp, truncate};
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc;

pub enum Work {
    Error {
        project: ProjectInfo,
        item: Box<ErrorItem>,
        country: String,
        user_agent: String,
        sdk: SdkInfo,
        received_at: i64,
    },
    Session(SessionRow),
    Event(Box<EventRow>),
}

#[derive(Debug, Serialize, Clone)]
pub struct SessionRow {
    pub session_id: String,
    pub project_id: String,
    pub distinct_id: String,
    pub user_id: String,
    pub release: String,
    pub environment: String,
    pub platform: String,
    pub status: String,
    pub started: String,
    pub timestamp: String,
    pub duration_ms: u64,
    pub error_count: u32,
    pub retention_days: u16,
}

#[derive(Debug, Serialize, Clone)]
pub struct EventRow {
    pub event_id: String,
    pub project_id: String,
    pub timestamp: String,
    pub r#type: String,
    pub name: String,
    pub url: String,
    pub hostname: String,
    pub path: String,
    pub referrer: String,
    pub referrer_host: String,
    pub utm_source: String,
    pub utm_medium: String,
    pub utm_campaign: String,
    pub utm_term: String,
    pub utm_content: String,
    pub country: String,
    pub device_type: String,
    pub browser: String,
    pub browser_version: String,
    pub os_name: String,
    pub os_version: String,
    pub device_model: String,
    pub app_version: String,
    pub release: String,
    pub environment: String,
    pub platform: String,
    pub visitor_id: String,
    pub user_id: String,
    pub session_id: String,
    pub props: BTreeMap<String, String>,
    pub retention_days: u16,
}

#[derive(Debug, Serialize, Clone)]
pub struct ErrorRow {
    pub event_id: String,
    pub project_id: String,
    pub issue_id: String,
    pub fingerprint: String,
    pub timestamp: String,
    pub received_at: String,
    pub platform: String,
    pub level: String,
    pub release: String,
    pub environment: String,
    pub exception_type: String,
    pub exception_value: String,
    pub message: String,
    pub culprit: String,
    pub handled: u8,
    pub fatal: u8,
    pub frames: String,
    pub exceptions: String,
    pub breadcrumbs: String,
    pub tags: BTreeMap<String, String>,
    pub contexts: String,
    pub debug_images: String,
    pub user_id: String,
    pub anon_id: String,
    pub session_id: String,
    pub device_model: String,
    pub os_name: String,
    pub os_version: String,
    pub browser: String,
    pub country: String,
    pub url: String,
    pub sdk_name: String,
    pub sdk_version: String,
    pub symbolicated: u8,
    pub retention_days: u16,
}

/// Outcome of upserting an issue, used to trigger alerts.
#[derive(Debug, Clone, PartialEq)]
pub enum IssueChange {
    New,
    Regressed,
    Seen,
}

pub fn spawn(state: AppState, mut rx: mpsc::Receiver<Work>) {
    tokio::spawn(async move {
        let mut errors: Vec<ErrorRow> = Vec::new();
        let mut sessions: Vec<SessionRow> = Vec::new();
        let mut events: Vec<EventRow> = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_millis(1000));
        // Flush in the background (at most 2 inserts in flight) so processing never waits on ClickHouse.
        let inflight = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
        let spawn_flush = |state: &AppState, e: &mut Vec<ErrorRow>, s: &mut Vec<SessionRow>, v: &mut Vec<EventRow>, sem: std::sync::Arc<tokio::sync::Semaphore>| {
            let (state, mut e, mut s, mut v) = (state.clone(), std::mem::take(e), std::mem::take(s), std::mem::take(v));
            async move {
                if e.is_empty() && s.is_empty() && v.is_empty() {
                    return;
                }
                let permit = sem.acquire_owned().await.expect("semaphore closed");
                tokio::spawn(async move {
                    flush(&state, &mut e, &mut s, &mut v).await;
                    drop(permit);
                });
            }
        };
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let Some(work) = msg else { break };
                    match work {
                        Work::Error { project, item, country, user_agent, sdk, received_at } => {
                            match process_error(&state, &project, *item, country, user_agent, sdk, received_at).await {
                                Ok(row) => errors.push(row),
                                Err(e) => tracing::error!("failed to process error event: {e:#}"),
                            }
                        }
                        Work::Session(s) => sessions.push(s),
                        Work::Event(e) => events.push(*e),
                    }
                    if errors.len() + sessions.len() + events.len() >= 5000 {
                        spawn_flush(&state, &mut errors, &mut sessions, &mut events, inflight.clone()).await;
                    }
                }
                _ = tick.tick() => spawn_flush(&state, &mut errors, &mut sessions, &mut events, inflight.clone()).await,
            }
        }
        flush(&state, &mut errors, &mut sessions, &mut events).await;
        let _ = inflight.acquire_many(2).await;
    });
}

async fn flush(state: &AppState, errors: &mut Vec<ErrorRow>, sessions: &mut Vec<SessionRow>, events: &mut Vec<EventRow>) {
    for attempt in 0..3 {
        let mut ok = true;
        if !errors.is_empty() {
            match state.ch.insert("errors", errors).await {
                Ok(()) => errors.clear(),
                Err(e) => { ok = false; tracing::warn!(attempt, "insert errors: {e:#}") }
            }
        }
        if !sessions.is_empty() {
            match state.ch.insert("sessions", sessions).await {
                Ok(()) => sessions.clear(),
                Err(e) => { ok = false; tracing::warn!(attempt, "insert sessions: {e:#}") }
            }
        }
        if !events.is_empty() {
            match state.ch.insert("events", events).await {
                Ok(()) => events.clear(),
                Err(e) => { ok = false; tracing::warn!(attempt, "insert events: {e:#}") }
            }
        }
        if ok {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))).await;
    }
    // Keep memory bounded if ClickHouse stays down.
    for (name, len) in [("errors", errors.len()), ("sessions", sessions.len()), ("events", events.len())] {
        if len > 50_000 {
            tracing::error!("dropping {len} buffered {name} rows: ClickHouse unavailable");
        }
    }
    if errors.len() > 50_000 { errors.clear() }
    if sessions.len() > 50_000 { sessions.clear() }
    if events.len() > 50_000 { events.clear() }
}

pub fn exceptions_of(item: &ErrorItem) -> Vec<ExceptionData> {
    let mut v = item.exceptions.clone();
    if v.is_empty() {
        if let Some(e) = &item.exception {
            v.push(e.clone());
        }
    }
    v
}

async fn process_error(
    state: &AppState,
    project: &ProjectInfo,
    item: ErrorItem,
    country: String,
    user_agent: String,
    sdk: SdkInfo,
    received_at: i64,
) -> anyhow::Result<ErrorRow> {
    let platform = item.platform.clone().unwrap_or_else(|| project.platform.clone());
    let release = item.release.clone().unwrap_or_default();
    let mut exceptions = exceptions_of(&item);
    let images = crate::symbolicate::normalize_images(&item.debug_images);

    let symbolicated = crate::symbolicate::symbolicate(state, &project.id, &release, &platform, &mut exceptions, &images).await;

    let message = item.message.clone().unwrap_or_default();
    let fingerprint = crate::grouping::fingerprint(&platform, &exceptions, Some(&message), item.fingerprint.as_deref());
    let title = crate::grouping::title(&exceptions, Some(&message));
    let culprit = crate::grouping::culprit(&exceptions);
    let level = item.level.clone().unwrap_or_else(|| if item.fatal.unwrap_or(false) { "fatal".into() } else { "error".into() });
    let ts = item.timestamp.as_ref().and_then(parse_timestamp).unwrap_or(received_at);

    let (issue_id, change) = upsert_issue(state, project, &fingerprint, &title, &culprit, &level, &platform, &release, ts).await?;
    if change == IssueChange::New {
        let exc = serde_json::to_value(&exceptions).unwrap_or_default();
        let tags = serde_json::to_value(&item.tags).unwrap_or_default();
        if let Err(e) = crate::ownership::assign_new_issue(state, &project.id, &issue_id, &exc, &tags).await {
            tracing::warn!("ownership rules failed: {e:#}");
        }
    }
    if change != IssueChange::Seen {
        let ctx = serde_json::json!({
            "environment": item.environment.clone().unwrap_or_else(|| "production".into()), "level": level, "release": release,
            "handled": item.handled.unwrap_or(true), "tags": item.tags,
        });
        crate::alerts::notify_issue(state, project, &issue_id, if change == IssueChange::New { "new_issue" } else { "regression" }, Some(ctx));
    }
    if !release.is_empty() {
        let created = sqlx::query("INSERT INTO releases (project_id, version, created_at) VALUES ($1, $2, $3) ON CONFLICT (project_id, version) DO NOTHING")
            .bind(&project.id).bind(&release).bind(ts).execute(&state.db).await.map(|r| r.rows_affected() == 1).unwrap_or(false);
        if created {
            crate::alerts::notify_release(state, project, &release);
        }
    }

    let mut breadcrumbs = serde_json::Value::Array(item.breadcrumbs.iter().rev().take(100).rev().cloned().collect());
    let mut contexts = item.contexts.clone().unwrap_or(serde_json::Value::Null);
    let mut tags: serde_json::Value = serde_json::to_value(&item.tags)?;
    let mut exc_value = serde_json::to_value(&exceptions)?;
    let mut msg_value = serde_json::Value::String(message);
    let mut url_value = serde_json::Value::String(item.url.clone().unwrap_or_default());
    if project.scrub_enabled {
        let s = Scrubber::new(&project.scrub_fields);
        for v in [&mut breadcrumbs, &mut contexts, &mut tags, &mut exc_value, &mut msg_value, &mut url_value] {
            s.scrub_value(v);
        }
    }
    let exceptions: Vec<ExceptionData> = serde_json::from_value(exc_value).unwrap_or_default();
    let first = exceptions.first().cloned().unwrap_or_default();
    let device = item.device.clone().unwrap_or_default();
    let tags: BTreeMap<String, String> = tags
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (truncate(k, 64), truncate(&crate::models::value_to_string(v), 256))).collect())
        .unwrap_or_default();
    let browser = if user_agent.is_empty() { String::new() } else {
        woothee::parser::Parser::new().parse(&user_agent).map(|r| r.name.to_string()).filter(|n| n != "UNKNOWN").unwrap_or_default()
    };

    Ok(ErrorRow {
        event_id: item.event_id.clone().filter(|i| uuid::Uuid::parse_str(i).is_ok()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        project_id: project.id.clone(),
        issue_id,
        fingerprint,
        timestamp: ch_time(ts),
        received_at: ch_time(received_at),
        platform,
        level,
        release,
        environment: item.environment.clone().unwrap_or_else(|| "production".into()),
        exception_type: truncate(&first.ty, 256),
        exception_value: truncate(&first.value, 4096),
        message: truncate(msg_value.as_str().unwrap_or(""), 4096),
        culprit,
        handled: item.handled.map(|h| h as u8).unwrap_or(1),
        fatal: item.fatal.unwrap_or(false) as u8,
        frames: serde_json::to_string(&first.stacktrace)?,
        exceptions: serde_json::to_string(&exceptions)?,
        breadcrumbs: serde_json::to_string(&breadcrumbs)?,
        tags,
        contexts: if contexts.is_null() { String::new() } else { serde_json::to_string(&contexts)? },
        debug_images: serde_json::to_string(&images)?,
        user_id: item.user_id.clone().or_else(|| item.user.as_ref().and_then(|u| u.id.clone())).unwrap_or_default(),
        anon_id: item.anon_id.as_deref().filter(|a| !a.is_empty()).map(|a| crate::ingest::hash_install_id(&project.id, a)).unwrap_or_default(),
        session_id: item.session_id.clone().unwrap_or_default(),
        device_model: device.model.unwrap_or_default(),
        os_name: device.os_name.unwrap_or_default(),
        os_version: device.os_version.unwrap_or_default(),
        browser,
        country,
        url: truncate(url_value.as_str().unwrap_or(""), 2048),
        sdk_name: sdk.name,
        sdk_version: sdk.version,
        symbolicated: symbolicated as u8,
        retention_days: project.retention_days as u16,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_issue(
    state: &AppState,
    project: &ProjectInfo,
    fingerprint: &str,
    title: &str,
    culprit: &str,
    level: &str,
    platform: &str,
    release: &str,
    ts: i64,
) -> anyhow::Result<(String, IssueChange)> {
    let existing = sqlx::query("SELECT id, status, ignored_until, resolved_in_release FROM issues WHERE project_id = $1 AND fingerprint = $2")
        .bind(&project.id)
        .bind(fingerprint)
        .fetch_optional(&state.db)
        .await?;
    match existing {
        None => {
            let id = new_id();
            let short = sqlx::query("SELECT COALESCE(MAX(short_id), 0) AS m FROM issues WHERE project_id = $1")
                .bind(&project.id)
                .fetch_one(&state.db)
                .await?
                .i("m")
                + 1;
            let res = sqlx::query(
                "INSERT INTO issues (id, short_id, project_id, fingerprint, title, culprit, level, platform, status, first_seen, last_seen, times_seen, first_release, last_release) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'open', $9, $9, 1, $10, $10) ON CONFLICT (project_id, fingerprint) DO NOTHING",
            )
            .bind(&id).bind(short).bind(&project.id).bind(fingerprint).bind(title).bind(culprit)
            .bind(level).bind(platform).bind(ts).bind(release)
            .execute(&state.db)
            .await?;
            if res.rows_affected() == 1 {
                return Ok((id, IssueChange::New));
            }
            // Lost a race; fall through as a sighting.
            let row = sqlx::query("SELECT id FROM issues WHERE project_id = $1 AND fingerprint = $2")
                .bind(&project.id).bind(fingerprint).fetch_one(&state.db).await?;
            Ok((row.s("id"), IssueChange::Seen))
        }
        Some(row) => {
            let id = row.s("id");
            let status = row.s("status");
            let now = now_ms();
            let mut regressed = status == "resolved";
            // An event from a release older than the one with the fix is not a regression.
            if regressed {
                if let Some(fixed) = row.os("resolved_in_release").filter(|r| !r.is_empty() && r != release) {
                    if !release.is_empty() && release_is_older(state, &project.id, release, &fixed).await? {
                        regressed = false;
                    }
                }
            }
            let unignore = status == "ignored" && row.oi("ignored_until").map(|u| u > 0 && u < now).unwrap_or(false);
            let new_status = if regressed || unignore { "open".to_string() } else { status };
            sqlx::query(
                "UPDATE issues SET last_seen = CASE WHEN last_seen < $1 THEN $1 ELSE last_seen END, times_seen = times_seen + 1, \
                 last_release = CASE WHEN $2 = '' THEN last_release ELSE $2 END, status = $3, \
                 regressed_at = CASE WHEN $4 = 1 THEN $5 ELSE regressed_at END, \
                 resolved_at = CASE WHEN $4 = 1 THEN NULL ELSE resolved_at END, title = $6, culprit = $7 WHERE id = $8",
            )
            .bind(ts).bind(release).bind(&new_status).bind(regressed as i64).bind(now)
            .bind(title).bind(culprit).bind(&id)
            .execute(&state.db)
            .await?;
            Ok((id, if regressed { IssueChange::Regressed } else { IssueChange::Seen }))
        }
    }
}

/// Was `release` first seen before `than`? Unknown releases count as newer.
async fn release_is_older(state: &AppState, project_id: &str, release: &str, than: &str) -> anyhow::Result<bool> {
    let created = |v: String| {
        let state = state.clone();
        let project_id = project_id.to_string();
        async move {
            sqlx::query("SELECT created_at FROM releases WHERE project_id = $1 AND version = $2")
                .bind(project_id)
                .bind(v)
                .fetch_optional(&state.db)
                .await
                .map(|r| r.map(|r| r.i("created_at")))
        }
    };
    Ok(match (created(release.to_string()).await?, created(than.to_string()).await?) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    })
}
