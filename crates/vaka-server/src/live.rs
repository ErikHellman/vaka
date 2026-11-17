//! Live stream: new errors, analytics events and spans pushed to subscribers over
//! Server-Sent Events (`GET /api/v1/projects/{p}/stream`).
//!
//! The processor publishes each batch after it has been written to ClickHouse, so a
//! client that receives an `event_id` can immediately fetch the full event over REST.
//! Payloads are compact summaries (no stack frames, breadcrumbs or contexts).

use crate::auth::{ApiError, Principal};
use crate::processor::{ErrorRow, EventRow, SpanRow};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// Messages buffered per subscriber before it starts losing them (it then gets a `lagged` event).
pub const CHANNEL_CAPACITY: usize = 4096;
/// Concurrent stream connections per server.
const MAX_STREAMS: usize = 200;

static OPEN_STREAMS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone)]
pub struct LiveMsg {
    pub project_id: String,
    /// `error`, `event` or `span`.
    pub kind: &'static str,
    /// Analytics event type (`pageview`, `custom`, ...) or empty.
    pub subtype: String,
    pub environment: String,
    pub data: Value,
}

pub fn channel() -> broadcast::Sender<Arc<LiveMsg>> {
    broadcast::channel(CHANNEL_CAPACITY).0
}

/// ClickHouse-style `2026-11-16 14:03:15.695` (UTC) to RFC 3339.
fn iso(ts: &str) -> String {
    if ts.contains('T') {
        return ts.to_string();
    }
    format!("{}Z", ts.replacen(' ', "T", 1))
}

fn send(state: &AppState, msg: LiveMsg) {
    let _ = state.live.send(Arc::new(msg));
}

/// Publish a flushed batch. Cheap when nobody is listening.
pub fn publish(state: &AppState, errors: &[ErrorRow], events: &[EventRow], spans: &[SpanRow]) {
    if state.live.receiver_count() == 0 {
        return;
    }
    for e in errors {
        send(state, error_msg(e));
    }
    for e in events {
        send(state, event_msg(e));
    }
    for s in spans {
        send(state, span_msg(s));
    }
}

fn error_msg(e: &ErrorRow) -> LiveMsg {
    LiveMsg {
        project_id: e.project_id.clone(),
        kind: "error",
        subtype: String::new(),
        environment: e.environment.clone(),
        data: json!({
            "event_id": e.event_id,
            "issue_id": e.issue_id,
            "timestamp": iso(&e.timestamp),
            "level": e.level,
            "exception_type": e.exception_type,
            "exception_value": e.exception_value,
            "message": e.message,
            "culprit": e.culprit,
            "handled": e.handled == 1,
            "fatal": e.fatal == 1,
            "release": e.release,
            "environment": e.environment,
            "platform": e.platform,
            "url": e.url,
            "country": e.country,
            "browser": e.browser,
            "os_name": e.os_name,
            "device_model": e.device_model,
            "user_id": e.user_id,
            "session_id": e.session_id,
            "trace_id": e.trace_id,
        }),
    }
}

fn event_msg(e: &EventRow) -> LiveMsg {
    LiveMsg {
        project_id: e.project_id.clone(),
        kind: "event",
        subtype: e.r#type.clone(),
        environment: e.environment.clone(),
        data: json!({
            "event_id": e.event_id,
            "timestamp": iso(&e.timestamp),
            "type": e.r#type,
            "name": e.name,
            "hostname": e.hostname,
            "path": e.path,
            "referrer_host": e.referrer_host,
            "utm_source": e.utm_source,
            "country": e.country,
            "device_type": e.device_type,
            "browser": e.browser,
            "os_name": e.os_name,
            "app_version": e.app_version,
            "release": e.release,
            "environment": e.environment,
            "platform": e.platform,
            "user_id": e.user_id,
            "session_id": e.session_id,
            "props": e.props,
        }),
    }
}

fn span_msg(s: &SpanRow) -> LiveMsg {
    LiveMsg {
        project_id: s.project_id.clone(),
        kind: "span",
        subtype: if s.is_root == 1 {
            "root".into()
        } else {
            String::new()
        },
        environment: s.environment.clone(),
        data: json!({
            "trace_id": s.trace_id,
            "span_id": s.span_id,
            "parent_span_id": s.parent_span_id,
            "name": s.name,
            "op": s.op,
            "transaction": s.transaction,
            "status": s.status,
            "is_root": s.is_root == 1,
            "start": iso(&s.start),
            "duration_ms": s.duration_ms,
            "http_method": s.http_method,
            "http_status": s.http_status,
            "release": s.release,
            "environment": s.environment,
            "service": s.service,
        }),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct StreamQuery {
    /// Comma-separated: `error`, `event`, `span`. Default `error,event`.
    pub types: Option<String>,
    /// Comma-separated analytics event types (`pageview`, `screen_view`, `custom`, ...).
    pub event_types: Option<String>,
    /// Only this environment.
    pub environment: Option<String>,
    /// Only root spans (one per trace). Default true.
    pub root_spans_only: Option<bool>,
}

#[derive(Debug, Clone)]
struct Filter {
    project_id: String,
    kinds: Vec<String>,
    event_types: Vec<String>,
    environment: Option<String>,
    root_spans_only: bool,
}

fn csv(s: &Option<String>) -> Vec<String> {
    s.as_deref()
        .unwrap_or("")
        .split(',')
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
        .collect()
}

impl Filter {
    fn new(project_id: String, q: &StreamQuery) -> Result<Filter, ApiError> {
        let mut kinds: Vec<String> = csv(&q.types)
            .into_iter()
            .map(|k| k.trim_end_matches('s').to_string())
            .collect();
        if kinds.is_empty() {
            kinds = vec!["error".into(), "event".into()];
        }
        if let Some(bad) = kinds
            .iter()
            .find(|k| !matches!(k.as_str(), "error" | "event" | "span"))
        {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                format!("unknown stream type '{bad}' (use error, event, span)"),
            ));
        }
        Ok(Filter {
            project_id,
            kinds,
            event_types: csv(&q.event_types),
            environment: q.environment.clone().filter(|e| !e.is_empty()),
            root_spans_only: q.root_spans_only.unwrap_or(true),
        })
    }

    fn matches(&self, m: &LiveMsg) -> bool {
        if m.project_id != self.project_id || !self.kinds.iter().any(|k| k == m.kind) {
            return false;
        }
        if let Some(env) = &self.environment {
            if &m.environment != env {
                return false;
            }
        }
        match m.kind {
            "event" => {
                self.event_types.is_empty() || self.event_types.iter().any(|t| t == &m.subtype)
            }
            "span" => !self.root_spans_only || m.subtype == "root",
            _ => true,
        }
    }
}

/// Decrements the open-stream counter when the response stream is dropped.
struct StreamGuard;

impl Drop for StreamGuard {
    fn drop(&mut self) {
        OPEN_STREAMS.fetch_sub(1, Ordering::Relaxed);
    }
}

pub async fn stream(
    State(s): State<AppState>,
    p: Principal,
    Path(project): Path<String>,
    Query(q): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, ApiError> {
    let proj = crate::query::project_for(&s, &p, &project).await?;
    let filter = Filter::new(proj.id.clone(), &q)?;
    if OPEN_STREAMS.fetch_add(1, Ordering::Relaxed) >= MAX_STREAMS {
        OPEN_STREAMS.fetch_sub(1, Ordering::Relaxed);
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many open streams on this server".into(),
        ));
    }
    let guard = StreamGuard;
    let rx = s.live.subscribe();
    let ready = Event::default().event("ready").data(
        json!({"project": proj.slug, "types": filter.kinds, "event_types": filter.event_types, "environment": filter.environment}).to_string(),
    );
    let first = futures::stream::once(async move { Ok(ready) });
    let rest = futures::stream::unfold((rx, filter, guard), |(mut rx, filter, guard)| async move {
        loop {
            match rx.recv().await {
                Ok(m) if filter.matches(&m) => {
                    let ev = Event::default().event(m.kind).data(m.data.to_string());
                    return Some((Ok(ev), (rx, filter, guard)));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    let ev = Event::default()
                        .event("lagged")
                        .data(json!({"dropped": n}).to_string());
                    return Some((Ok(ev), (rx, filter, guard)));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(Sse::new(futures::StreamExt::chain(first, rest)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(kind: &'static str, subtype: &str, env: &str) -> LiveMsg {
        LiveMsg {
            project_id: "p1".into(),
            kind,
            subtype: subtype.into(),
            environment: env.into(),
            data: json!({}),
        }
    }

    #[test]
    fn default_filter_is_errors_and_events() {
        let f = Filter::new("p1".into(), &StreamQuery::default()).unwrap();
        assert!(f.matches(&msg("error", "", "prod")));
        assert!(f.matches(&msg("event", "pageview", "prod")));
        assert!(!f.matches(&msg("span", "root", "prod")));
        let mut other = msg("error", "", "prod");
        other.project_id = "p2".into();
        assert!(!f.matches(&other));
    }

    #[test]
    fn filters_by_type_event_type_and_environment() {
        let q = StreamQuery {
            types: Some("events, spans".into()),
            event_types: Some("custom".into()),
            environment: Some("prod".into()),
            root_spans_only: None,
        };
        let f = Filter::new("p1".into(), &q).unwrap();
        assert!(!f.matches(&msg("error", "", "prod")));
        assert!(f.matches(&msg("event", "custom", "prod")));
        assert!(!f.matches(&msg("event", "pageview", "prod")));
        assert!(!f.matches(&msg("event", "custom", "staging")));
        assert!(f.matches(&msg("span", "root", "prod")));
        assert!(!f.matches(&msg("span", "", "prod")));
    }

    #[test]
    fn timestamps_are_rfc3339() {
        assert_eq!(iso("2026-11-16 14:03:15.695"), "2026-11-16T14:03:15.695Z");
        assert_eq!(iso("2026-11-16T14:03:15Z"), "2026-11-16T14:03:15Z");
    }

    #[test]
    fn rejects_unknown_types() {
        let q = StreamQuery {
            types: Some("logs".into()),
            ..Default::default()
        };
        assert!(Filter::new("p1".into(), &q).is_err());
    }
}
