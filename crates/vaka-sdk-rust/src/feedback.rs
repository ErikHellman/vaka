//! User feedback: forward what users wrote (a support form, in-app chat) to Vaka.

use crate::{client, new_id, now_ms, SDK_NAME, SDK_VERSION};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;

/// Most images one feedback item can carry.
pub const MAX_ATTACHMENTS: usize = 3;
const MAX_MESSAGE_CHARS: usize = 5000;

/// Feedback a user wrote. Only `message` is required.
///
/// ```no_run
/// let r = vaka_sdk::capture_feedback(vaka_sdk::Feedback {
///     message: "The export button does nothing".into(),
///     email: Some("ann@example.com".into()),
///     issue: Some("API-12".into()),
///     ..Default::default()
/// });
/// ```
#[derive(Clone, Debug, Default)]
pub struct Feedback {
    /// What the user wrote. Longer than 5000 characters is cut.
    pub message: String,
    /// Contact email, only when the user gave it.
    pub email: Option<String>,
    pub name: Option<String>,
    /// The error event this feedback is about (see [`crate::last_event_id`]).
    pub event_id: Option<String>,
    /// The issue this feedback is about: id or short id like `API-12`.
    pub issue: Option<String>,
    /// Defaults to the user set with [`crate::identify`].
    pub user_id: Option<String>,
    /// The end user's session, when your app has one.
    pub session_id: Option<String>,
    /// Page or screen the feedback is about.
    pub url: Option<String>,
    /// The end user's IP address. Feedback is rate limited per address, so without it all
    /// feedback from this server shares one limit.
    pub ip: Option<String>,
    pub tags: BTreeMap<String, String>,
    /// Up to 3 PNG, JPEG or WebP images, 5 MB each.
    pub attachments: Vec<Attachment>,
}

/// An image sent with feedback.
#[derive(Clone, Debug, Default)]
pub struct Attachment {
    pub filename: String,
    /// Defaults to one guessed from the file name.
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

/// The stored feedback's ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackResult {
    pub id: String,
    /// Looks like `API-F12`.
    pub short_id: String,
}

/// Why feedback was not stored.
#[derive(Debug)]
pub enum FeedbackError {
    /// [`crate::init`] was not called, or the SDK is disabled.
    NotInitialized,
    /// The feedback is invalid (empty message, too many attachments).
    Invalid(String),
    /// The server refused it. 429 means a rate limit was hit.
    Status(u16, String),
    /// The server could not be reached.
    Transport(String),
}

impl fmt::Display for FeedbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeedbackError::NotInitialized => write!(f, "vaka: not initialized"),
            FeedbackError::Invalid(m) => write!(f, "vaka: {m}"),
            FeedbackError::Status(code, m) if m.is_empty() => write!(f, "vaka: feedback: HTTP {code}"),
            FeedbackError::Status(code, m) => write!(f, "vaka: feedback: HTTP {code}: {m}"),
            FeedbackError::Transport(m) => write!(f, "vaka: feedback: {m}"),
        }
    }
}

impl std::error::Error for FeedbackError {}

fn content_type_of(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

fn multipart(item: &Value, files: &[Attachment]) -> (Vec<u8>, String) {
    let boundary = format!("vaka-{}", new_id().replace('-', ""));
    let mut out = Vec::new();
    out.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"feedback\"\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    out.extend_from_slice(item.to_string().as_bytes());
    out.extend_from_slice(b"\r\n");
    for a in files {
        let name: String = a.filename.chars().filter(|c| !matches!(c, '"' | '\r' | '\n')).collect();
        let ct = a.content_type.as_deref().unwrap_or_else(|| content_type_of(&a.filename));
        out.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"attachment\"; filename=\"{name}\"\r\nContent-Type: {ct}\r\n\r\n").as_bytes());
        out.extend_from_slice(&a.data);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (out, format!("multipart/form-data; boundary={boundary}"))
}

/// Send user feedback right away (`POST /ingest/v1/feedback`), with the release,
/// environment, user and tags set on the SDK. Blocks until the server answers.
pub fn capture_feedback(feedback: Feedback) -> Result<FeedbackResult, FeedbackError> {
    let c = client().ok_or(FeedbackError::NotInitialized)?;
    let message = feedback.message.trim();
    if message.is_empty() {
        return Err(FeedbackError::Invalid("feedback needs a message".into()));
    }
    if feedback.attachments.len() > MAX_ATTACHMENTS {
        return Err(FeedbackError::Invalid(format!("at most {MAX_ATTACHMENTS} attachments per feedback")));
    }
    let message: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
    let (scope_user, mut tags) = match c.scope.lock() {
        Ok(s) => (s.user_id.clone(), s.tags.clone()),
        Err(_) => (None, BTreeMap::new()),
    };
    tags.extend(feedback.tags.clone());
    let id = new_id();
    let mut item = json!({
        "id": id,
        "timestamp": now_ms(),
        "platform": "rust",
        "source": "api",
        "message": message,
        "environment": c.config.environment,
        "tags": tags,
    });
    let fields = [
        ("release", c.config.release.clone()),
        ("email", feedback.email.map(|e| e.trim().to_string())),
        ("name", feedback.name.map(|n| n.trim().to_string())),
        ("event_id", feedback.event_id),
        ("issue", feedback.issue),
        ("user_id", feedback.user_id.or(scope_user)),
        ("session_id", feedback.session_id),
        ("url", feedback.url),
    ];
    for (k, v) in fields {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            item[k] = json!(v);
        }
    }
    let url = format!("{}/ingest/v1/feedback", c.config.endpoint.trim_end_matches('/'));
    let mut req = c.agent.post(&url).set("X-Vaka-Key", &c.config.key).set("User-Agent", &format!("{SDK_NAME}/{SDK_VERSION}"));
    if let Some(ip) = feedback.ip.as_deref().filter(|i| !i.is_empty()) {
        req = req.set("X-Forwarded-For", ip);
    }
    let result = if feedback.attachments.is_empty() {
        req.set("Content-Type", "application/json").send_string(&item.to_string())
    } else {
        let (body, ct) = multipart(&item, &feedback.attachments);
        req.set("Content-Type", &ct).send_bytes(&body)
    };
    let body = match result {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(code, r)) => {
            let mut text = String::new();
            let _ = r.into_reader().take(64 * 1024).read_to_string(&mut text);
            let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"].as_str().map(String::from)).unwrap_or_default();
            return Err(FeedbackError::Status(code, msg));
        }
        Err(e) => return Err(FeedbackError::Transport(e.to_string())),
    };
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    crate::add_breadcrumb("feedback", "user feedback sent", crate::Level::Info);
    Ok(FeedbackResult {
        id: v["id"].as_str().map(String::from).unwrap_or(id),
        short_id: v["short_id"].as_str().unwrap_or_default().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(content_type_of("a.PNG"), "image/png");
        assert_eq!(content_type_of("a.webp"), "image/webp");
        assert_eq!(content_type_of("a.jpg"), "image/jpeg");
    }

    #[test]
    fn multipart_body() {
        let (body, ct) = multipart(&json!({"message": "hi"}), &[Attachment { filename: "s\"hot.png".into(), content_type: None, data: vec![1, 2] }]);
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"feedback\"")));
        assert!(text.contains("{\"message\":\"hi\"}"));
        assert!(text.contains("filename=\"shot.png\"\r\nContent-Type: image/png\r\n\r\n\u{1}\u{2}\r\n"));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
    }
}
