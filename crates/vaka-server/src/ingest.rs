//! Public ingest API: `/ingest/v1/*`. Authenticated by a per-project ingest key
//! (write-only, safe to embed in apps). Requests are validated, enriched and
//! queued; heavy work (symbolication, grouping) happens in the processor.

use crate::models::*;
use crate::processor::{EventRow, SessionRow, Work};
use crate::state::{AppState, ProjectInfo};
use crate::util::{ch_time, new_id, now_ms, parse_timestamp, sha256_hex, truncate};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

pub const MAX_ITEMS_PER_REQUEST: usize = 1000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ingest/v1/batch", post(batch))
        .route("/ingest/v1/flags", post(flags))
        .route("/ingest/v1/{kind}", post(single_kind))
        .route("/ingest/v1/ping", get(|| async { Json(json!({"ok": true})) }))
        // OTLP/HTTP: set OTEL_EXPORTER_OTLP_ENDPOINT to <vaka>/ingest/otlp (the SDK appends /v1/traces).
        .route("/ingest/otlp/v1/traces", post(otlp_traces).layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024)))
        .route("/ingest/otlp/v1/logs", post(otlp_logs).layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024)))
        .route("/ingest/otlp/v1/metrics", post(otlp_metrics))
}

#[derive(Deserialize)]
pub struct KeyQuery {
    key: Option<String>,
}

#[derive(Serialize)]
struct Rejected {
    index: usize,
    error: String,
}

pub struct IngestError(StatusCode, String);

impl IntoResponse for IngestError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1, "docs": "/docs/ingest.md"}))).into_response()
    }
}

fn extract_key(headers: &HeaderMap, q: &KeyQuery) -> Option<String> {
    if let Some(k) = headers.get("x-vaka-key").and_then(|v| v.to_str().ok()) {
        return Some(k.trim().to_string());
    }
    if let Some(k) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(k.trim().to_string());
    }
    q.key.clone()
}

async fn authenticate(state: &AppState, headers: &HeaderMap, q: &KeyQuery) -> Result<ProjectInfo, IngestError> {
    let key = extract_key(headers, q).ok_or_else(|| {
        IngestError(
            StatusCode::UNAUTHORIZED,
            "missing ingest key: send it as the `X-Vaka-Key` header (or `?key=` for sendBeacon)".into(),
        )
    })?;
    match crate::projects::by_ingest_key(state, &key).await {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err(IngestError(
            StatusCode::UNAUTHORIZED,
            "unknown or revoked ingest key: check the key in the project settings (keys start with `vk_`)".into(),
        )),
        Err(e) => {
            tracing::error!("ingest key lookup failed: {e:#}");
            Err(IngestError(StatusCode::SERVICE_UNAVAILABLE, "temporarily unavailable, retry later".into()))
        }
    }
}

/// Request-level context used to enrich items.
pub struct RequestCtx {
    pub ip: Option<IpAddr>,
    pub user_agent: String,
    pub country: String,
    pub sdk: SdkInfo,
}

fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> Option<IpAddr> {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(ip) = xff.split(',').next().and_then(|s| s.trim().parse().ok()) {
            return Some(ip);
        }
    }
    if let Some(ip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse().ok()) {
        return Some(ip);
    }
    Some(peer.ip())
}

fn country(state: &AppState, headers: &HeaderMap, ip: Option<IpAddr>) -> String {
    for h in ["cf-ipcountry", "x-vercel-ip-country", "x-country-code"] {
        if let Some(c) = headers.get(h).and_then(|v| v.to_str().ok()) {
            let c = c.trim().to_uppercase();
            if c.len() == 2 && c != "XX" {
                return c;
            }
        }
    }
    if let (Some(reader), Some(ip)) = (&state.geoip, ip) {
        if let Ok(c) = reader.lookup::<maxminddb::geoip2::Country>(ip) {
            if let Some(code) = c.country.and_then(|c| c.iso_code) {
                return code.to_string();
            }
        }
    }
    String::new()
}

fn parse_body(body: &Bytes) -> Result<Value, IngestError> {
    serde_json::from_slice(body).map_err(|e| {
        IngestError(StatusCode::BAD_REQUEST, format!("request body is not valid JSON: {e}"))
    })
}

async fn batch(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(q): Query<KeyQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, IngestError> {
    let project = authenticate(&state, &headers, &q).await?;
    let v = parse_body(&body)?;
    let batch: Batch = serde_json::from_value(v).map_err(|e| {
        IngestError(StatusCode::BAD_REQUEST, format!("expected {{\"sdk\": {{...}}, \"items\": [...]}}: {e}"))
    })?;
    let ctx = build_ctx(&state, &headers, peer, batch.sdk.unwrap_or_default());
    accept(&state, &project, &ctx, batch.items, None).await
}

/// Evaluate all feature flags for one identity: `{distinct_id, user_id?, properties?, environment?}`.
async fn flags(State(state): State<AppState>, Query(q): Query<KeyQuery>, headers: HeaderMap, body: Bytes) -> Result<Response, IngestError> {
    let project = authenticate(&state, &headers, &q).await?;
    let req: crate::flags::EvalRequest = serde_json::from_value(parse_body(&body)?).map_err(|e| IngestError(StatusCode::BAD_REQUEST, format!("invalid flag request: {e}")))?;
    match crate::flags::evaluate_all(&state, &project, &req).await {
        Ok(v) => Ok(Json(v).into_response()),
        Err(e) if e.to_string().contains("required") => Err(IngestError(StatusCode::BAD_REQUEST, e.to_string())),
        Err(e) => {
            tracing::error!("flag evaluation failed: {e:#}");
            Err(IngestError(StatusCode::INTERNAL_SERVER_ERROR, "flag evaluation failed".into()))
        }
    }
}

async fn single_kind(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(kind): Path<String>,
    Query(q): Query<KeyQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, IngestError> {
    let ty = match kind.as_str() {
        "errors" => "error",
        "sessions" => "session",
        "events" => "event",
        "spans" => "span",
        other => {
            return Err(IngestError(
                StatusCode::NOT_FOUND,
                format!("unknown ingest endpoint `{other}`: use errors, sessions, events, spans or batch"),
            ))
        }
    };
    let project = authenticate(&state, &headers, &q).await?;
    let v = parse_body(&body)?;
    let sdk = headers
        .get("x-vaka-sdk")
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            let (name, version) = s.split_once('/').unwrap_or((s, ""));
            SdkInfo { name: name.into(), version: version.into() }
        })
        .unwrap_or_default();
    let items = match v {
        Value::Array(a) => a,
        other => vec![other],
    };
    let ctx = build_ctx(&state, &headers, peer, sdk);
    accept(&state, &project, &ctx, items, Some(ty)).await
}

fn build_ctx(state: &AppState, headers: &HeaderMap, peer: SocketAddr, sdk: SdkInfo) -> RequestCtx {
    let ip = client_ip(headers, peer);
    RequestCtx {
        country: country(state, headers, ip),
        ip,
        user_agent: headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
        sdk,
    }
}

async fn accept(
    state: &AppState,
    project: &ProjectInfo,
    ctx: &RequestCtx,
    items: Vec<Value>,
    forced_type: Option<&str>,
) -> Result<Response, IngestError> {
    if items.len() > MAX_ITEMS_PER_REQUEST {
        return Err(IngestError(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("too many items ({}), the limit is {MAX_ITEMS_PER_REQUEST} per request", items.len()),
        ));
    }
    let (accepted, rejected) = accept_items(state, project, ctx, items, forced_type).await?;
    let status = if accepted == 0 && !rejected.is_empty() { StatusCode::BAD_REQUEST } else { StatusCode::ACCEPTED };
    Ok((status, Json(json!({"accepted": accepted, "rejected": rejected}))).into_response())
}

async fn accept_items(
    state: &AppState,
    project: &ProjectInfo,
    ctx: &RequestCtx,
    items: Vec<Value>,
    forced_type: Option<&str>,
) -> Result<(usize, Vec<Rejected>), IngestError> {
    // Refuse the whole batch up front when the queue is nearly full, so SDKs can retry it
    // without creating duplicates of items that were already enqueued.
    if state.work_tx.capacity() < items.len() {
        return Err(IngestError(StatusCode::TOO_MANY_REQUESTS, "ingest queue is full, retry with backoff".into()));
    }
    let mut accepted = 0usize;
    let mut rejected = Vec::new();
    let salt = current_salt(state).await;
    for (index, mut raw) in items.into_iter().enumerate() {
        if let (Some(t), Value::Object(o)) = (forced_type, &mut raw) {
            o.entry("type").or_insert_with(|| Value::String(t.to_string()));
        }
        let item: Item = match serde_json::from_value(raw) {
            Ok(i) => i,
            Err(e) => {
                rejected.push(Rejected {
                    index,
                    error: format!("invalid item: {e} (each item needs \"type\": \"error\" | \"session\" | \"event\" | \"span\")"),
                });
                continue;
            }
        };
        let work = match item {
            Item::Error(e) => {
                if e.exceptions.is_empty() && e.exception.is_none() && e.message.as_deref().unwrap_or("").is_empty() {
                    rejected.push(Rejected { index, error: "error items need `exceptions`, `exception` or `message`".into() });
                    continue;
                }
                Work::Error {
                    project: project.clone(),
                    item: e,
                    country: ctx.country.clone(),
                    user_agent: ctx.user_agent.clone(),
                    sdk: ctx.sdk.clone(),
                    received_at: now_ms(),
                }
            }
            Item::Session(s) => {
                if s.session_id.is_empty() {
                    rejected.push(Rejected { index, error: "session items need a non-empty `session_id`".into() });
                    continue;
                }
                Work::Session(session_row(project, &s, &salt, ctx))
            }
            Item::Event(e) => {
                if e.name.trim().is_empty() {
                    rejected.push(Rejected { index, error: "event items need a non-empty `name`".into() });
                    continue;
                }
                match event_row(project, &e, &salt, ctx) {
                    Some(row) => Work::Event(Box::new(row)),
                    None => {
                        // Bots are dropped silently; count them as accepted.
                        accepted += 1;
                        continue;
                    }
                }
            }
            Item::Span(sp) => match crate::perf::span_row(project, &sp, now_ms()) {
                Ok(row) => Work::Span(Box::new(row)),
                Err(error) => {
                    rejected.push(Rejected { index, error });
                    continue;
                }
            },
        };
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(work)) = state.work_tx.try_send(work) {
            // Lost a race for the last slots: wait briefly rather than failing half a batch.
            if tokio::time::timeout(std::time::Duration::from_secs(5), state.work_tx.send(work)).await.map(|r| r.is_err()).unwrap_or(true) {
                return Err(IngestError(StatusCode::SERVICE_UNAVAILABLE, "ingest queue is full, retry with backoff".into()));
            }
        }
        accepted += 1;
    }
    Ok((accepted, rejected))
}

// ---------------------------------------------------------------------------
// OTLP/HTTP
// ---------------------------------------------------------------------------

fn otlp_reply(signal: crate::otel::Signal, proto: bool, rejected: i64, message: &str) -> Response {
    let (body, ct) = crate::otel::response(signal, proto, rejected, message);
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, ct)], body).into_response()
}

fn otlp_decode(signal: crate::otel::Signal, headers: &HeaderMap, body: &Bytes) -> Result<(Value, bool), IngestError> {
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    crate::otel::decode(signal, ct, body).map_err(|e| {
        if e.starts_with("unsupported content type") {
            IngestError(StatusCode::UNSUPPORTED_MEDIA_TYPE, e)
        } else {
            IngestError(StatusCode::BAD_REQUEST, e)
        }
    })
}

fn otlp_ctx(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> RequestCtx {
    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("otlp");
    let (name, version) = ua.split(' ').next().unwrap_or("otlp").split_once('/').unwrap_or((ua, ""));
    build_ctx(state, headers, peer, SdkInfo { name: truncate(name, 64), version: truncate(version, 32) })
}

/// Enqueue converted items in chunks (OTLP batches can be larger than our per-request limit).
async fn accept_chunks(state: &AppState, project: &ProjectInfo, ctx: &RequestCtx, items: Vec<Value>) -> Result<(usize, Vec<String>), IngestError> {
    let mut accepted = 0;
    let mut errors = Vec::new();
    let mut items = items;
    while !items.is_empty() {
        let rest = items.split_off(items.len().min(MAX_ITEMS_PER_REQUEST));
        let (a, r) = accept_items(state, project, ctx, std::mem::replace(&mut items, rest), None).await?;
        accepted += a;
        errors.extend(r.into_iter().map(|r| r.error));
    }
    Ok((accepted, errors))
}

async fn otlp_traces(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(q): Query<KeyQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, IngestError> {
    use crate::otel::Signal;
    let project = authenticate(&state, &headers, &q).await?;
    let (req, proto) = otlp_decode(Signal::Traces, &headers, &body)?;
    let items = crate::otel::trace_items(&req);
    let ctx = otlp_ctx(&state, &headers, peer);
    let (_, errors) = accept_chunks(&state, &project, &ctx, items).await?;
    Ok(otlp_reply(Signal::Traces, proto, errors.len() as i64, errors.first().map(String::as_str).unwrap_or("")))
}

async fn otlp_logs(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(q): Query<KeyQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, IngestError> {
    use crate::otel::Signal;
    let project = authenticate(&state, &headers, &q).await?;
    let (req, proto) = otlp_decode(Signal::Logs, &headers, &body)?;
    let (rows, error_items) = crate::otel::log_rows(&project, &req, now_ms());
    if let Err(e) = state.ch.insert("logs", &rows).await {
        tracing::warn!("insert logs: {e:#}");
        return Err(IngestError(StatusCode::SERVICE_UNAVAILABLE, "could not store logs, retry later".into()));
    }
    let ctx = otlp_ctx(&state, &headers, peer);
    let (_, errors) = accept_chunks(&state, &project, &ctx, error_items).await?;
    Ok(otlp_reply(Signal::Logs, proto, 0, errors.first().map(String::as_str).unwrap_or("")))
}

/// Metrics are not stored; accept them so a Collector or SDK with all signals enabled does not retry.
async fn otlp_metrics(State(state): State<AppState>, Query(q): Query<KeyQuery>, headers: HeaderMap) -> Result<Response, IngestError> {
    authenticate(&state, &headers, &q).await?;
    let proto = !headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").contains("json");
    Ok(if proto {
        (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "application/x-protobuf")], Vec::<u8>::new()).into_response()
    } else {
        (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "application/json")], "{}").into_response()
    })
}

pub async fn current_salt(state: &AppState) -> String {
    let today = crate::util::iso_time(now_ms())[..10].to_string();
    {
        let s = state.salt.read().await;
        if s.0 == today {
            return s.1.clone();
        }
    }
    let salt = match crate::maintenance::salt_for_day(state, &today).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("could not load daily salt: {e:#}");
            return state.salt.read().await.1.clone();
        }
    };
    *state.salt.write().await = (today, salt.clone());
    salt
}

/// Hash a mobile/app install id so it is never stored raw (stable, so users can be counted).
pub fn hash_install_id(project_id: &str, anon_id: &str) -> String {
    sha256_hex(&format!("install|{project_id}|{anon_id}"))[..16].to_string()
}

/// Anonymous, non-reversible visitor id that changes every day (Plausible-style).
pub fn visitor_id(salt: &str, project_id: &str, ctx: &RequestCtx, anon_id: Option<&str>) -> String {
    let input = match anon_id.filter(|a| !a.is_empty()) {
        // Mobile apps send a random install id; hash it so it is never stored raw.
        Some(a) => return hash_install_id(project_id, a),
        None => format!(
            "{salt}|{project_id}|{}|{}",
            ctx.ip.map(|i| i.to_string()).unwrap_or_default(),
            ctx.user_agent
        ),
    };
    sha256_hex(&input)[..16].to_string()
}

struct Ua {
    browser: String,
    browser_version: String,
    os_name: String,
    os_version: String,
    device_type: String,
    bot: bool,
}

fn parse_ua(ua: &str) -> Ua {
    let parser = woothee::parser::Parser::new();
    match parser.parse(ua) {
        Some(r) => Ua {
            browser: if r.name == "UNKNOWN" { String::new() } else { r.name.to_string() },
            browser_version: r.version.split('.').next().unwrap_or("").to_string(),
            os_name: if r.os == "UNKNOWN" { String::new() } else { r.os.to_string() },
            os_version: if r.os_version == "UNKNOWN" { String::new() } else { r.os_version.to_string() },
            device_type: match r.category {
                "pc" => "desktop",
                "smartphone" | "mobilephone" => "mobile",
                "appliance" => "other",
                _ => "",
            }
            .to_string(),
            bot: r.category == "crawler",
        },
        None => Ua {
            browser: String::new(),
            browser_version: String::new(),
            os_name: String::new(),
            os_version: String::new(),
            device_type: String::new(),
            bot: ua.to_lowercase().contains("bot"),
        },
    }
}

fn session_row(project: &ProjectInfo, s: &SessionItem, salt: &str, ctx: &RequestCtx) -> SessionRow {
    let now = now_ms();
    let ts = s.timestamp.as_ref().and_then(parse_timestamp).unwrap_or(now);
    let started = s.started.as_ref().and_then(parse_timestamp).unwrap_or(ts);
    SessionRow {
        session_id: truncate(&s.session_id, 128),
        project_id: project.id.clone(),
        distinct_id: s
            .user_id
            .clone()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| visitor_id(salt, &project.id, ctx, s.anon_id.as_deref())),
        user_id: s.user_id.clone().unwrap_or_default(),
        release: s.release.clone().unwrap_or_default(),
        environment: s.environment.clone().unwrap_or_else(|| "production".into()),
        platform: s.platform.clone().unwrap_or_else(|| project.platform.clone()),
        status: match s.status.as_deref().unwrap_or("ok") {
            "exited" => "exited",
            "crashed" => "crashed",
            "errored" => "errored",
            "abnormal" => "abnormal",
            _ => "ok",
        }
        .to_string(),
        started: ch_time(started),
        timestamp: ch_time(ts.max(started)),
        duration_ms: s.duration_ms.unwrap_or_else(|| (ts - started).max(0) as u64),
        error_count: s.errors.unwrap_or(0),
        flags: crate::models::flag_map(&s.flags),
        retention_days: project.retention_days as u16,
    }
}

fn event_row(project: &ProjectInfo, e: &EventItem, salt: &str, ctx: &RequestCtx) -> Option<EventRow> {
    let ua = parse_ua(&ctx.user_agent);
    let is_web = e.url.is_some();
    if ua.bot && is_web {
        return None;
    }
    let ts = e.timestamp.as_ref().and_then(parse_timestamp).unwrap_or_else(now_ms);
    let (hostname, path, query) = match e.url.as_deref().and_then(|u| url::Url::parse(u).ok()) {
        Some(u) => (
            u.host_str().unwrap_or("").to_string(),
            u.path().to_string(),
            u.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
        ),
        None => (String::new(), e.screen.clone().unwrap_or_default(), BTreeMap::new()),
    };
    let mut referrer_host = e
        .referrer
        .as_deref()
        .and_then(|r| url::Url::parse(r).ok())
        .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()))
        .unwrap_or_default();
    if !hostname.is_empty() && referrer_host == hostname.trim_start_matches("www.") {
        referrer_host.clear(); // internal navigation, not a referral
    }
    let device = e.device.clone().unwrap_or_default();
    let ty = match e.name.as_str() {
        "pageview" => "pageview",
        "screen_view" => "screen_view",
        _ => "custom",
    };
    let q = |k: &str| query.get(k).cloned().unwrap_or_default();
    let mut props: BTreeMap<String, String> = e
        .props
        .iter()
        .take(64)
        .map(|(k, v)| (truncate(k, 128), truncate(&value_to_string(v), 1024)))
        .collect();
    if let Some(screen) = &e.screen {
        props.entry("screen".into()).or_insert_with(|| screen.clone());
    }
    Some(EventRow {
        event_id: e.event_id.clone().filter(|i| uuid::Uuid::parse_str(i).is_ok()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        project_id: project.id.clone(),
        timestamp: ch_time(ts),
        r#type: ty.into(),
        name: truncate(&e.name, 200),
        url: truncate(e.url.as_deref().unwrap_or(""), 2048),
        hostname,
        path: truncate(&path, 1024),
        referrer: truncate(e.referrer.as_deref().unwrap_or(""), 2048),
        referrer_host,
        utm_source: q("utm_source"),
        utm_medium: q("utm_medium"),
        utm_campaign: q("utm_campaign"),
        utm_term: q("utm_term"),
        utm_content: q("utm_content"),
        country: ctx.country.clone(),
        device_type: if ua.device_type.is_empty() && device.model.is_some() { "mobile".into() } else { ua.device_type },
        browser: ua.browser,
        browser_version: ua.browser_version,
        os_name: device.os_name.clone().unwrap_or(ua.os_name),
        os_version: device.os_version.clone().unwrap_or(ua.os_version),
        device_model: device.model.clone().unwrap_or_default(),
        app_version: device.app_version.clone().unwrap_or_default(),
        release: e.release.clone().unwrap_or_default(),
        environment: e.environment.clone().unwrap_or_else(|| "production".into()),
        platform: e.platform.clone().unwrap_or_else(|| project.platform.clone()),
        visitor_id: visitor_id(salt, &project.id, ctx, e.anon_id.as_deref()),
        user_id: e.user_id.clone().unwrap_or_default(),
        session_id: e.session_id.clone().unwrap_or_default(),
        props,
        flags: crate::models::flag_map(&e.flags),
        retention_days: project.retention_days as u16,
    })
    .map(|mut r| {
        if r.event_id.is_empty() {
            r.event_id = new_id();
        }
        r
    })
}
