//! OpenTelemetry ingestion (OTLP/HTTP traces and logs, protobuf or JSON) and log queries.
//!
//! Spans become Vaka spans (SERVER and CONSUMER spans count as transactions), span `exception`
//! events and ERROR/FATAL log records become Vaka errors, and every log record goes to the
//! `logs` table.

use crate::auth::Principal;
use crate::query::{project_for, QResult};
use crate::state::{AppState, ProjectInfo};
use crate::util::{ch_time, iso_time, now_ms, parse_time_expr, truncate};
use base64::Engine;
use prost::Message;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Signal {
    Traces,
    Logs,
}

/// Decode an OTLP request body into OTLP/JSON. Returns the value and whether it was protobuf.
pub fn decode(signal: Signal, content_type: &str, body: &[u8]) -> Result<(Value, bool), String> {
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let is_json = ct == "application/json" || (ct.is_empty() && body.first() == Some(&b'{'));
    if is_json {
        return serde_json::from_slice(body).map(|v| (v, false)).map_err(|e| format!("request body is not valid OTLP/JSON: {e}"));
    }
    if !(ct.is_empty() || ct == "application/x-protobuf" || ct == "application/protobuf") {
        return Err(format!("unsupported content type `{ct}`: use application/x-protobuf or application/json"));
    }
    let v = match signal {
        Signal::Traces => {
            let r = opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest::decode(body).map_err(|e| format!("request body is not valid OTLP protobuf: {e}"))?;
            serde_json::to_value(&r).map_err(|e| e.to_string())?
        }
        Signal::Logs => {
            let r = opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest::decode(body).map_err(|e| format!("request body is not valid OTLP protobuf: {e}"))?;
            serde_json::to_value(&r).map_err(|e| e.to_string())?
        }
    };
    Ok((v, true))
}

/// The OTLP response: empty on success, `partial_success` when some items were rejected.
pub fn response(signal: Signal, proto: bool, rejected: i64, message: &str) -> (Vec<u8>, &'static str) {
    if proto {
        let bytes = match signal {
            Signal::Traces => {
                use opentelemetry_proto::tonic::collector::trace::v1::{ExportTracePartialSuccess, ExportTraceServiceResponse};
                let partial = (rejected > 0).then(|| ExportTracePartialSuccess { rejected_spans: rejected, error_message: message.to_string() });
                ExportTraceServiceResponse { partial_success: partial }.encode_to_vec()
            }
            Signal::Logs => {
                use opentelemetry_proto::tonic::collector::logs::v1::{ExportLogsPartialSuccess, ExportLogsServiceResponse};
                let partial = (rejected > 0).then(|| ExportLogsPartialSuccess { rejected_log_records: rejected, error_message: message.to_string() });
                ExportLogsServiceResponse { partial_success: partial }.encode_to_vec()
            }
        };
        (bytes, "application/x-protobuf")
    } else {
        let key = if signal == Signal::Traces { "rejectedSpans" } else { "rejectedLogRecords" };
        let v = if rejected > 0 { json!({"partialSuccess": {key: rejected.to_string(), "errorMessage": message}}) } else { json!({}) };
        (v.to_string().into_bytes(), "application/json")
    }
}

// ---------------------------------------------------------------------------
// OTLP/JSON helpers (lenient: camelCase or snake_case, numbers as strings or numbers)
// ---------------------------------------------------------------------------

fn field<'a>(v: &'a Value, names: &[&str]) -> &'a Value {
    for n in names {
        if let Some(x) = v.get(*n) {
            if !x.is_null() {
                return x;
            }
        }
    }
    &Value::Null
}

fn arr<'a>(v: &'a Value, names: &[&str]) -> impl Iterator<Item = &'a Value> {
    field(v, names).as_array().map(|a| a.iter()).into_iter().flatten()
}

fn int(v: &Value) -> Option<i128> {
    match v {
        Value::Number(n) => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)).or_else(|| n.as_f64().map(|f| f as i128)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Nanoseconds since the epoch to milliseconds.
fn nanos_ms(v: &Value) -> Option<i64> {
    int(v).filter(|n| *n > 0).map(|n| (n / 1_000_000) as i64)
}

/// An OTLP AnyValue as plain JSON.
fn any_value(v: &Value) -> Value {
    let Some(o) = v.as_object() else { return v.clone() };
    for (k, x) in o {
        let key = k.to_ascii_lowercase().replace('_', "");
        return match key.as_str() {
            "stringvalue" | "boolvalue" | "doublevalue" => x.clone(),
            "intvalue" => int(x).map(|n| json!(n as i64)).unwrap_or(Value::Null),
            "bytesvalue" => x.clone(),
            "arrayvalue" => Value::Array(arr(x, &["values"]).map(any_value).collect()),
            "kvlistvalue" => Value::Object(attrs(field(x, &["values"])).into_iter().collect()),
            _ => continue,
        };
    }
    Value::Null
}

/// OTLP attributes ([{key, value}]) as a map.
fn attrs(list: &Value) -> BTreeMap<String, Value> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|kv| Some((kv.get("key")?.as_str()?.to_string(), any_value(field(kv, &["value"])))))
        .collect()
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::String(_) => None,
        other => Some(other.to_string()),
    }
}

/// `vaka.error_captured: true` on a span or log record: a Vaka SDK already sent this error
/// through the ingest API, so OTLP must not create a second one.
fn captured_by_sdk(a: &BTreeMap<String, Value>) -> bool {
    a.get("vaka.error_captured").is_some_and(|v| v.as_bool().unwrap_or(false) || v.as_str() == Some("true"))
}

fn first(a: &BTreeMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| text(a.get(*k)))
}

/// Trace and span ids are hex in OTLP/JSON; some exporters send base64.
fn id(v: &Value) -> String {
    let s = v.as_str().unwrap_or("").trim();
    if s.is_empty() || s.chars().all(|c| c == '0') {
        return String::new();
    }
    if s.chars().all(|c| c.is_ascii_hexdigit()) {
        return s.to_ascii_lowercase();
    }
    match base64::engine::general_purpose::STANDARD.decode(s) {
        Ok(b) if b.iter().any(|x| *x != 0) => hex::encode(b),
        _ => String::new(),
    }
}

fn enum_code(v: &Value, names: &[&str]) -> i64 {
    if let Some(n) = int(v) {
        return n as i64;
    }
    let s = v.as_str().unwrap_or("").to_ascii_uppercase();
    names.iter().position(|n| s.ends_with(n)).map(|i| i as i64).unwrap_or(0)
}

struct Resource {
    service: String,
    release: Option<String>,
    environment: Option<String>,
    platform: Option<String>,
}

fn resource(r: &Value) -> Resource {
    let a = attrs(field(field(r, &["resource"]), &["attributes"]));
    let platform = first(&a, &["telemetry.sdk.language"]).map(|l| {
        match l.as_str() {
            "nodejs" => "node",
            "webjs" => "javascript",
            "java" => "jvm",
            "go" | "dotnet" | "python" | "rust" | "swift" => if l == "swift" { "ios" } else { l.as_str() },
            _ => "other",
        }
        .to_string()
    });
    Resource {
        service: first(&a, &["service.name"]).unwrap_or_default(),
        release: first(&a, &["service.version"]),
        environment: first(&a, &["deployment.environment.name", "deployment.environment"]),
        platform,
    }
}

const KINDS: &[&str] = &["UNSPECIFIED", "INTERNAL", "SERVER", "CLIENT", "PRODUCER", "CONSUMER"];

/// Convert an OTLP traces request into Vaka span and error items.
pub fn trace_items(req: &Value) -> Vec<Value> {
    let mut items = Vec::new();
    for rs in arr(req, &["resourceSpans", "resource_spans"]) {
        let res = resource(rs);
        for ss in arr(rs, &["scopeSpans", "scope_spans", "instrumentationLibrarySpans"]) {
            let scope = field(field(ss, &["scope", "instrumentationLibrary"]), &["name"]).as_str().unwrap_or("").to_string();
            for sp in arr(ss, &["spans"]) {
                let a = attrs(field(sp, &["attributes"]));
                let kind = match enum_code(field(sp, &["kind"]), KINDS) {
                    2 => "server",
                    3 => "client",
                    4 => "producer",
                    5 => "consumer",
                    _ => "internal",
                };
                let method = first(&a, &["http.request.method", "http.method"]);
                let status_code = first(&a, &["http.response.status_code", "http.status_code"]).and_then(|s| s.parse::<u16>().ok());
                let op = if method.is_some() && kind == "server" {
                    "http.server"
                } else if method.is_some() && kind == "client" {
                    "http.client"
                } else if a.contains_key("db.system") || a.contains_key("db.system.name") {
                    "db"
                } else if a.contains_key("messaging.system") {
                    if kind == "consumer" {
                        "queue.process"
                    } else {
                        "queue.publish"
                    }
                } else if a.contains_key("rpc.system") {
                    if kind == "server" {
                        "rpc.server"
                    } else {
                        "rpc.client"
                    }
                } else {
                    "function"
                };
                let raw_name = field(sp, &["name"]).as_str().unwrap_or("").to_string();
                let name = match (&method, first(&a, &["http.route"])) {
                    (Some(m), Some(route)) if kind == "server" && !raw_name.contains(&route) => format!("{} {route}", m.to_uppercase()),
                    _ if raw_name.is_empty() => op.to_string(),
                    _ => raw_name.clone(),
                };
                let status = match enum_code(field(field(sp, &["status"]), &["code"]), &["UNSET", "OK", "ERROR"]) {
                    1 => Some("ok"),
                    2 => Some("error"),
                    _ => None,
                };
                let trace_id = id(field(sp, &["traceId", "trace_id"]));
                let span_id = id(field(sp, &["spanId", "span_id"]));
                let start = nanos_ms(field(sp, &["startTimeUnixNano", "start_time_unix_nano"]));
                let end = nanos_ms(field(sp, &["endTimeUnixNano", "end_time_unix_nano"]));
                let user_id = first(&a, &["enduser.id", "user.id"]);
                let session_id = first(&a, &["session.id"]);
                let mut span_attrs: Map<String, Value> = a.iter().filter(|(k, _)| !k.starts_with("exception.")).map(|(k, v)| (k.clone(), v.clone())).collect();
                if !scope.is_empty() {
                    span_attrs.insert("otel.scope".into(), json!(scope));
                }
                items.push(json!({
                    "type": "span", "trace_id": trace_id, "span_id": span_id, "parent_span_id": id(field(sp, &["parentSpanId", "parent_span_id"])),
                    "name": name, "op": op, "kind": kind, "is_transaction": matches!(kind, "server" | "consumer"), "status": status,
                    "start": start, "end": end, "release": res.release, "environment": res.environment, "platform": res.platform,
                    "service": res.service, "user_id": user_id, "session_id": session_id, "attributes": span_attrs,
                    "http": {"method": method, "status_code": status_code},
                }));
                // A Vaka SDK that already reported the error itself (with breadcrumbs and
                // richer frames) marks the span so the exception events don't duplicate it.
                let sdk_captured = captured_by_sdk(&a);
                for ev in arr(sp, &["events"]) {
                    if sdk_captured || field(ev, &["name"]).as_str() != Some("exception") {
                        continue;
                    }
                    let ea = attrs(field(ev, &["attributes"]));
                    let ts = nanos_ms(field(ev, &["timeUnixNano", "time_unix_nano"])).or(end).or(start);
                    let escaped = ea.get("exception.escaped").and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true"))).unwrap_or(false);
                    let platform = res.platform.clone().unwrap_or_default();
                    items.push(error_item(&ea, ts, "error", Some(!escaped), &res, &platform, &trace_id, &span_id, user_id.clone(), session_id.clone(), &name, None));
                }
            }
        }
    }
    items
}

#[allow(clippy::too_many_arguments)]
fn error_item(
    a: &BTreeMap<String, Value>,
    ts: Option<i64>,
    level: &str,
    handled: Option<bool>,
    res: &Resource,
    platform: &str,
    trace_id: &str,
    span_id: &str,
    user_id: Option<String>,
    session_id: Option<String>,
    span_name: &str,
    message: Option<String>,
) -> Value {
    let ty = first(a, &["exception.type"]);
    let msg = first(a, &["exception.message"]).or_else(|| message.clone());
    let frames = first(a, &["exception.stacktrace"]).map(|s| parse_stack(&s, platform)).unwrap_or_default();
    let mut tags = Map::new();
    if !res.service.is_empty() {
        tags.insert("service".into(), json!(res.service));
    }
    if !span_name.is_empty() {
        tags.insert("otel.span".into(), json!(truncate(span_name, 200)));
    }
    if !span_id.is_empty() {
        tags.insert("span_id".into(), json!(span_id));
    }
    let mut item = json!({
        "type": "error", "timestamp": ts, "level": level, "handled": handled,
        "platform": res.platform, "release": res.release, "environment": res.environment,
        "trace_id": if trace_id.is_empty() { Value::Null } else { json!(trace_id) },
        "user_id": user_id, "session_id": session_id, "tags": tags,
    });
    if ty.is_some() || !frames.is_empty() {
        item["exception"] = json!({"type": ty.unwrap_or_else(|| "Error".into()), "value": msg.clone().unwrap_or_default(), "stacktrace": frames});
    } else {
        item["message"] = json!(msg.unwrap_or_else(|| "exception".into()));
    }
    item
}

fn vendor(path: &str, function: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("node_modules")
        || p.contains("site-packages")
        || p.contains("dist-packages")
        || p.starts_with("node:")
        || p.contains("/usr/lib/")
        || p.contains("/go/src/runtime/")
        || p.contains("/go/pkg/mod/")
        || p.contains("<frozen")
        || ["java.", "javax.", "jdk.", "sun.", "kotlin.", "kotlinx.", "scala.", "android.", "androidx.", "org.springframework.", "io.netty.", "System.", "Microsoft."].iter().any(|x| function.starts_with(x))
}

fn frame(function: &str, file: &str, line: Option<u32>, col: Option<u32>) -> Value {
    json!({"function": function, "filename": file, "lineno": line, "colno": col, "in_app": !vendor(file, function)})
}

/// Parse a stack trace string (Java/Kotlin, JavaScript, Python, .NET, Go, Ruby) into frames,
/// crash site first. Unknown formats give no frames.
pub fn parse_stack(s: &str, platform: &str) -> Vec<Value> {
    static JAVA: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"^\s*at ([\w$.<>/\-]+)\(([^:()]*)(?::(\d+))?\)").unwrap());
    static JS: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"^\s*at (?:(.+?) \()?(.+?):(\d+):(\d+)\)?$").unwrap());
    static PY: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r#"^\s*File "([^"]+)", line (\d+), in (.+)$"#).unwrap());
    static NET: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"^\s*at (.+?\)) in (.+):line (\d+)").unwrap());
    static GO: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"^\s+(/.+\.go):(\d+)").unwrap());
    static RB: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"^\s*(?:from )?(.+?\.rb):(\d+):in [`'](.+)'").unwrap());
    let mut out = Vec::new();
    let lines: Vec<&str> = s.lines().collect();
    if lines.iter().any(|l| PY.is_match(l)) || platform == "python" {
        for l in &lines {
            if let Some(c) = PY.captures(l) {
                out.push(frame(&c[3], &c[1], c[2].parse().ok(), None));
            }
        }
        out.reverse(); // Python prints the crash site last.
        return out.into_iter().take(100).collect();
    }
    for (i, l) in lines.iter().enumerate() {
        if let Some(c) = NET.captures(l) {
            out.push(frame(&c[1], &c[2], c[3].parse().ok(), None));
        } else if let Some(c) = JAVA.captures(l).filter(|_| !l.contains(".js:") && !l.contains(".ts:")) {
            out.push(frame(&c[1], &c[2], c.get(3).and_then(|m| m.as_str().parse().ok()), None));
        } else if let Some(c) = JS.captures(l) {
            out.push(frame(c.get(1).map(|m| m.as_str()).unwrap_or("<anonymous>"), &c[2], c[3].parse().ok(), c[4].parse().ok()));
        } else if let Some(c) = RB.captures(l) {
            out.push(frame(&c[3], &c[1], c[2].parse().ok(), None));
        } else if let Some(c) = GO.captures(l) {
            // Go: the function is on the line before the file.
            let func = i.checked_sub(1).map(|j| lines[j].trim()).unwrap_or("");
            let func = func.split('(').next().unwrap_or(func);
            out.push(frame(func, &c[1], c[2].parse().ok(), None));
        }
        if out.len() >= 100 {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub struct LogRow {
    pub project_id: String,
    pub timestamp: String,
    pub severity: String,
    pub severity_number: u8,
    pub body: String,
    pub trace_id: String,
    pub span_id: String,
    pub service: String,
    pub release: String,
    pub environment: String,
    pub user_id: String,
    pub attributes: BTreeMap<String, String>,
    pub retention_days: u16,
}

const SEVERITIES: &[(&str, u8)] = &[("trace", 1), ("debug", 5), ("info", 9), ("warn", 13), ("error", 17), ("fatal", 21)];

pub fn severity_label(n: u8, text: &str) -> &'static str {
    if n > 0 {
        return SEVERITIES.iter().rev().find(|(_, lo)| n >= *lo).map(|(l, _)| *l).unwrap_or("trace");
    }
    match text.to_ascii_lowercase().as_str() {
        "trace" => "trace",
        "debug" => "debug",
        "warn" | "warning" => "warn",
        "error" | "err" | "severe" => "error",
        "fatal" | "critical" | "crit" | "panic" | "emergency" | "alert" => "fatal",
        _ => "info",
    }
}

pub fn severity_min(label: &str) -> Option<u8> {
    let l = if label == "warning" { "warn" } else { label };
    SEVERITIES.iter().find(|(s, _)| *s == l).map(|(_, n)| *n)
}

/// Convert an OTLP logs request into log rows and Vaka error items (ERROR and FATAL records).
pub fn log_rows(project: &ProjectInfo, req: &Value, now: i64) -> (Vec<LogRow>, Vec<Value>) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for rl in arr(req, &["resourceLogs", "resource_logs"]) {
        let res = resource(rl);
        for sl in arr(rl, &["scopeLogs", "scope_logs", "instrumentationLibraryLogs"]) {
            let scope = field(field(sl, &["scope", "instrumentationLibrary"]), &["name"]).as_str().unwrap_or("").to_string();
            for lr in arr(sl, &["logRecords", "log_records"]) {
                let a = attrs(field(lr, &["attributes"]));
                let ts = nanos_ms(field(lr, &["timeUnixNano", "time_unix_nano"])).or_else(|| nanos_ms(field(lr, &["observedTimeUnixNano", "observed_time_unix_nano"]))).unwrap_or(now);
                let sev_text = field(lr, &["severityText", "severity_text"]).as_str().unwrap_or("").to_string();
                let num = int(field(lr, &["severityNumber", "severity_number"])).unwrap_or_else(|| {
                    // Enum names like SEVERITY_NUMBER_ERROR.
                    let s = field(lr, &["severityNumber", "severity_number"]).as_str().unwrap_or("").to_ascii_lowercase();
                    s.rsplit('_').next().and_then(severity_min).map(i128::from).unwrap_or(0)
                });
                let num = num.clamp(0, 24) as u8;
                let label = severity_label(num, &sev_text);
                let body = match any_value(field(lr, &["body"])) {
                    Value::String(s) => s,
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                let trace_id = id(field(lr, &["traceId", "trace_id"]));
                let span_id = id(field(lr, &["spanId", "span_id"]));
                let user_id = first(&a, &["enduser.id", "user.id"]);
                let mut attributes: BTreeMap<String, String> = a.iter().take(64).map(|(k, v)| (truncate(k, 128), truncate(&crate::models::value_to_string(v), 2048))).collect();
                if !scope.is_empty() {
                    attributes.insert("otel.scope".into(), truncate(&scope, 256));
                }
                if matches!(label, "error" | "fatal") && !captured_by_sdk(&a) {
                    let platform = res.platform.clone().unwrap_or_default();
                    let mut e = error_item(&a, Some(ts), label, None, &res, &platform, &trace_id, &span_id, user_id.clone(), first(&a, &["session.id"]), "", Some(truncate(&body, 1000)));
                    if !scope.is_empty() {
                        e["tags"]["logger"] = json!(truncate(&scope, 200));
                    }
                    errors.push(e);
                }
                rows.push(LogRow {
                    project_id: project.id.clone(),
                    timestamp: ch_time(ts),
                    severity: label.to_string(),
                    severity_number: if num > 0 { num } else { severity_min(label).unwrap_or(9) },
                    body: truncate(&body, 16_384),
                    trace_id,
                    span_id,
                    service: truncate(&res.service, 128),
                    release: res.release.clone().unwrap_or_default(),
                    environment: res.environment.clone().unwrap_or_else(|| "production".into()),
                    user_id: user_id.unwrap_or_default(),
                    attributes,
                    retention_days: project.retention_days as u16,
                });
            }
        }
    }
    (rows, errors)
}

const LOG_COLS: &str = "toUnixTimestamp64Milli(timestamp) AS ts_ms, severity, body, trace_id, span_id, service, release, environment, attributes";

fn log_json(r: &Value) -> Value {
    json!({
        "timestamp": iso_time(r["ts_ms"].as_i64().unwrap_or(0)), "severity": r["severity"], "body": r["body"],
        "trace_id": r["trace_id"], "span_id": r["span_id"], "service": r["service"], "release": r["release"],
        "environment": r["environment"], "attributes": r["attributes"],
    })
}

/// Logs that belong to a trace, oldest first. `t0` is the trace start (ms).
pub async fn logs_for_trace(state: &AppState, project_id: &str, trace_id: &str, t0: i64) -> anyhow::Result<Vec<Value>> {
    let rows = state
        .ch
        .query(
            &format!(
                "SELECT {LOG_COLS} FROM logs WHERE project_id = {{p:String}} AND trace_id = {{t:String}} \
                 AND timestamp >= fromUnixTimestamp64Milli({{lo:Int64}}) AND timestamp <= fromUnixTimestamp64Milli({{hi:Int64}}) ORDER BY timestamp LIMIT 200"
            ),
            &[("p", project_id.to_string()), ("t", trace_id.to_string()), ("lo", (t0 - 86_400_000).to_string()), ("hi", (t0 + 86_400_000).to_string())],
        )
        .await?;
    Ok(rows.iter().map(log_json).collect())
}

#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct LogParams {
    #[serde(default)]
    pub project: String,
    pub since: Option<String>,
    pub until: Option<String>,
    /// Text to look for in the body (case-insensitive).
    pub q: Option<String>,
    /// Minimum severity: trace, debug, info, warn, error, fatal.
    pub severity: Option<String>,
    pub trace_id: Option<String>,
    pub service: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub limit: Option<i64>,
}

/// Search logs, newest first, with counts per severity.
pub async fn search_logs(state: &AppState, p: &Principal, a: LogParams) -> QResult<Value> {
    let proj = project_for(state, p, &a.project).await?;
    let now = now_ms();
    let since = a.since.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 86_400_000);
    let until = a.until.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now);
    let mut cond = "project_id = {p:String} AND timestamp >= fromUnixTimestamp64Milli({since:Int64}) AND timestamp <= fromUnixTimestamp64Milli({until:Int64})".to_string();
    let mut ps: Vec<(&str, String)> = vec![("p", proj.id.clone()), ("since", since.to_string()), ("until", until.to_string())];
    if let Some(q) = a.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        cond.push_str(" AND positionCaseInsensitiveUTF8(body, {q:String}) > 0");
        ps.push(("q", q.to_string()));
    }
    if let Some(sev) = a.severity.as_deref().filter(|s| !s.is_empty()) {
        let n = severity_min(&sev.to_ascii_lowercase()).ok_or_else(|| crate::auth::ApiError::bad("severity must be trace, debug, info, warn, error or fatal"))?;
        cond.push_str(" AND severity_number >= {sev:UInt8}");
        ps.push(("sev", n.to_string()));
    }
    for (key, val) in [("trace_id", &a.trace_id), ("service", &a.service), ("release", &a.release), ("environment", &a.environment)] {
        if let Some(v) = val.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            cond.push_str(&format!(" AND {key} = {{{key}:String}}"));
            ps.push((key, if key == "trace_id" { v.to_ascii_lowercase() } else { v.to_string() }));
        }
    }
    ps.push(("lim", a.limit.unwrap_or(100).clamp(1, 1000).to_string()));
    let rows = state.ch.query(&format!("SELECT {LOG_COLS} FROM logs WHERE {cond} ORDER BY timestamp DESC LIMIT {{lim:UInt32}}"), &ps).await?;
    let counts = state.ch.query(&format!("SELECT severity AS sev, count() AS n FROM logs WHERE {cond} GROUP BY sev"), &ps).await?;
    let by_sev: Map<String, Value> = counts.iter().map(|r| (r["sev"].as_str().unwrap_or("").to_string(), json!(r["n"].as_i64().unwrap_or(0)))).collect();
    Ok(json!({
        "project": proj.slug, "since": iso_time(since), "until": iso_time(until),
        "counts": by_sev, "logs": rows.iter().map(log_json).collect::<Vec<_>>(),
        "hint": "Use get_trace with a log's trace_id to see the request it belongs to.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj() -> ProjectInfo {
        ProjectInfo { id: "p1".into(), slug: "web".into(), name: "Web".into(), platform: "node".into(), retention_days: 30, scrub_enabled: false, scrub_fields: String::new() }
    }

    fn sample_traces() -> Value {
        json!({"resourceSpans": [{
            "resource": {"attributes": [
                {"key": "service.name", "value": {"stringValue": "checkout"}},
                {"key": "service.version", "value": {"stringValue": "2.1.0"}},
                {"key": "deployment.environment.name", "value": {"stringValue": "staging"}},
                {"key": "telemetry.sdk.language", "value": {"stringValue": "java"}}]},
            "scopeSpans": [{"scope": {"name": "io.opentelemetry.tomcat"}, "spans": [
                {"traceId": "5b8efff798038103d269b633813fc60c", "spanId": "eee19b7ec3c1b174", "parentSpanId": "eee19b7ec3c1b173",
                 "name": "GET", "kind": 2, "startTimeUnixNano": "1700000000000000000", "endTimeUnixNano": "1700000000250000000",
                 "status": {"code": "STATUS_CODE_ERROR"},
                 "attributes": [{"key": "http.request.method", "value": {"stringValue": "GET"}},
                                {"key": "http.route", "value": {"stringValue": "/cart/{id}"}},
                                {"key": "http.response.status_code", "value": {"intValue": "500"}},
                                {"key": "enduser.id", "value": {"stringValue": "u-7"}}],
                 "events": [{"name": "exception", "timeUnixNano": "1700000000200000000", "attributes": [
                    {"key": "exception.type", "value": {"stringValue": "java.lang.IllegalStateException"}},
                    {"key": "exception.message", "value": {"stringValue": "cart is empty"}},
                    {"key": "exception.escaped", "value": {"boolValue": true}},
                    {"key": "exception.stacktrace", "value": {"stringValue": "java.lang.IllegalStateException: cart is empty\n\tat com.shop.Cart.total(Cart.java:42)\n\tat java.base/java.util.ArrayList.forEach(ArrayList.java:1596)"}}]}]},
                {"traceId": "5b8efff798038103d269b633813fc60c", "spanId": "aaa19b7ec3c1b174", "parentSpanId": "eee19b7ec3c1b174",
                 "name": "SELECT carts", "kind": "SPAN_KIND_CLIENT", "startTimeUnixNano": 1700000000010000000u64, "endTimeUnixNano": 1700000000090000000u64,
                 "attributes": [{"key": "db.system", "value": {"stringValue": "postgresql"}}]}
            ]}]
        }]})
    }

    #[test]
    fn converts_spans_and_exceptions() {
        let items = trace_items(&sample_traces());
        assert_eq!(items.len(), 3);
        let s = &items[0];
        assert_eq!((s["type"].as_str(), s["name"].as_str(), s["op"].as_str(), s["kind"].as_str()), (Some("span"), Some("GET /cart/{id}"), Some("http.server"), Some("server")));
        assert_eq!(s["is_transaction"], true);
        assert_eq!(s["status"], "error");
        assert_eq!((s["start"].as_i64(), s["end"].as_i64()), (Some(1_700_000_000_000), Some(1_700_000_000_250)));
        assert_eq!((s["release"].as_str(), s["environment"].as_str(), s["service"].as_str(), s["user_id"].as_str()), (Some("2.1.0"), Some("staging"), Some("checkout"), Some("u-7")));
        assert_eq!(s["http"]["status_code"], 500);
        let e = items.iter().find(|i| i["type"] == "error").unwrap();
        assert_eq!(e["exception"]["type"], "java.lang.IllegalStateException");
        assert_eq!(e["handled"], false);
        assert_eq!(e["trace_id"], "5b8efff798038103d269b633813fc60c");
        assert_eq!(e["exception"]["stacktrace"][0]["function"], "com.shop.Cart.total");
        assert_eq!(e["exception"]["stacktrace"][0]["lineno"], 42);
        assert_eq!(e["exception"]["stacktrace"][1]["in_app"], false);
        let db = &items[2];
        assert_eq!((db["op"].as_str(), db["kind"].as_str(), db["is_transaction"].as_bool()), (Some("db"), Some("client"), Some(false)));
        // The converted items are valid Vaka spans.
        let sp: crate::models::SpanItem = serde_json::from_value(items[0].clone()).unwrap();
        let row = crate::perf::span_row(&proj(), &sp, 0).unwrap();
        assert_eq!(row.is_root, 1);
        assert_eq!(row.http_status, 500);
        let _: crate::models::ErrorItem = serde_json::from_value(e.clone()).unwrap();
    }

    #[test]
    fn protobuf_round_trip() {
        use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, KeyValue};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
        let kv = |k: &str, v: any_value::Value| KeyValue { key: k.into(), value: Some(AnyValue { value: Some(v) }), ..Default::default() };
        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource { attributes: vec![kv("service.name", any_value::Value::StringValue("api".into()))], ..Default::default() }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        trace_id: vec![1; 16],
                        span_id: vec![2; 8],
                        name: "work".into(),
                        kind: 1,
                        start_time_unix_nano: 1_700_000_000_000_000_000,
                        end_time_unix_nano: 1_700_000_001_000_000_000,
                        attributes: vec![kv("retries", any_value::Value::IntValue(3))],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let (v, proto) = decode(Signal::Traces, "application/x-protobuf", &req.encode_to_vec()).unwrap();
        assert!(proto);
        let items = trace_items(&v);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["trace_id"], "01010101010101010101010101010101");
        assert_eq!(items[0]["span_id"], "0202020202020202");
        assert_eq!(items[0]["parent_span_id"], "");
        assert_eq!(items[0]["service"], "api");
        assert_eq!(items[0]["attributes"]["retries"], 3);
        assert_eq!(items[0]["end"].as_i64(), Some(1_700_000_001_000));
        let (bytes, ct) = response(Signal::Traces, true, 0, "");
        assert_eq!(ct, "application/x-protobuf");
        assert!(bytes.is_empty());
        assert!(decode(Signal::Traces, "text/plain", b"x").is_err());
    }

    #[test]
    fn converts_logs() {
        let req = json!({"resourceLogs": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "worker"}}]},
            "scopeLogs": [{"scope": {"name": "app.jobs"}, "logRecords": [
                {"timeUnixNano": "1700000000000000000", "severityNumber": 9, "severityText": "INFO", "body": {"stringValue": "job started"},
                 "traceId": "5b8efff798038103d269b633813fc60c", "spanId": "eee19b7ec3c1b174"},
                {"timeUnixNano": "1700000001000000000", "severityNumber": "SEVERITY_NUMBER_ERROR", "body": {"stringValue": "job failed: timeout"},
                 "attributes": [{"key": "job.id", "value": {"intValue": 12}}]},
                {"observedTimeUnixNano": "1700000002000000000", "severityText": "warning", "body": {"kvlistValue": {"values": [{"key": "a", "value": {"boolValue": true}}]}}}
            ]}]}]});
        let (rows, errors) = log_rows(&proj(), &req, 5);
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].severity.as_str(), rows[0].trace_id.as_str(), rows[0].service.as_str()), ("info", "5b8efff798038103d269b633813fc60c", "worker"));
        assert_eq!((rows[1].severity.as_str(), rows[1].severity_number), ("error", 17));
        assert_eq!(rows[1].attributes["job.id"], "12");
        assert_eq!((rows[2].severity.as_str(), rows[2].body.as_str()), ("warn", r#"{"a":true}"#));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["message"], "job failed: timeout");
        assert_eq!(errors[0]["tags"]["logger"], "app.jobs");
        let _: crate::models::ErrorItem = serde_json::from_value(errors[0].clone()).unwrap();
    }

    #[test]
    fn skips_errors_already_captured_by_sdk() {
        let flag = json!({"key": "vaka.error_captured", "value": {"boolValue": true}});
        let logs = json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [
            {"severityNumber": 17, "body": {"stringValue": "boom"}, "attributes": [flag]}]}]}]});
        let (rows, errors) = log_rows(&proj(), &logs, 5);
        assert_eq!((rows.len(), errors.len()), (1, 0));
        let traces = json!({"resourceSpans": [{"scopeSpans": [{"spans": [{"traceId": "01010101010101010101010101010101",
            "spanId": "0202020202020202", "name": "GET /", "kind": 2, "attributes": [flag],
            "events": [{"name": "exception", "attributes": [{"key": "exception.type", "value": {"stringValue": "ValueError"}}]}]}]}]}]});
        let items = trace_items(&traces);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["type"], "span");
    }

    #[test]
    fn stack_formats() {
        let js = "TypeError: x is undefined\n    at render (/app/src/view.js:10:5)\n    at /app/node_modules/react/index.js:3:1";
        let f = parse_stack(js, "node");
        assert_eq!((f[0]["function"].as_str(), f[0]["lineno"].as_i64(), f[0]["in_app"].as_bool()), (Some("render"), Some(10), Some(true)));
        assert_eq!(f[1]["in_app"], false);
        let py = "Traceback (most recent call last):\n  File \"/app/main.py\", line 3, in <module>\n    run()\n  File \"/app/jobs.py\", line 9, in run\n    1/0\nZeroDivisionError: division by zero";
        let f = parse_stack(py, "python");
        assert_eq!((f[0]["function"].as_str(), f[0]["filename"].as_str()), (Some("run"), Some("/app/jobs.py")));
        let net = "System.Exception: boom\n   at Shop.Cart.Total() in /src/Cart.cs:line 12";
        assert_eq!(parse_stack(net, "dotnet")[0]["lineno"], 12);
        let go = "panic: boom\n\ngoroutine 1 [running]:\nmain.handler(0x1)\n\t/app/main.go:21 +0x1d";
        let f = parse_stack(go, "go");
        assert_eq!((f[0]["function"].as_str(), f[0]["lineno"].as_i64()), (Some("main.handler"), Some(21)));
        assert!(parse_stack("just text", "").is_empty());
        assert_eq!(severity_label(0, "CRITICAL"), "fatal");
        assert_eq!(severity_label(14, ""), "warn");
    }
}
