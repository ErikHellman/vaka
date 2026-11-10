//! Wire format accepted by the ingest API (schema version 1).
//!
//! Everything is lenient: unknown fields are ignored and almost every field is
//! optional, so SDKs written by coding agents degrade gracefully instead of
//! being rejected.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SdkInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct Batch {
    #[serde(default)]
    pub sdk: Option<SdkInfo>,
    pub items: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    Error(Box<ErrorItem>),
    Session(SessionItem),
    Event(EventItem),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Device {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub os_name: Option<String>,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub app_version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UserRef {
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Frame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Class (JVM), module or package path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Full URL or absolute path of the file (used to find JS source maps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abs_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colno: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_app: Option<bool>,
    /// Native frames: hex address, e.g. "0x1000a3f4".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_line: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_context: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_context: Vec<String>,
    /// Set by the server when the frame was rewritten by symbolication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbolicated: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExceptionData {
    #[serde(default, rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Stack frames, crash site FIRST (frames[0] is where the error was raised).
    #[serde(default, alias = "frames")]
    pub stacktrace: Vec<Frame>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DebugImage {
    #[serde(default, rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub debug_id: Option<String>,
    /// ELF build id (hex) / PE code id; converted to a debug id when debug_id is missing.
    #[serde(default)]
    pub code_id: Option<String>,
    #[serde(default)]
    pub code_file: Option<String>,
    #[serde(default)]
    pub image_addr: Option<String>,
    #[serde(default)]
    pub image_size: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ErrorItem {
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub timestamp: Option<Value>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub handled: Option<bool>,
    #[serde(default)]
    pub fatal: Option<bool>,
    /// Exception chain, outermost first. `exception` (single) is also accepted.
    #[serde(default)]
    pub exceptions: Vec<ExceptionData>,
    #[serde(default)]
    pub exception: Option<ExceptionData>,
    #[serde(default)]
    pub breadcrumbs: Vec<Value>,
    #[serde(default)]
    pub tags: BTreeMap<String, Value>,
    #[serde(default)]
    pub contexts: Option<Value>,
    #[serde(default)]
    pub user: Option<UserRef>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub anon_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub device: Option<Device>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub debug_images: Vec<DebugImage>,
    #[serde(default)]
    pub fingerprint: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionItem {
    pub session_id: String,
    /// ok | exited | crashed | errored | abnormal
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub started: Option<Value>,
    #[serde(default)]
    pub timestamp: Option<Value>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub errors: Option<u32>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub anon_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventItem {
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub timestamp: Option<Value>,
    /// "pageview", "screen_view", or any custom event name.
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
    /// Screen name for screen_view events.
    #[serde(default)]
    pub screen: Option<String>,
    #[serde(default)]
    pub props: BTreeMap<String, Value>,
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
    pub device: Option<Device>,
}

pub fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
