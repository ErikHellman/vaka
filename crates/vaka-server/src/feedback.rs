//! User feedback: short messages from end users, sent from in-app widgets, post-crash prompts or
//! the API, optionally with screenshots and a contact email. Each item carries the session, user,
//! release, device and feature flags it was sent with and, from a crash prompt, the crash's issue.
//!
//! Feedback lives in the metadata DB (low volume, mutable triage fields). Screenshots are stored
//! next to debug files in the artifact directory and mirrored to S3 when configured. A background
//! worker links crash-prompt feedback to its issue, tags sentiment and topics with the configured
//! LLM, and fires `new_feedback` alerts.

use crate::auth::{ApiError, Principal};
use crate::db::RowExt;
use crate::ingest::RequestCtx;
use crate::models::{Device, UserRef};
use crate::query::{project_for, QResult};
use crate::state::{AppState, ProjectInfo};
use crate::util::{iso_time, new_id, now_ms, parse_time_expr, sha256_hex, truncate};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

pub const MAX_MESSAGE_CHARS: usize = 5000;
pub const MAX_ATTACHMENTS: usize = 3;
pub const MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_IMAGE_SIDE: u32 = 4096;
pub const STATUSES: &[&str] = &["new", "triaged", "resolved", "ignored"];
pub const SENTIMENTS: &[&str] = &["positive", "neutral", "negative", "mixed"];
pub const SOURCES: &[&str] = &["widget", "crash_prompt", "api"];
pub const SUMMARY_DIMENSIONS: &[&str] = &["sentiment", "topic", "release", "flag", "day", "status", "platform", "source"];

/// A feedback item as SDKs send it (in a batch with `"type": "feedback"`, or as the `feedback`
/// part of a multipart upload).
#[derive(Debug, Default, Deserialize)]
pub struct FeedbackItem {
    /// Client-generated id (UUID recommended): retries with the same id are deduplicated.
    #[serde(default, alias = "feedback_id")]
    pub id: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default, alias = "contact_email")]
    pub email: Option<String>,
    #[serde(default, alias = "contact_name")]
    pub name: Option<String>,
    /// The error event this feedback is about (crash prompts).
    #[serde(default)]
    pub event_id: Option<String>,
    /// widget | crash_prompt | api
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub timestamp: Option<Value>,
    #[serde(default)]
    pub user: Option<UserRef>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub anon_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Screen name (mobile); stored in `url` when no URL is given.
    #[serde(default)]
    pub screen: Option<String>,
    #[serde(default)]
    pub device: Option<Device>,
    #[serde(default)]
    pub tags: BTreeMap<String, Value>,
    #[serde(default)]
    pub flags: BTreeMap<String, Value>,
}

/// An uploaded file (multipart part).
pub struct Upload {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum Reject {
    Bad(String),
    RateLimited(String),
    Unavailable(String),
}

impl Reject {
    pub fn message(&self) -> &str {
        match self {
            Reject::Bad(m) | Reject::RateLimited(m) | Reject::Unavailable(m) => m,
        }
    }
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ImageInfo {
    pub content_type: &'static str,
    pub ext: &'static str,
    pub width: u32,
    pub height: u32,
}

fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}

fn le16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}

fn le24(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, 0]))
}

/// Identify a PNG, JPEG or WebP image by its bytes and read its dimensions.
pub fn inspect_image(b: &[u8]) -> Result<ImageInfo, String> {
    let bad = || "attachments must be PNG, JPEG or WebP images".to_string();
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes(b.get(16..20).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
        let h = u32::from_be_bytes(b.get(20..24).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
        return Ok(ImageInfo { content_type: "image/png", ext: "png", width: w, height: h });
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xFF {
                return Err(bad());
            }
            let marker = b[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
                i += 2;
                continue;
            }
            let len = be16(b, i + 2).ok_or_else(bad)? as usize;
            if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = be16(b, i + 5).ok_or_else(bad)?;
                let w = be16(b, i + 7).ok_or_else(bad)?;
                return Ok(ImageInfo { content_type: "image/jpeg", ext: "jpg", width: w, height: h });
            }
            if marker == 0xDA || len < 2 {
                break;
            }
            i += 2 + len;
        }
        return Err(bad());
    }
    if b.len() >= 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        let (w, h) = match &b[12..16] {
            b"VP8 " => (le16(b, 26).ok_or_else(bad)? & 0x3fff, le16(b, 28).ok_or_else(bad)? & 0x3fff),
            b"VP8L" => {
                let (b1, b2, b3, b4) = (b[21] as u32, b[22] as u32, b[23] as u32, b[24] as u32);
                (1 + (b1 | ((b2 & 0x3F) << 8)), 1 + ((b2 >> 6) | (b3 << 2) | ((b4 & 0x0F) << 10)))
            }
            b"VP8X" => (1 + le24(b, 24).ok_or_else(bad)?, 1 + le24(b, 27).ok_or_else(bad)?),
            _ => return Err(bad()),
        };
        return Ok(ImageInfo { content_type: "image/webp", ext: "webp", width: w, height: h });
    }
    Err(bad())
}

/// Drop APP1 (EXIF, XMP) and APP13 (IPTC) segments from a JPEG: they can hold GPS
/// coordinates, device serials and names.
pub fn strip_jpeg_metadata(b: &[u8]) -> Vec<u8> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return b.to_vec();
    }
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&b[..2]);
    let mut i = 2;
    while i + 4 <= b.len() && b[i] == 0xFF {
        let marker = b[i + 1];
        if marker == 0xDA {
            break;
        }
        let Some(len) = be16(b, i + 2).map(|l| l as usize) else { break };
        let end = (i + 2 + len).min(b.len());
        if !matches!(marker, 0xE1 | 0xED) {
            out.extend_from_slice(&b[i..end]);
        }
        i = end;
    }
    out.extend_from_slice(&b[i.min(b.len())..]);
    out
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

static WINDOWS: Lazy<Mutex<HashMap<String, (i64, u32)>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Fixed one-hour windows kept in memory. Returns false when `key` is over `limit`.
fn take(key: &str, limit: u32, now: i64) -> bool {
    if limit == 0 {
        return true;
    }
    let window = now / 3_600_000;
    let mut m = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 50_000 {
        m.retain(|_, (w, _)| *w == window);
    }
    let e = m.entry(key.to_string()).or_insert((window, 0));
    if e.0 != window {
        *e = (window, 0);
    }
    if e.1 >= limit {
        return false;
    }
    e.1 += 1;
    true
}

fn check_rate(state: &AppState, project: &ProjectInfo, ctx: &RequestCtx, session_id: &str) -> Result<(), Reject> {
    let now = now_ms();
    let ip = ctx.ip.map(|i| sha256_hex(&i.to_string())).unwrap_or_default();
    if !ip.is_empty() && !take(&format!("{}|ip|{ip}", project.id), state.config.feedback_ip_limit, now) {
        return Err(Reject::RateLimited(format!("too much feedback from this address: the limit is {} per hour", state.config.feedback_ip_limit)));
    }
    if !session_id.is_empty() && !take(&format!("{}|s|{session_id}", project.id), state.config.feedback_session_limit, now) {
        return Err(Reject::RateLimited(format!("too much feedback in this session: the limit is {} per hour", state.config.feedback_session_limit)));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

pub fn short_id(project_slug: &str, n: i64) -> String {
    format!("{}-F{n}", project_slug.to_uppercase())
}

fn valid_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else { return false };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && e.len() <= 254 && !e.contains(char::is_whitespace)
}

fn llm_enabled(state: &AppState) -> bool {
    state.config.llm_base_url.is_some() && state.config.llm_model.is_some()
}

/// Validate, store and (for crash prompts) link one feedback item with its attachments.
pub async fn ingest(state: &AppState, project: &ProjectInfo, ctx: &RequestCtx, item: FeedbackItem, uploads: Vec<Upload>) -> Result<Value, Reject> {
    let message = item.message.as_deref().unwrap_or("").trim().to_string();
    if message.is_empty() {
        return Err(Reject::Bad("feedback needs a non-empty `message`".into()));
    }
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(Reject::Bad(format!("`message` is longer than {MAX_MESSAGE_CHARS} characters")));
    }
    if uploads.len() > MAX_ATTACHMENTS {
        return Err(Reject::Bad(format!("at most {MAX_ATTACHMENTS} attachments per feedback")));
    }
    let mut images = Vec::new();
    for u in &uploads {
        if u.bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(Reject::Bad(format!("attachment `{}` is larger than 5 MB", u.name)));
        }
        let info = inspect_image(&u.bytes).map_err(|e| Reject::Bad(format!("attachment `{}`: {e}", u.name)))?;
        if info.width == 0 || info.height == 0 || info.width > MAX_IMAGE_SIDE || info.height > MAX_IMAGE_SIDE {
            return Err(Reject::Bad(format!("attachment `{}` is {}x{} px; the maximum is {MAX_IMAGE_SIDE} px a side", u.name, info.width, info.height)));
        }
        images.push(info);
    }
    let email = item.email.as_deref().unwrap_or("").trim().to_lowercase();
    if !email.is_empty() && !valid_email(&email) {
        return Err(Reject::Bad("`email` is not a valid email address".into()));
    }
    let client_id = item.id.as_deref().map(str::trim).filter(|i| !i.is_empty()).map(|i| truncate(i, 64)).unwrap_or_else(new_id);
    let existing = sqlx::query("SELECT f.id, f.short_id FROM feedback f WHERE f.project_id = $1 AND f.client_id = $2")
        .bind(&project.id)
        .bind(&client_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| Reject::Unavailable(e.to_string()))?;
    if let Some(r) = existing {
        return Ok(json!({"id": r.s("id"), "short_id": short_id(&project.slug, r.i("short_id")), "duplicate": true}));
    }
    let session_id = truncate(item.session_id.as_deref().unwrap_or(""), 128);
    check_rate(state, project, ctx, &session_id)?;

    let (message, tags) = {
        let mut tags = serde_json::to_value(&item.tags).unwrap_or(Value::Null);
        let msg = if project.scrub_enabled {
            let s = crate::scrub::Scrubber::new(&project.scrub_fields);
            s.scrub_value(&mut tags);
            s.scrub_text(&message)
        } else {
            message
        };
        let tags: BTreeMap<String, String> = tags
            .as_object()
            .map(|o| o.iter().take(32).map(|(k, v)| (truncate(k, 64), truncate(&crate::models::value_to_string(v), 256))).collect())
            .unwrap_or_default();
        (msg, tags)
    };
    let source = item.source.as_deref().filter(|s| SOURCES.contains(s)).unwrap_or(if uploads.is_empty() { "api" } else { "widget" }).to_string();
    let device = item.device.clone().unwrap_or_default();
    let browser = if ctx.user_agent.is_empty() {
        String::new()
    } else {
        woothee::parser::Parser::new().parse(&ctx.user_agent).map(|r| r.name.to_string()).filter(|n| n != "UNKNOWN").unwrap_or_default()
    };
    let device_json = json!({
        "model": device.model, "os_name": device.os_name, "os_version": device.os_version,
        "app_version": device.app_version, "browser": if browser.is_empty() { Value::Null } else { Value::String(browser) },
    });
    let url = item.url.clone().filter(|u| !u.is_empty()).or_else(|| item.screen.clone()).unwrap_or_default();
    let url = match project.scrub_enabled {
        true => crate::scrub::Scrubber::new(&project.scrub_fields).scrub_text(&url),
        false => url,
    };
    let event_id = item.event_id.as_deref().filter(|e| uuid::Uuid::parse_str(e).is_ok()).map(|e| e.to_lowercase()).unwrap_or_default();
    let issue_id = if event_id.is_empty() { None } else { issue_for_event(state, &project.id, &event_id).await };
    let now = now_ms();
    let created = item.timestamp.as_ref().and_then(crate::util::parse_timestamp).filter(|t| *t <= now + 60_000).unwrap_or(now);
    let user_id = item.user_id.clone().or_else(|| item.user.as_ref().and_then(|u| u.id.clone())).unwrap_or_default();
    let anon_id = item.anon_id.as_deref().filter(|a| !a.is_empty()).map(|a| crate::ingest::hash_install_id(&project.id, a)).unwrap_or_default();
    let id = new_id();
    let sdk = if ctx.sdk.name.is_empty() { String::new() } else { format!("{}/{}", ctx.sdk.name, ctx.sdk.version) };

    let mut short = 0;
    for attempt in 0..5 {
        short = sqlx::query("SELECT COALESCE(MAX(short_id), 0) AS m FROM feedback WHERE project_id = $1")
            .bind(&project.id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| Reject::Unavailable(e.to_string()))?
            .i("m")
            + 1;
        let res = sqlx::query(
            "INSERT INTO feedback (id, project_id, client_id, short_id, message, contact_email, contact_name, user_id, anon_id, session_id, release, environment, \
             platform, url, device, country, sdk, flags, tags, event_id, issue_id, source, status, tag_state, attachment_count, created_at, received_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, 'new', $23, $24, $25, $26, $26)",
        )
        .bind(&id)
        .bind(&project.id)
        .bind(&client_id)
        .bind(short)
        .bind(&message)
        .bind(&email)
        .bind(truncate(item.name.as_deref().unwrap_or("").trim(), 128))
        .bind(truncate(&user_id, 256))
        .bind(&anon_id)
        .bind(&session_id)
        .bind(truncate(item.release.as_deref().unwrap_or(""), 256))
        .bind(truncate(item.environment.as_deref().unwrap_or("production"), 64))
        .bind(truncate(item.platform.as_deref().unwrap_or(&project.platform), 64))
        .bind(truncate(&url, 2048))
        .bind(device_json.to_string())
        .bind(&ctx.country)
        .bind(sdk.clone())
        .bind(serde_json::to_string(&crate::models::flag_map(&item.flags)).unwrap_or_default())
        .bind(serde_json::to_string(&tags).unwrap_or_default())
        .bind(&event_id)
        .bind(issue_id.clone())
        .bind(&source)
        .bind(if llm_enabled(state) { "pending" } else { "skipped" })
        .bind(images.len() as i64)
        .bind(created)
        .bind(now)
        .execute(&state.db)
        .await;
        match res {
            Ok(_) => break,
            Err(e) if attempt < 4 && e.to_string().to_lowercase().contains("unique") => {
                // Same client id from a concurrent retry, or a short id race.
                if let Ok(Some(r)) = sqlx::query("SELECT id, short_id FROM feedback WHERE project_id = $1 AND client_id = $2").bind(&project.id).bind(&client_id).fetch_optional(&state.db).await {
                    return Ok(json!({"id": r.s("id"), "short_id": short_id(&project.slug, r.i("short_id")), "duplicate": true}));
                }
                continue;
            }
            Err(e) => return Err(Reject::Unavailable(e.to_string())),
        }
    }

    if !uploads.is_empty() {
        let dir = format!("{}/feedback/{}/{}", state.config.artifact_dir(), project.id, id);
        if let Err(e) = store_attachments(state, project, &id, &dir, uploads, &images, now).await {
            tracing::error!("storing feedback attachments failed: {e:#}");
            let _ = sqlx::query("DELETE FROM feedback_attachments WHERE feedback_id = $1").bind(&id).execute(&state.db).await;
            let _ = sqlx::query("DELETE FROM feedback WHERE id = $1").bind(&id).execute(&state.db).await;
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Err(Reject::Unavailable("could not store the attachments, retry later".into()));
        }
    }
    Ok(json!({"id": id, "short_id": short_id(&project.slug, short), "issue_id": issue_id}))
}

async fn store_attachments(state: &AppState, project: &ProjectInfo, feedback_id: &str, dir: &str, uploads: Vec<Upload>, images: &[ImageInfo], now: i64) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    for (pos, (u, info)) in uploads.into_iter().zip(images).enumerate() {
        let bytes = if info.ext == "jpg" { strip_jpeg_metadata(&u.bytes) } else { u.bytes };
        let path = format!("{dir}/{pos}.{}", info.ext);
        tokio::fs::write(&path, &bytes).await?;
        crate::s3::store_artifact(state, &path).await?;
        sqlx::query(
            "INSERT INTO feedback_attachments (id, feedback_id, project_id, position, name, content_type, size, sha256, width, height, path, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(new_id())
        .bind(feedback_id)
        .bind(&project.id)
        .bind(pos as i64)
        .bind(truncate(&u.name, 200))
        .bind(info.content_type)
        .bind(bytes.len() as i64)
        .bind(hex::encode(Sha256::digest(&bytes)))
        .bind(info.width as i64)
        .bind(info.height as i64)
        .bind(&path)
        .bind(now)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

/// The issue an error event was grouped into, once the event has reached ClickHouse.
async fn issue_for_event(state: &AppState, project_id: &str, event_id: &str) -> Option<String> {
    let rows = state
        .ch
        .query(
            "SELECT issue_id FROM errors WHERE project_id = {p:String} AND event_id = toUUIDOrZero({e:String}) LIMIT 1",
            &[("p", project_id.to_string()), ("e", event_id.to_string())],
        )
        .await
        .ok()?;
    rows.first().and_then(|r| r["issue_id"].as_str()).filter(|i| !i.is_empty()).map(String::from)
}

// ---------------------------------------------------------------------------
// Query layer (shared by REST and MCP)
// ---------------------------------------------------------------------------

const SELECT: &str = "SELECT f.id, f.project_id, f.short_id, f.message, f.contact_email, f.contact_name, f.user_id, f.session_id, f.release, f.environment, \
    f.platform, f.url, f.device, f.country, f.sdk, f.flags, f.tags, f.event_id, f.issue_id, f.source, f.status, f.sentiment, f.topics, f.summary, \
    f.tag_state, f.attachment_count, f.created_at, f.updated_at, p.slug AS project_slug, u.email AS assignee_email, i.short_id AS issue_short \
    FROM feedback f JOIN projects p ON p.id = f.project_id LEFT JOIN users u ON u.id = f.assignee_id LEFT JOIN issues i ON i.id = f.issue_id";

fn parse_json(s: &str) -> Value {
    if s.is_empty() {
        return json!({});
    }
    serde_json::from_str(s).unwrap_or_else(|_| json!({}))
}

pub fn topics_of(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn row_json(state: &AppState, r: &sqlx::any::AnyRow, include_contact: bool) -> Value {
    let slug = r.s("project_slug");
    let id = r.s("id");
    let n = r.i("attachment_count");
    let attachments: Vec<Value> = (0..n).map(|i| json!({"index": i, "url": format!("{}/api/v1/feedback/{id}/attachments/{i}", state.config.public_url)})).collect();
    let mut v = json!({
        "id": id,
        "short_id": short_id(&slug, r.i("short_id")),
        "project_id": r.s("project_id"),
        "project": slug,
        "message": r.s("message"),
        "status": r.s("status"),
        "sentiment": r.os("sentiment"),
        "topics": topics_of(&r.s("topics")),
        "summary": r.os("summary").filter(|s| !s.is_empty()),
        "tagging": r.s("tag_state"),
        "source": r.s("source"),
        "release": r.s("release"),
        "environment": r.s("environment"),
        "platform": r.s("platform"),
        "url": r.s("url"),
        "device": parse_json(&r.s("device")),
        "country": r.s("country"),
        "sdk": r.s("sdk"),
        "flags": parse_json(&r.s("flags")),
        "tags": parse_json(&r.s("tags")),
        "user_id": r.os("user_id").filter(|s| !s.is_empty()),
        "session_id": r.os("session_id").filter(|s| !s.is_empty()),
        "event_id": r.os("event_id").filter(|s| !s.is_empty()),
        "issue_id": r.os("issue_id"),
        "issue_short_id": r.oi("issue_short").map(|n| crate::query::short_id(&slug, n)),
        "assignee": r.os("assignee_email"),
        "attachments": attachments,
        "created_at": iso_time(r.i("created_at")),
        "updated_at": iso_time(r.i("updated_at")),
    });
    let email = r.s("contact_email");
    let name = r.s("contact_name");
    v["has_contact"] = json!(!email.is_empty());
    if include_contact {
        v["contact_email"] = if email.is_empty() { Value::Null } else { Value::String(email) };
        v["contact_name"] = if name.is_empty() { Value::Null } else { Value::String(name) };
    }
    v
}

/// Load one feedback row by id or short id (`WEB-F12`), checking read access.
async fn find(state: &AppState, p: &Principal, id: &str) -> QResult<sqlx::any::AnyRow> {
    let short = id.rsplit_once("-F").or_else(|| id.rsplit_once("-f")).and_then(|(s, n)| Some((s.to_lowercase(), n.parse::<i64>().ok()?)));
    let row = match short {
        Some((slug, n)) => {
            sqlx::query(&format!("{SELECT} WHERE (p.slug = $1 AND f.short_id = $2) OR f.id = $3")).bind(slug).bind(n).bind(id).fetch_optional(&state.db).await?
        }
        None => sqlx::query(&format!("{SELECT} WHERE f.id = $1")).bind(id).fetch_optional(&state.db).await?,
    };
    let row = row.ok_or_else(|| ApiError::not_found("feedback"))?;
    if !p.can_read(&row.s("project_id")) {
        return Err(ApiError::not_found("feedback"));
    }
    Ok(row)
}

enum Bind {
    S(String),
    I(i64),
}

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub project: String,
    /// open (new + triaged, default), all, or a comma-separated list of new, triaged, resolved, ignored.
    pub status: Option<String>,
    /// Comma-separated: positive, neutral, negative, mixed, untagged.
    pub sentiment: Option<String>,
    pub topic: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub platform: Option<String>,
    /// widget, crash_prompt or api.
    pub source: Option<String>,
    /// A flag key (feedback sent with the flag set) or key=variant.
    pub flag: Option<String>,
    /// Issue id or short id: feedback linked to that issue.
    pub issue: Option<String>,
    pub user: Option<String>,
    pub session: Option<String>,
    pub has_screenshot: Option<bool>,
    /// Text search in the message.
    pub query: Option<String>,
    /// Time window, e.g. "24h", "7d" (default 30d).
    pub since: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

fn csv(s: &Option<String>) -> Vec<String> {
    s.as_deref().unwrap_or("").split(',').map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect()
}

pub async fn list(state: &AppState, p: &Principal, q: ListParams) -> QResult<Value> {
    let project = project_for(state, p, &q.project).await?;
    let now = now_ms();
    let since = parse_time_expr(q.since.as_deref().unwrap_or("30d"), now).ok_or_else(|| ApiError::bad("`since` must look like 24h, 7d or an ISO time"))?;
    let mut wh = vec!["f.project_id = $1".to_string(), "f.created_at >= $2".to_string()];
    let mut binds = vec![Bind::S(project.id.clone()), Bind::I(since)];
    let push = |cond: &str, b: Bind, binds: &mut Vec<Bind>, wh: &mut Vec<String>| {
        binds.push(b);
        wh.push(cond.replace('?', &format!("${}", binds.len())));
    };
    let statuses = match q.status.as_deref().unwrap_or("open") {
        "all" => vec![],
        "open" => vec!["new".to_string(), "triaged".to_string()],
        _ => csv(&q.status),
    };
    if let Some(bad) = statuses.iter().find(|s| !STATUSES.contains(&s.as_str())) {
        return Err(ApiError::bad(format!("unknown status `{bad}`: use open, all, or {}", STATUSES.join(", "))));
    }
    if !statuses.is_empty() {
        let list: Vec<String> = statuses.iter().map(|s| format!("'{s}'")).collect();
        wh.push(format!("f.status IN ({})", list.join(", ")));
    }
    let sentiments = csv(&q.sentiment);
    if !sentiments.is_empty() {
        let mut parts = Vec::new();
        for s in &sentiments {
            match s.as_str() {
                "untagged" => parts.push("f.sentiment IS NULL".to_string()),
                x if SENTIMENTS.contains(&x) => parts.push(format!("f.sentiment = '{x}'")),
                other => return Err(ApiError::bad(format!("unknown sentiment `{other}`: use {}, untagged", SENTIMENTS.join(", ")))),
            }
        }
        wh.push(format!("({})", parts.join(" OR ")));
    }
    if let Some(t) = q.topic.as_deref().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
        push("f.topics LIKE ?", Bind::S(format!("%{}%", serde_json::to_string(&t).unwrap_or_default())), &mut binds, &mut wh);
    }
    for (val, col) in [(&q.release, "f.release"), (&q.environment, "f.environment"), (&q.platform, "f.platform"), (&q.source, "f.source"), (&q.user, "f.user_id"), (&q.session, "f.session_id")] {
        if let Some(v) = val.as_deref().filter(|v| !v.is_empty()) {
            push(&format!("{col} = ?"), Bind::S(v.to_string()), &mut binds, &mut wh);
        }
    }
    if let Some(f) = q.flag.as_deref().filter(|f| !f.is_empty()) {
        let pat = match f.split_once('=') {
            Some((k, v)) => format!("%{}:{}%", serde_json::to_string(k).unwrap_or_default(), serde_json::to_string(v).unwrap_or_default()),
            None => format!("%{}:%", serde_json::to_string(f).unwrap_or_default()),
        };
        push("f.flags LIKE ?", Bind::S(pat), &mut binds, &mut wh);
    }
    if let Some(i) = q.issue.as_deref().filter(|i| !i.is_empty()) {
        let issue = crate::query::issue_for(state, p, i).await?;
        push("f.issue_id = ?", Bind::S(issue["id"].as_str().unwrap_or("").to_string()), &mut binds, &mut wh);
    }
    match q.has_screenshot {
        Some(true) => wh.push("f.attachment_count > 0".into()),
        Some(false) => wh.push("f.attachment_count = 0".into()),
        None => {}
    }
    if let Some(t) = q.query.as_deref().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()) {
        push("LOWER(f.message) LIKE ?", Bind::S(format!("%{t}%")), &mut binds, &mut wh);
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let sql = format!("{SELECT} WHERE {} ORDER BY f.created_at DESC LIMIT {} OFFSET {offset}", wh.join(" AND "), limit + 1);
    let mut query = sqlx::query(&sql);
    for b in binds {
        query = match b {
            Bind::S(s) => query.bind(s),
            Bind::I(i) => query.bind(i),
        };
    }
    let rows = query.fetch_all(&state.db).await?;
    let include_contact = p.can_write(&project.id);
    let has_more = rows.len() as i64 > limit;
    let items: Vec<Value> = rows.iter().take(limit as usize).map(|r| row_json(state, r, include_contact)).collect();
    Ok(json!({"project": project.slug, "feedback": items, "count": items.len(), "has_more": has_more, "next_offset": if has_more { json!(offset + limit) } else { Value::Null }}))
}

pub async fn get(state: &AppState, p: &Principal, id: &str) -> QResult<Value> {
    let row = find(state, p, id).await?;
    let project_id = row.s("project_id");
    let mut v = row_json(state, &row, p.can_write(&project_id));
    let atts = sqlx::query("SELECT position, name, content_type, size, width, height FROM feedback_attachments WHERE feedback_id = $1 ORDER BY position")
        .bind(row.s("id"))
        .fetch_all(&state.db)
        .await?;
    v["attachments"] = Value::Array(
        atts.iter()
            .map(|a| {
                json!({
                    "index": a.i("position"), "name": a.s("name"), "content_type": a.s("content_type"), "size": a.i("size"),
                    "width": a.i("width"), "height": a.i("height"),
                    "url": format!("{}/api/v1/feedback/{}/attachments/{}", state.config.public_url, row.s("id"), a.i("position")),
                })
            })
            .collect(),
    );
    if let Some(issue_id) = row.os("issue_id") {
        if let Ok(Some(issue)) = crate::query::get_issue_row(state, &issue_id).await {
            v["issue"] = json!({"id": issue["id"], "short_id": issue["short_id"], "title": issue["title"], "status": issue["status"], "times_seen": issue["times_seen"]});
        }
    }
    let slug = row.s("project_slug");
    if let Some(sid) = row.os("session_id").filter(|s| !s.is_empty()) {
        v["session_timeline"] = json!(format!("/api/v1/projects/{slug}/sessions/{sid}/timeline"));
    }
    if let Some(uid) = row.os("user_id").filter(|s| !s.is_empty()) {
        v["user_timeline"] = json!(format!("/api/v1/projects/{slug}/users/{uid}/timeline"));
    }
    Ok(v)
}

/// The bytes of one attachment: (content type, bytes).
pub async fn attachment(state: &AppState, p: &Principal, id: &str, index: i64) -> QResult<(String, Vec<u8>)> {
    let row = find(state, p, id).await?;
    let a = sqlx::query("SELECT content_type, path FROM feedback_attachments WHERE feedback_id = $1 AND position = $2")
        .bind(row.s("id"))
        .bind(index)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("attachment"))?;
    let path = a.s("path");
    if !crate::s3::ensure_local(state, &path).await {
        return Err(ApiError::not_found("attachment file"));
    }
    let bytes = tokio::fs::read(&path).await.map_err(anyhow::Error::from)?;
    Ok((a.s("content_type"), bytes))
}

#[derive(Debug, Default, Deserialize)]
pub struct Update {
    /// new | triaged | resolved | ignored
    pub status: Option<String>,
    /// Email of a user, or "" to unassign.
    pub assignee: Option<String>,
    /// positive | neutral | negative | mixed, or "" to clear.
    pub sentiment: Option<String>,
    /// Replaces the topic tags.
    pub topics: Option<Vec<String>>,
    /// Issue id or short id to link, or "" to unlink.
    pub issue: Option<String>,
}

pub async fn update(state: &AppState, p: &Principal, id: &str, u: Update) -> QResult<Value> {
    let row = find(state, p, id).await?;
    let fid = row.s("id");
    let project_id = row.s("project_id");
    if !p.can_write(&project_id) {
        return Err(ApiError::forbidden());
    }
    let now = now_ms();
    if let Some(s) = &u.status {
        if !STATUSES.contains(&s.as_str()) {
            return Err(ApiError::bad(format!("unknown status `{s}`: use {}", STATUSES.join(", "))));
        }
        sqlx::query("UPDATE feedback SET status = $1, updated_at = $2 WHERE id = $3").bind(s).bind(now).bind(&fid).execute(&state.db).await?;
    }
    if let Some(a) = &u.assignee {
        let user_id = if a.is_empty() {
            None
        } else {
            let r = sqlx::query("SELECT id FROM users WHERE email = $1").bind(a.to_lowercase()).fetch_optional(&state.db).await?;
            Some(r.ok_or_else(|| ApiError::bad(format!("no user with email {a}")))?.s("id"))
        };
        sqlx::query("UPDATE feedback SET assignee_id = $1, updated_at = $2 WHERE id = $3").bind(user_id).bind(now).bind(&fid).execute(&state.db).await?;
    }
    if let Some(s) = &u.sentiment {
        let val = match s.as_str() {
            "" => None,
            x if SENTIMENTS.contains(&x) => Some(x.to_string()),
            other => return Err(ApiError::bad(format!("unknown sentiment `{other}`: use {}", SENTIMENTS.join(", ")))),
        };
        sqlx::query("UPDATE feedback SET sentiment = $1, tag_state = 'manual', updated_at = $2 WHERE id = $3").bind(val).bind(now).bind(&fid).execute(&state.db).await?;
    }
    if let Some(t) = &u.topics {
        let topics = clean_topics(t);
        sqlx::query("UPDATE feedback SET topics = $1, tag_state = 'manual', updated_at = $2 WHERE id = $3")
            .bind(serde_json::to_string(&topics).unwrap_or_default())
            .bind(now)
            .bind(&fid)
            .execute(&state.db)
            .await?;
    }
    if let Some(i) = &u.issue {
        let issue_id = if i.is_empty() {
            None
        } else {
            let issue = crate::query::issue_for(state, p, i).await?;
            if issue["project_id"].as_str() != Some(project_id.as_str()) {
                return Err(ApiError::bad("the issue belongs to another project"));
            }
            issue["id"].as_str().map(String::from)
        };
        sqlx::query("UPDATE feedback SET issue_id = $1, updated_at = $2 WHERE id = $3").bind(issue_id).bind(now).bind(&fid).execute(&state.db).await?;
    }
    crate::auth::audit(
        state,
        &p.label(),
        "update_feedback",
        &fid,
        &serde_json::to_string(&json!({"status": u.status, "assignee": u.assignee, "sentiment": u.sentiment, "topics": u.topics, "issue": u.issue})).unwrap_or_default(),
    )
    .await;
    get(state, p, &fid).await
}

pub fn clean_topics(t: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in t {
        let x = truncate(x.trim().to_lowercase().trim_matches(|c: char| c == '#' || c == '"'), 40);
        if !x.is_empty() && !out.contains(&x) {
            out.push(x);
        }
    }
    out.truncate(5);
    out
}

#[derive(Debug, Default, Deserialize)]
pub struct SummaryParams {
    #[serde(default)]
    pub project: String,
    /// sentiment (default), topic, release, flag, day, status, platform, source
    pub by: Option<String>,
    /// For by=flag: the flag key to split by variant (omit to count every key=variant pair).
    pub flag: Option<String>,
    /// Time window (default 30d).
    pub since: Option<String>,
    /// Include resolved and ignored feedback too (default true).
    pub include_closed: Option<bool>,
    pub limit: Option<i64>,
}

#[derive(Default)]
struct Bucket {
    count: i64,
    sentiment: BTreeMap<String, i64>,
    quotes: Vec<Value>,
}

/// Counts by a dimension with a sentiment split and sample quotes (negative ones first).
pub async fn summary(state: &AppState, p: &Principal, q: SummaryParams) -> QResult<Value> {
    let project = project_for(state, p, &q.project).await?;
    let by = q.by.as_deref().unwrap_or("sentiment");
    if !SUMMARY_DIMENSIONS.contains(&by) {
        return Err(ApiError::bad(format!("`by` must be one of {}", SUMMARY_DIMENSIONS.join(", "))));
    }
    let now = now_ms();
    let since = parse_time_expr(q.since.as_deref().unwrap_or("30d"), now).ok_or_else(|| ApiError::bad("`since` must look like 24h, 7d or an ISO time"))?;
    let closed = if q.include_closed.unwrap_or(true) { "" } else { " AND status IN ('new', 'triaged')" };
    let rows = sqlx::query(&format!(
        "SELECT id, short_id, message, sentiment, topics, release, flags, status, platform, source, created_at FROM feedback \
         WHERE project_id = $1 AND created_at >= $2{closed} ORDER BY created_at DESC LIMIT 20000"
    ))
    .bind(&project.id)
    .bind(since)
    .fetch_all(&state.db)
    .await?;
    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut total = Bucket::default();
    for r in &rows {
        let sentiment = r.os("sentiment").unwrap_or_else(|| "untagged".into());
        let keys: Vec<String> = match by {
            "sentiment" => vec![sentiment.clone()],
            "topic" => {
                let t = topics_of(&r.s("topics"));
                if t.is_empty() { vec!["(none)".into()] } else { t }
            }
            "release" => vec![Some(r.s("release")).filter(|s| !s.is_empty()).unwrap_or_else(|| "(none)".into())],
            "flag" => {
                let flags: BTreeMap<String, String> = serde_json::from_str(&r.s("flags")).unwrap_or_default();
                match q.flag.as_deref().filter(|f| !f.is_empty()) {
                    Some(k) => vec![flags.get(k).cloned().unwrap_or_else(|| "(not set)".into())],
                    None => flags.iter().map(|(k, v)| format!("{k}={v}")).collect(),
                }
            }
            "day" => vec![iso_time(r.i("created_at"))[..10].to_string()],
            other => vec![r.s(other)],
        };
        let quote = json!({"short_id": short_id(&project.slug, r.i("short_id")), "message": truncate(&r.s("message"), 280), "sentiment": r.os("sentiment"), "created_at": iso_time(r.i("created_at"))});
        total.count += 1;
        *total.sentiment.entry(sentiment.clone()).or_default() += 1;
        for k in keys {
            let b = buckets.entry(k).or_default();
            b.count += 1;
            *b.sentiment.entry(sentiment.clone()).or_default() += 1;
            let slot = b.quotes.len() < 3 || (sentiment == "negative" && b.quotes.iter().any(|q| q["sentiment"] != "negative"));
            if slot {
                if b.quotes.len() >= 3 {
                    if let Some(pos) = b.quotes.iter().rposition(|q| q["sentiment"] != "negative") {
                        b.quotes.remove(pos);
                    }
                }
                b.quotes.push(quote.clone());
            }
        }
    }
    let rate = |b: &Bucket| {
        let tagged: i64 = b.sentiment.iter().filter(|(k, _)| k.as_str() != "untagged").map(|(_, v)| *v).sum();
        if tagged == 0 { Value::Null } else { json!(((*b.sentiment.get("negative").unwrap_or(&0) + *b.sentiment.get("mixed").unwrap_or(&0)) as f64 / tagged as f64 * 1000.0).round() / 1000.0) }
    };
    let mut list: Vec<(String, Bucket)> = buckets.into_iter().collect();
    if by == "day" {
        list.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        list.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(a.0.cmp(&b.0)));
    }
    let limit = q.limit.unwrap_or(25).clamp(1, 200) as usize;
    let out: Vec<Value> = list
        .iter()
        .take(limit)
        .map(|(k, b)| json!({"key": k, "count": b.count, "sentiment": b.sentiment, "negative_rate": rate(b), "quotes": b.quotes}))
        .collect();
    Ok(json!({
        "project": project.slug,
        "by": by,
        "since": iso_time(since),
        "total": total.count,
        "sentiment": total.sentiment,
        "negative_rate": rate(&total),
        "buckets": out,
        "note": "negative_rate = (negative + mixed) / tagged feedback; untagged items are counted but not rated",
    }))
}

/// Feedback count, sentiment split and recent quotes for one issue (issue and fix context views).
pub async fn for_issue(state: &AppState, issue_id: &str) -> Value {
    let rows = sqlx::query("SELECT f.short_id, f.message, f.sentiment, f.created_at, p.slug FROM feedback f JOIN projects p ON p.id = f.project_id WHERE f.issue_id = $1 ORDER BY f.created_at DESC LIMIT 200")
        .bind(issue_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let quotes: Vec<Value> = rows
        .iter()
        .take(3)
        .map(|r| json!({"short_id": short_id(&r.s("slug"), r.i("short_id")), "message": truncate(&r.s("message"), 500), "sentiment": r.os("sentiment"), "created_at": iso_time(r.i("created_at"))}))
        .collect();
    json!({"count": rows.len(), "quotes": quotes})
}

/// Feedback entries for a timeline: one session or one identified user.
pub async fn for_timeline(state: &AppState, project_id: &str, session_id: Option<&str>, user_id: Option<&str>, from: i64, to: i64) -> Vec<Value> {
    let (col, val) = match (session_id, user_id) {
        (Some(s), _) => ("session_id", s),
        (None, Some(u)) => ("user_id", u),
        _ => return vec![],
    };
    let rows = sqlx::query(&format!(
        "SELECT f.id, f.short_id, f.message, f.sentiment, f.source, f.attachment_count, f.created_at, p.slug FROM feedback f JOIN projects p ON p.id = f.project_id \
         WHERE f.project_id = $1 AND f.{col} = $2 AND f.created_at >= $3 AND f.created_at <= $4 ORDER BY f.created_at LIMIT 100"
    ))
    .bind(project_id)
    .bind(val)
    .bind(from)
    .bind(to)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    rows.iter()
        .map(|r| {
            json!({
                "ts": iso_time(r.i("created_at")), "ts_ms": r.i("created_at"), "kind": "feedback",
                "short_id": short_id(&r.s("slug"), r.i("short_id")), "message": truncate(&r.s("message"), 500),
                "sentiment": r.os("sentiment"), "source": r.s("source"), "screenshots": r.i("attachment_count"),
            })
        })
        .collect()
}

/// Feedback counts and negative rate per value of `column` (release) or per variant of a flag.
pub async fn sentiment_by(state: &AppState, project_id: &str, release: Option<&str>, flag: Option<&str>, since: i64) -> Value {
    let rows = sqlx::query("SELECT sentiment, release, flags FROM feedback WHERE project_id = $1 AND created_at >= $2 LIMIT 20000")
        .bind(project_id)
        .bind(since)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let mut m: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    for r in rows {
        let key = if let Some(f) = flag {
            let flags: BTreeMap<String, String> = serde_json::from_str(&r.s("flags")).unwrap_or_default();
            flags.get(f).cloned().unwrap_or_else(|| "(not set)".into())
        } else {
            let rel = r.s("release");
            if let Some(want) = release {
                if rel != want {
                    continue;
                }
            }
            rel
        };
        let e = m.entry(key).or_default();
        e.0 += 1;
        match r.os("sentiment").as_deref() {
            Some("negative" | "mixed") => {
                e.1 += 1;
                e.2 += 1
            }
            Some(_) => e.2 += 1,
            None => {}
        }
    }
    let out: BTreeMap<String, Value> = m
        .into_iter()
        .map(|(k, (n, neg, tagged))| (k, json!({"count": n, "negative": neg, "negative_rate": if tagged == 0 { Value::Null } else { json!((neg as f64 / tagged as f64 * 1000.0).round() / 1000.0) }})))
        .collect();
    json!(out)
}

// ---------------------------------------------------------------------------
// Retention and erasure
// ---------------------------------------------------------------------------

async fn delete_where(state: &AppState, cond: &str, binds: &[Bind]) -> anyhow::Result<u64> {
    let sel = format!("SELECT id FROM feedback WHERE {cond}");
    let mut q = sqlx::query(&sel);
    for b in binds {
        q = match b {
            Bind::S(s) => q.bind(s.clone()),
            Bind::I(i) => q.bind(*i),
        };
    }
    let ids: Vec<String> = q.fetch_all(&state.db).await?.iter().map(|r| r.s("id")).collect();
    for id in &ids {
        let files = sqlx::query("SELECT path FROM feedback_attachments WHERE feedback_id = $1").bind(id).fetch_all(&state.db).await?;
        for f in files {
            let path = f.s("path");
            let _ = tokio::fs::remove_file(&path).await;
            crate::s3::delete_artifact(state, &path).await;
            if let Some(dir) = std::path::Path::new(&path).parent() {
                let _ = tokio::fs::remove_dir(dir).await;
            }
        }
        sqlx::query("DELETE FROM feedback_attachments WHERE feedback_id = $1").bind(id).execute(&state.db).await?;
        sqlx::query("DELETE FROM feedback WHERE id = $1").bind(id).execute(&state.db).await?;
    }
    Ok(ids.len() as u64)
}

pub async fn enforce_retention(state: &AppState, project: &ProjectInfo) -> anyhow::Result<u64> {
    let cutoff = now_ms() - project.retention_days.max(1) * 86_400_000;
    delete_where(state, "project_id = $1 AND created_at < $2", &[Bind::S(project.id.clone()), Bind::I(cutoff)]).await
}

/// Delete a user's feedback: by user id, and by contact email when `user` is an address.
pub async fn erase_user(state: &AppState, project_id: &str, user: &str) -> anyhow::Result<u64> {
    delete_where(
        state,
        "project_id = $1 AND (user_id = $2 OR (contact_email != '' AND contact_email = $3))",
        &[Bind::S(project_id.into()), Bind::S(user.into()), Bind::S(user.trim().to_lowercase())],
    )
    .await
}

// ---------------------------------------------------------------------------
// Background worker: crash links, LLM tagging, alerts
// ---------------------------------------------------------------------------

const ALERT_WAIT_MS: i64 = 120_000;
const LINK_WINDOW_MS: i64 = 600_000;

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            if let Err(e) = link_pending(&state).await {
                tracing::warn!("feedback issue links failed: {e:#}");
            }
            if llm_enabled(&state) {
                if let Err(e) = tag_pending(&state).await {
                    tracing::warn!("feedback tagging failed: {e:#}");
                }
            }
            if let Err(e) = alert_pending(&state).await {
                tracing::warn!("feedback alerts failed: {e:#}");
            }
        }
    });
}

async fn link_pending(state: &AppState) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT id, project_id, event_id FROM feedback WHERE issue_id IS NULL AND event_id != '' AND created_at > $1 LIMIT 100")
        .bind(now_ms() - LINK_WINDOW_MS)
        .fetch_all(&state.db)
        .await?;
    for r in rows {
        if let Some(issue) = issue_for_event(state, &r.s("project_id"), &r.s("event_id")).await {
            sqlx::query("UPDATE feedback SET issue_id = $1 WHERE id = $2 AND issue_id IS NULL").bind(issue).bind(r.s("id")).execute(&state.db).await?;
        }
    }
    Ok(())
}

const TAG_SYSTEM: &str = "You classify one piece of end-user feedback about a software product for the team that builds it. \
Reply with only a JSON object, no prose and no code fences: {\"sentiment\": \"positive\" | \"neutral\" | \"negative\" | \"mixed\", \
\"topics\": [1 to 3 short lowercase labels of 1 to 3 words naming what the feedback is about, e.g. \"checkout\", \"login\", \"performance\", \"dark mode\"], \
\"summary\": \"one sentence of at most 20 words, in English\"}. Bug reports and complaints are negative; feature requests without complaint are neutral. \
Reuse a label from existing_topics whenever one fits. Never put personal data such as names, email addresses or phone numbers in topics or summary.";

pub fn tag_request(model: &str, message: &str, platform: &str, location: &str, existing: &[String]) -> Value {
    json!({
        "model": model,
        "max_tokens": 300,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": TAG_SYSTEM},
            {"role": "user", "content": serde_json::to_string(&json!({"platform": platform, "screen_or_path": location, "existing_topics": existing, "feedback": message})).unwrap_or_default()},
        ],
    })
}

/// Parse the model's reply into (sentiment, topics, summary).
pub fn parse_tags(text: &str) -> anyhow::Result<(String, Vec<String>, String)> {
    let start = text.find('{').ok_or_else(|| anyhow::anyhow!("no JSON object in reply"))?;
    let end = text.rfind('}').ok_or_else(|| anyhow::anyhow!("no JSON object in reply"))?;
    let v: Value = serde_json::from_str(&text[start..=end])?;
    let sentiment = v["sentiment"].as_str().unwrap_or("").to_lowercase();
    if !SENTIMENTS.contains(&sentiment.as_str()) {
        anyhow::bail!("unexpected sentiment `{sentiment}`");
    }
    let topics: Vec<String> = v["topics"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect();
    let mut topics = clean_topics(&topics);
    topics.truncate(3);
    Ok((sentiment, topics, truncate(v["summary"].as_str().unwrap_or("").trim(), 300)))
}

/// Only the path of a URL goes to the LLM: query strings can carry personal data.
fn location_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => u.path().to_string(),
        Err(_) => truncate(url.split('?').next().unwrap_or(""), 200),
    }
}

async fn existing_topics(state: &AppState, project_id: &str) -> Vec<String> {
    let rows = sqlx::query("SELECT topics FROM feedback WHERE project_id = $1 AND topics != '' AND topics != '[]' ORDER BY created_at DESC LIMIT 300")
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for r in rows {
        for t in topics_of(&r.s("topics")) {
            *counts.entry(t).or_default() += 1;
        }
    }
    let mut v: Vec<(String, i64)> = counts.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    v.into_iter().take(40).map(|(t, _)| t).collect()
}

async fn tag_pending(state: &AppState) -> anyhow::Result<()> {
    let (Some(base), Some(model)) = (state.config.llm_base_url.clone(), state.config.llm_model.clone()) else { return Ok(()) };
    let now = now_ms();
    let rows = sqlx::query("SELECT id, project_id, message, platform, url, tag_attempts FROM feedback WHERE tag_state = 'pending' AND (tag_next_at IS NULL OR tag_next_at <= $1) ORDER BY created_at LIMIT 10")
        .bind(now)
        .fetch_all(&state.db)
        .await?;
    for r in rows {
        let existing = existing_topics(state, &r.s("project_id")).await;
        let body = tag_request(&model, &r.s("message"), &r.s("platform"), &location_of(&r.s("url")), &existing);
        let result: anyhow::Result<(String, Vec<String>, String)> = async {
            let mut req = state.http.post(format!("{base}/chat/completions")).json(&body).timeout(Duration::from_secs(60));
            if let Some(key) = &state.config.llm_api_key {
                req = req.bearer_auth(key);
            }
            let resp = req.send().await?;
            let status = resp.status();
            let v: Value = resp.json().await?;
            if !status.is_success() {
                anyhow::bail!("LLM API returned {status}: {}", v["error"]["message"].as_str().unwrap_or(""));
            }
            parse_tags(&crate::anomalies::narrative_text(&v)?)
        }
        .await;
        match result {
            Ok((sentiment, topics, summary)) => {
                sqlx::query("UPDATE feedback SET sentiment = $1, topics = $2, summary = $3, tag_state = 'done' WHERE id = $4 AND tag_state = 'pending'")
                    .bind(sentiment)
                    .bind(serde_json::to_string(&topics)?)
                    .bind(summary)
                    .bind(r.s("id"))
                    .execute(&state.db)
                    .await?;
            }
            Err(e) => {
                let attempts = r.i("tag_attempts") + 1;
                tracing::warn!("tagging feedback {} failed (attempt {attempts}): {e:#}", r.s("id"));
                let (st, next) = if attempts >= 3 { ("failed", None) } else { ("pending", Some(now + 30_000 * (1 << attempts))) };
                sqlx::query("UPDATE feedback SET tag_attempts = $1, tag_state = $2, tag_next_at = $3 WHERE id = $4")
                    .bind(attempts)
                    .bind(st)
                    .bind(next)
                    .bind(r.s("id"))
                    .execute(&state.db)
                    .await?;
            }
        }
    }
    Ok(())
}

/// Alerts wait until tagging and the crash link are done (or 2 minutes passed), so sentiment and
/// linked-to-issue filters see final values.
async fn alert_pending(state: &AppState) -> anyhow::Result<()> {
    let now = now_ms();
    // Items older than a day (a long outage) are not alerted any more.
    sqlx::query("UPDATE feedback SET alerted = 1 WHERE alerted = 0 AND received_at < $1").bind(now - 86_400_000).execute(&state.db).await?;
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE f.alerted = 0 AND (f.tag_state != 'pending' OR f.received_at < $1) AND (f.event_id = '' OR f.issue_id IS NOT NULL OR f.received_at < $1) \
         ORDER BY f.created_at LIMIT 50"
    ))
    .bind(now - ALERT_WAIT_MS)
    .fetch_all(&state.db)
    .await?;
    for r in rows {
        let id = r.s("id");
        let claimed = sqlx::query("UPDATE feedback SET alerted = 1 WHERE id = $1 AND alerted = 0").bind(&id).execute(&state.db).await?.rows_affected();
        if claimed == 0 {
            continue;
        }
        let Some(project) = crate::projects::find(state, &r.s("project_id")).await? else { continue };
        let fb = row_json(state, &r, false);
        if let Err(e) = crate::alerts::notify_feedback(state, &project, &fb).await {
            tracing::warn!("feedback alert failed: {e:#}");
        }
    }
    Ok(())
}

/// Digest block: count vs the previous period, sentiment split, top topics and negative quotes.
pub async fn digest_block(state: &AppState, project: &ProjectInfo, from: i64, to: i64) -> Value {
    let prev_from = from - (to - from);
    let rows = sqlx::query("SELECT short_id, message, sentiment, topics, created_at FROM feedback WHERE project_id = $1 AND created_at >= $2 AND created_at < $3 ORDER BY created_at DESC LIMIT 5000")
        .bind(&project.id)
        .bind(prev_from)
        .bind(to)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let (cur, prev): (Vec<_>, Vec<_>) = rows.iter().partition(|r| r.i("created_at") >= from);
    if cur.is_empty() && prev.is_empty() {
        return Value::Null;
    }
    let mut sentiment: BTreeMap<String, i64> = BTreeMap::new();
    let mut topics: BTreeMap<String, i64> = BTreeMap::new();
    for r in &cur {
        *sentiment.entry(r.os("sentiment").unwrap_or_else(|| "untagged".into())).or_default() += 1;
        for t in topics_of(&r.s("topics")) {
            *topics.entry(t).or_default() += 1;
        }
    }
    let mut top: Vec<(String, i64)> = topics.into_iter().collect();
    top.sort_by_key(|x| std::cmp::Reverse(x.1));
    let quotes: Vec<Value> = cur
        .iter()
        .filter(|r| r.os("sentiment").as_deref() == Some("negative"))
        .take(2)
        .map(|r| json!({"short_id": short_id(&project.slug, r.i("short_id")), "message": truncate(&r.s("message"), 200)}))
        .collect();
    json!({
        "count": cur.len(),
        "previous": prev.len(),
        "sentiment": sentiment,
        "top_topics": top.into_iter().take(3).map(|(t, n)| json!({"topic": t, "count": n})).collect::<Vec<_>>(),
        "negative_quotes": quotes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    #[test]
    fn inspects_png() {
        assert_eq!(inspect_image(&png(640, 480)).unwrap(), ImageInfo { content_type: "image/png", ext: "png", width: 640, height: 480 });
        assert!(inspect_image(b"GIF89a....").is_err());
        assert!(inspect_image(b"").is_err());
    }

    fn jpeg_with_exif() -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        // APP0 JFIF
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x07, b'J', b'F', b'I', b'F', 0]);
        // APP1 EXIF with "secret" payload
        b.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x08, b'E', b'x', b'i', b'f', b'G', b'P']);
        // SOF0: len 11, precision 8, height 300, width 400
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x01, 0x2C, 0x01, 0x90, 0x01, 0x01, 0x11, 0x00]);
        // SOS + data + EOI
        b.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0x12, 0x34, 0xFF, 0xD9]);
        b
    }

    #[test]
    fn inspects_and_strips_jpeg() {
        let j = jpeg_with_exif();
        let info = inspect_image(&j).unwrap();
        assert_eq!((info.content_type, info.width, info.height), ("image/jpeg", 400, 300));
        let stripped = strip_jpeg_metadata(&j);
        assert!(!stripped.windows(4).any(|w| w == b"Exif"));
        assert!(stripped.windows(4).any(|w| w == b"JFIF"));
        assert!(stripped.ends_with(&[0x12, 0x34, 0xFF, 0xD9]));
        assert_eq!(inspect_image(&stripped).unwrap().width, 400);
    }

    #[test]
    fn inspects_webp() {
        let mut b = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        b.extend_from_slice(&[10, 0, 0, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&[0x7F, 0x02, 0x00]); // width - 1 = 639
        b.extend_from_slice(&[0xDF, 0x01, 0x00]); // height - 1 = 479
        let info = inspect_image(&b).unwrap();
        assert_eq!((info.width, info.height), (640, 480));
    }

    #[test]
    fn rate_windows() {
        let now = 10 * 3_600_000;
        assert!(take("t|a", 2, now));
        assert!(take("t|a", 2, now + 1));
        assert!(!take("t|a", 2, now + 2));
        assert!(take("t|a", 2, now + 3_600_000));
        assert!(take("t|b", 0, now));
    }

    #[test]
    fn parses_tags() {
        let (s, t, sum) = parse_tags("```json\n{\"sentiment\": \"Negative\", \"topics\": [\"Checkout\", \"checkout\", \"payments\", \"a\", \"b\"], \"summary\": \"Card payment fails.\"}\n```").unwrap();
        assert_eq!(s, "negative");
        assert_eq!(t, vec!["checkout", "payments", "a"]);
        assert_eq!(sum, "Card payment fails.");
        assert!(parse_tags("{\"sentiment\": \"angry\"}").is_err());
        assert!(parse_tags("no json").is_err());
    }

    #[test]
    fn emails_and_locations() {
        assert!(valid_email("a@b.se"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.se"));
        assert_eq!(location_of("https://x.io/checkout?email=a@b.se"), "/checkout");
        assert_eq!(location_of("CheckoutScreen"), "CheckoutScreen");
        assert_eq!(short_id("web", 12), "WEB-F12");
    }
}
