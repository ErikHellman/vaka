//! Rust SDK for [Vaka](https://github.com/ErikHellman/vaka): panics, errors,
//! release health (sessions) and custom events.
//!
//! ```no_run
//! let _vaka = vaka_sdk::init(vaka_sdk::Config {
//!     key: "vk_...".into(),
//!     endpoint: "https://vaka.example.com".into(),
//!     release: Some(env!("CARGO_PKG_VERSION").into()),
//!     ..Default::default()
//! });
//!
//! if let Err(e) = std::fs::read("missing.txt") {
//!     vaka_sdk::capture_error(&e);
//! }
//! vaka_sdk::track("job_finished", &[("kind", "import")]);
//! // Dropping the guard ends the session and flushes queued data.
//! ```
//!
//! Panics are reported automatically (synchronously, before the process
//! unwinds or aborts) and mark the session as crashed.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod stack;
#[cfg(feature = "tracing")]
pub mod tracing_layer;

pub use stack::{capture_backtrace, debug_images};

const SDK_NAME: &str = "vaka-rust";
const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_BREADCRUMBS: usize = 100;

/// SDK configuration.
#[derive(Clone, Debug)]
pub struct Config {
    /// Project ingest key (`vk_...`).
    pub key: String,
    /// Vaka server URL.
    pub endpoint: String,
    /// Release (app version). Must match the release used for debug file uploads, if any.
    pub release: Option<String>,
    /// Environment, default "production".
    pub environment: String,
    /// Report panics (default true).
    pub capture_panics: bool,
    /// Track a session for release health (default true).
    pub sessions: bool,
    /// Crate name prefixes that count as your code (in-app frames). Defaults to everything
    /// except std, core, alloc and common runtime crates.
    pub in_app_include: Vec<String>,
    /// How often queued items are sent (default 5 s).
    pub flush_interval: Duration,
    /// Print SDK diagnostics to stderr.
    pub debug: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            key: std::env::var("VAKA_KEY").unwrap_or_default(),
            endpoint: std::env::var("VAKA_ENDPOINT").unwrap_or_default(),
            release: std::env::var("VAKA_RELEASE").ok(),
            environment: std::env::var("VAKA_ENVIRONMENT").unwrap_or_else(|_| "production".into()),
            capture_panics: true,
            sessions: true,
            in_app_include: Vec::new(),
            flush_interval: Duration::from_secs(5),
            debug: false,
        }
    }
}

/// Severity of a captured event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Fatal,
    Error,
    Warning,
    Info,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Fatal => "fatal",
            Level::Error => "error",
            Level::Warning => "warning",
            Level::Info => "info",
        }
    }
}

enum Msg {
    Item(Value),
    Flush(Sender<()>),
    Shutdown,
}

struct Client {
    config: Config,
    tx: Mutex<Sender<Msg>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    scope: Mutex<Scope>,
    session: Mutex<Option<Session>>,
    agent: ureq::Agent,
}

#[derive(Default)]
struct Scope {
    user_id: Option<String>,
    tags: BTreeMap<String, String>,
    breadcrumbs: Vec<Value>,
}

struct Session {
    id: String,
    started_ms: i64,
    started: Instant,
    errors: u32,
    distinct_id: String,
}

static CLIENT: OnceLock<Client> = OnceLock::new();

/// Returned by [`init`]. Dropping it ends the session and flushes queued data.
#[must_use = "dropping the guard immediately ends the Vaka session"]
pub struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        shutdown(Duration::from_secs(5));
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Initialize the SDK. Keep the returned guard alive for the lifetime of the program.
pub fn init(config: Config) -> Guard {
    if config.key.is_empty() || config.endpoint.is_empty() {
        eprintln!("[vaka] init: `key` and `endpoint` are required; SDK disabled");
        return Guard(());
    }
    let (tx, rx) = mpsc::channel();
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build();
    let worker = {
        let config = config.clone();
        let agent = agent.clone();
        std::thread::Builder::new()
            .name("vaka-sdk".into())
            .spawn(move || worker_loop(config, agent, rx))
            .ok()
    };
    let client = Client {
        config: config.clone(),
        tx: Mutex::new(tx),
        worker: Mutex::new(worker),
        scope: Mutex::new(Scope::default()),
        session: Mutex::new(None),
        agent,
    };
    if CLIENT.set(client).is_err() {
        eprintln!("[vaka] init called twice; ignoring");
        return Guard(());
    }
    if config.sessions {
        start_session();
    }
    if config.capture_panics {
        install_panic_hook();
    }
    Guard(())
}

fn client() -> Option<&'static Client> {
    CLIENT.get()
}

fn send(item: Value) {
    if let Some(c) = client() {
        if let Ok(tx) = c.tx.lock() {
            let _ = tx.send(Msg::Item(item));
        }
    }
}

fn worker_loop(config: Config, agent: ureq::Agent, rx: Receiver<Msg>) {
    let mut queue: Vec<Value> = Vec::new();
    let mut last = Instant::now();
    let mut backoff = Duration::ZERO;
    loop {
        let wait = config.flush_interval.saturating_sub(last.elapsed()).max(Duration::from_millis(10));
        match rx.recv_timeout(wait) {
            Ok(Msg::Item(v)) => {
                let urgent = v["type"] == "error";
                queue.push(v);
                if urgent || queue.len() >= 50 {
                    flush_queue(&config, &agent, &mut queue, &mut backoff);
                    last = Instant::now();
                }
            }
            Ok(Msg::Flush(done)) => {
                flush_queue(&config, &agent, &mut queue, &mut backoff);
                last = Instant::now();
                let _ = done.send(());
            }
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                flush_queue(&config, &agent, &mut queue, &mut backoff);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                if last.elapsed() >= config.flush_interval + backoff {
                    flush_queue(&config, &agent, &mut queue, &mut backoff);
                    last = Instant::now();
                }
            }
        }
    }
}

fn post(config: &Config, agent: &ureq::Agent, items: &[Value]) -> Result<u16, String> {
    let url = format!("{}/ingest/v1/batch", config.endpoint.trim_end_matches('/'));
    let body = json!({"sdk": {"name": SDK_NAME, "version": SDK_VERSION}, "items": items});
    match agent.post(&url).set("X-Vaka-Key", &config.key).send_json(body) {
        Ok(r) => Ok(r.status()),
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            if config.debug {
                eprintln!("[vaka] ingest returned {code}: {text}");
            }
            Ok(code)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn flush_queue(config: &Config, agent: &ureq::Agent, queue: &mut Vec<Value>, backoff: &mut Duration) {
    if queue.is_empty() {
        return;
    }
    let items: Vec<Value> = std::mem::take(queue);
    match post(config, agent, &items) {
        Ok(code) if code == 429 || code >= 500 => {
            *backoff = (*backoff * 2).clamp(Duration::from_secs(5), Duration::from_secs(300));
            queue.extend(items.into_iter().take(1000));
        }
        Ok(code) => {
            *backoff = Duration::ZERO;
            if config.debug {
                eprintln!("[vaka] sent {} items ({code})", items.len());
            }
        }
        Err(e) => {
            if config.debug {
                eprintln!("[vaka] send failed: {e}");
            }
            *backoff = (*backoff * 2).clamp(Duration::from_secs(5), Duration::from_secs(300));
            queue.extend(items.into_iter().take(1000));
        }
    }
}

/// Send everything queued and wait (up to `timeout`) for it to finish.
pub fn flush(timeout: Duration) -> bool {
    let Some(c) = client() else { return false };
    let (done_tx, done_rx) = mpsc::channel();
    if let Ok(tx) = c.tx.lock() {
        if tx.send(Msg::Flush(done_tx)).is_err() {
            return false;
        }
    }
    done_rx.recv_timeout(timeout).is_ok()
}

fn shutdown(timeout: Duration) {
    let Some(c) = client() else { return };
    end_session("exited");
    if let Ok(tx) = c.tx.lock() {
        let _ = tx.send(Msg::Shutdown);
    }
    let handle = c.worker.lock().ok().and_then(|mut w| w.take());
    if let Some(h) = handle {
        let deadline = Instant::now() + timeout;
        while !h.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if h.is_finished() {
            let _ = h.join();
        }
    }
}

fn common(c: &Client) -> Map<String, Value> {
    let mut m = Map::new();
    if let Some(r) = &c.config.release {
        m.insert("release".into(), json!(r));
    }
    m.insert("environment".into(), json!(c.config.environment));
    if let Ok(s) = c.scope.lock() {
        if let Some(u) = &s.user_id {
            m.insert("user_id".into(), json!(u));
        }
    }
    m
}

fn session_id() -> Option<String> {
    client()?.session.lock().ok()?.as_ref().map(|s| s.id.clone())
}

fn start_session() {
    let Some(c) = client() else { return };
    let s = Session { id: new_id(), started_ms: now_ms(), started: Instant::now(), errors: 0, distinct_id: new_id() };
    let item = session_item(c, &s, "ok");
    if let Ok(mut slot) = c.session.lock() {
        *slot = Some(s);
    }
    send(item);
}

fn session_item(c: &Client, s: &Session, status: &str) -> Value {
    let mut m = common(c);
    m.insert("type".into(), json!("session"));
    m.insert("session_id".into(), json!(s.id));
    m.insert("status".into(), json!(status));
    m.insert("started".into(), json!(s.started_ms));
    m.insert("timestamp".into(), json!(now_ms()));
    m.insert("duration_ms".into(), json!(s.started.elapsed().as_millis() as u64));
    m.insert("errors".into(), json!(s.errors));
    m.insert("platform".into(), json!("rust"));
    m.insert("anon_id".into(), json!(s.distinct_id));
    Value::Object(m)
}

fn end_session(status: &str) -> Option<Value> {
    let c = client()?;
    let s = c.session.lock().ok()?.take()?;
    let status = if status == "exited" && s.errors > 0 { "errored" } else { status };
    let item = session_item(c, &s, status);
    send(item.clone());
    Some(item)
}

/// Set (or clear) the identified user id. Opt-in: only call this if your privacy policy allows it.
pub fn identify(user_id: Option<&str>) {
    if let Some(c) = client() {
        if let Ok(mut s) = c.scope.lock() {
            s.user_id = user_id.map(|u| u.to_string());
        }
    }
}

/// Set a tag included with every error.
pub fn set_tag(key: &str, value: &str) {
    if let Some(c) = client() {
        if let Ok(mut s) = c.scope.lock() {
            s.tags.insert(key.into(), value.into());
        }
    }
}

/// Record a breadcrumb (sent with the next error).
pub fn add_breadcrumb(category: &str, message: &str, level: Level) {
    if let Some(c) = client() {
        if let Ok(mut s) = c.scope.lock() {
            s.breadcrumbs.push(json!({"timestamp": now_ms(), "category": category, "message": message, "level": level.as_str()}));
            let n = s.breadcrumbs.len();
            if n > MAX_BREADCRUMBS {
                s.breadcrumbs.drain(..n - MAX_BREADCRUMBS);
            }
        }
    }
}

/// Track a custom analytics event.
pub fn track(name: &str, props: &[(&str, &str)]) {
    let Some(c) = client() else { return };
    let mut m = common(c);
    m.insert("type".into(), json!("event"));
    m.insert("name".into(), json!(name));
    m.insert("platform".into(), json!("rust"));
    if let Some(sid) = session_id() {
        m.insert("session_id".into(), json!(sid));
        if let Ok(s) = c.session.lock() {
            if let Some(s) = s.as_ref() {
                m.insert("anon_id".into(), json!(s.distinct_id));
            }
        }
    }
    let p: Map<String, Value> = props.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    m.insert("props".into(), Value::Object(p));
    send(Value::Object(m));
}

fn error_item(c: &Client, level: Level, handled: bool, exceptions: Vec<Value>, message: Option<String>) -> Value {
    let mut m = common(c);
    m.insert("type".into(), json!("error"));
    m.insert("event_id".into(), json!(new_id()));
    m.insert("timestamp".into(), json!(now_ms()));
    m.insert("platform".into(), json!("rust"));
    m.insert("level".into(), json!(level.as_str()));
    m.insert("handled".into(), json!(handled));
    m.insert("fatal".into(), json!(level == Level::Fatal));
    if let Some(msg) = message {
        m.insert("message".into(), json!(msg));
    }
    m.insert("exceptions".into(), Value::Array(exceptions));
    if let Ok(s) = c.scope.lock() {
        m.insert("breadcrumbs".into(), Value::Array(s.breadcrumbs.clone()));
        m.insert("tags".into(), json!(s.tags));
    }
    m.insert("debug_images".into(), json!(debug_images()));
    m.insert("contexts".into(), json!({"os": {"name": std::env::consts::OS, "arch": std::env::consts::ARCH}}));
    m.insert("device".into(), json!({"os_name": std::env::consts::OS}));
    if let Some(sid) = session_id() {
        m.insert("session_id".into(), json!(sid));
    }
    Value::Object(m)
}

fn note_error() {
    if let Some(c) = client() {
        if let Ok(mut s) = c.session.lock() {
            if let Some(s) = s.as_mut() {
                s.errors += 1;
            }
        }
    }
}

/// Report a handled error, including its `source()` chain and the current stack.
pub fn capture_error(err: &(dyn std::error::Error + 'static)) {
    let Some(c) = client() else { return };
    let frames = capture_backtrace(0, &c.config.in_app_include);
    let mut exceptions = Vec::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    let mut first = true;
    while let Some(e) = cur {
        exceptions.push(json!({
            "type": stack::type_name_of_error(e),
            "value": e.to_string(),
            "stacktrace": if first { frames.clone() } else { Vec::new() },
        }));
        first = false;
        cur = e.source();
        if exceptions.len() >= 10 {
            break;
        }
    }
    note_error();
    send(error_item(c, Level::Error, true, exceptions, None));
}

/// Report a message with the current stack.
pub fn capture_message(message: &str, level: Level) {
    let Some(c) = client() else { return };
    let frames = capture_backtrace(0, &c.config.in_app_include);
    let exc = json!({"type": "Message", "value": message, "stacktrace": frames});
    if matches!(level, Level::Error | Level::Fatal) {
        note_error();
    }
    send(error_item(c, level, true, vec![exc], Some(message.to_string())));
}

/// A timed operation for performance tracing. Finishes (and is sent) when dropped or when
/// [`Span::finish`] is called.
///
/// ```no_run
/// let mut req = vaka_sdk::start_span("GET /users/:id", "http.server");
/// {
///     let _db = req.child("SELECT users", "db");
///     // ... query ...
/// }
/// req.set_attribute("http.status_code", "200");
/// ```
pub struct Span {
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    transaction: String,
    name: String,
    op: String,
    status: &'static str,
    start_ms: i64,
    started: Instant,
    attributes: Map<String, Value>,
    measurements: Map<String, Value>,
    remote_parent: bool,
    done: bool,
}

fn hex_id(len: usize) -> String {
    uuid::Uuid::new_v4().simple().to_string()[..len].to_string()
}

/// Start a root span (a transaction) in a new trace. Use `op` such as `http.server`, `job`,
/// `function` or `app.start.cold`.
pub fn start_span(name: &str, op: &str) -> Span {
    Span::new(hex_id(32), None, name.to_string(), name, op)
}

/// Continue a trace from a W3C `traceparent` header (`00-<trace id>-<parent span id>-01`).
/// Falls back to a new trace if the header is malformed.
pub fn continue_span(traceparent: &str, name: &str, op: &str) -> Span {
    let parts: Vec<&str> = traceparent.trim().split('-').collect();
    if parts.len() >= 3 && parts[1].len() == 32 && parts[2].len() == 16 {
        let mut s = Span::new(parts[1].to_lowercase(), Some(parts[2].to_lowercase()), name.to_string(), name, op);
        // The incoming parent lives in another service: this span is this service's transaction.
        s.remote_parent = true;
        return s;
    }
    start_span(name, op)
}

impl Span {
    fn new(trace_id: String, parent: Option<String>, transaction: String, name: &str, op: &str) -> Span {
        Span {
            trace_id,
            span_id: hex_id(16),
            parent_span_id: parent,
            transaction,
            name: name.to_string(),
            op: op.to_string(),
            status: "ok",
            start_ms: now_ms(),
            started: Instant::now(),
            attributes: Map::new(),
            measurements: Map::new(),
            remote_parent: false,
            done: false,
        }
    }

    /// Start a child span in the same trace.
    pub fn child(&self, name: &str, op: &str) -> Span {
        Span::new(self.trace_id.clone(), Some(self.span_id.clone()), self.transaction.clone(), name, op)
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    pub fn span_id(&self) -> &str {
        &self.span_id
    }

    /// The W3C `traceparent` header value to pass this trace on to another service.
    pub fn traceparent(&self) -> String {
        format!("00-{}-{}-01", self.trace_id, self.span_id)
    }

    pub fn set_attribute(&mut self, key: &str, value: &str) {
        self.attributes.insert(key.to_string(), json!(value));
    }

    pub fn set_measurement(&mut self, key: &str, value: f64) {
        self.measurements.insert(key.to_string(), json!(value));
    }

    /// Mark the span as failed.
    pub fn set_error(&mut self) {
        self.status = "error";
    }

    /// Finish and send the span now.
    pub fn finish(mut self) {
        self.send_once();
    }

    fn send_once(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        let Some(c) = client() else { return };
        let mut m = common(c);
        m.insert("type".into(), json!("span"));
        m.insert("platform".into(), json!("rust"));
        m.insert("trace_id".into(), json!(self.trace_id));
        m.insert("span_id".into(), json!(self.span_id));
        if let Some(p) = &self.parent_span_id {
            m.insert("parent_span_id".into(), json!(p));
        }
        if self.remote_parent {
            m.insert("is_transaction".into(), json!(true));
        }
        m.insert("name".into(), json!(self.name));
        m.insert("op".into(), json!(self.op));
        m.insert("status".into(), json!(self.status));
        m.insert("transaction".into(), json!(self.transaction));
        m.insert("start_timestamp".into(), json!(self.start_ms));
        m.insert("duration_ms".into(), json!(self.started.elapsed().as_secs_f64() * 1000.0));
        if !self.attributes.is_empty() {
            m.insert("attributes".into(), Value::Object(std::mem::take(&mut self.attributes)));
        }
        if !self.measurements.is_empty() {
            m.insert("measurements".into(), Value::Object(std::mem::take(&mut self.measurements)));
        }
        if let Some(sid) = session_id() {
            m.insert("session_id".into(), json!(sid));
        }
        send(Value::Object(m));
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        self.send_once();
    }
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(c) = client() {
            let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "panic".to_string()
            };
            let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
            let frames = capture_backtrace(0, &c.config.in_app_include);
            let frames = stack::trim_panic_frames(frames);
            let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
            let exc = json!({"type": "panic", "value": msg, "stacktrace": frames});
            let mut item = error_item(c, Level::Fatal, false, vec![exc], None);
            item["tags"]["thread"] = json!(thread);
            item["tags"]["location"] = json!(location);
            // Send synchronously: the process may abort right after the hook returns.
            let mut items = vec![item];
            if let Some(s) = end_session_value("crashed") {
                items.push(s);
            }
            let _ = post(&c.config, &c.agent, &items);
        }
        prev(info);
    }));
}

fn end_session_value(status: &str) -> Option<Value> {
    let c = client()?;
    let s = c.session.lock().ok()?.take()?;
    Some(session_item(c, &s, status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_nest_and_continue() {
        let root = start_span("GET /a", "http.server");
        let child = root.child("SELECT", "db");
        assert_eq!(child.trace_id(), root.trace_id());
        assert_eq!(child.parent_span_id.as_deref(), Some(root.span_id()));
        assert_eq!(root.trace_id().len(), 32);
        assert_eq!(root.span_id().len(), 16);
        let tp = root.traceparent();
        let cont = continue_span(&tp, "job", "job");
        assert_eq!(cont.trace_id(), root.trace_id());
        assert_eq!(cont.parent_span_id.as_deref(), Some(root.span_id()));
        assert_eq!(continue_span("garbage", "x", "y").parent_span_id, None);
    }

    #[test]
    fn levels() {
        assert_eq!(Level::Fatal.as_str(), "fatal");
    }

    #[test]
    fn backtrace_is_crash_site_first() {
        #[inline(never)]
        fn inner() -> Vec<Value> {
            capture_backtrace(0, &[])
        }
        let frames = inner();
        assert!(!frames.is_empty());
        let names: Vec<String> = frames.iter().filter_map(|f| f["function"].as_str().map(|s| s.to_string())).collect();
        let pos = names.iter().position(|n| n.contains("inner"));
        assert!(pos.is_some(), "frames: {names:?}");
    }
}
