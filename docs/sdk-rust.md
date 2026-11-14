# Rust SDK

Crate `vaka-sdk` (source: `crates/vaka-sdk-rust`). Blocking HTTP on a background thread, so it
works with or without an async runtime.

Agent-followable install guide: [`/docs/install/rust`]({{ENDPOINT}}/docs/install/rust).

```toml
vaka-sdk = { version = "0.2", features = ["tracing"] }
```

## Config

| Field | Default | Meaning |
| --- | --- | --- |
| `key` | `$VAKA_KEY` | Ingest key; empty disables the SDK |
| `endpoint` | `$VAKA_ENDPOINT` | Server URL |
| `release` | `$VAKA_RELEASE` | App version |
| `environment` | `$VAKA_ENVIRONMENT` or `production` | |
| `capture_panics` | `true` | Install a panic hook |
| `sessions` | `true` | One session per process for crash-free rates |
| `in_app_include` | empty | Path prefixes that count as your code, e.g. `["myapp::"]`; empty means everything except std, core, alloc, tokio and similar runtime frames |
| `flush_interval` | 5 s | Batch interval |
| `debug` | `false` | Log SDK activity to stderr |

## API

```rust
let _guard = vaka_sdk::init(config);        // drop = end session + flush
vaka_sdk::capture_error(&err);              // any std::error::Error, with source chain
vaka_sdk::capture_message("text", Level::Warning);
vaka_sdk::add_breadcrumb("category", "message", Level::Info);
vaka_sdk::set_tag("key", "value");
vaka_sdk::track("event_name", &[("prop", "value")]);
vaka_sdk::identify(Some("user-123"));       // opt-in
vaka_sdk::flush(Duration::from_secs(2));
```

Performance spans (see [performance](performance.md)):

```rust
let mut req = vaka_sdk::start_span("GET /users/:id", "http.server"); // or continue_span(traceparent, ..)
{
    let _q = req.child("SELECT users", "db"); // sent when dropped
}
req.set_attribute("route", "users");
outgoing.header("traceparent", req.traceparent());
req.finish();                                  // or drop it
```

With feature `tracing`, `vaka_sdk::tracing_layer::VakaLayer` turns `tracing` events into
breadcrumbs, and `error!` events into Vaka errors.

## User feedback

Forward what users wrote in your own forms (support, contact, in-app chat). The call blocks until
the server answers, so call it from `spawn_blocking` in async code.

```rust
let r = vaka_sdk::capture_feedback(vaka_sdk::Feedback {
    message: form.message,
    email: form.email,                    // optional contact
    issue: Some("API-12".into()),         // or event_id: vaka_sdk::last_event_id()
    ip: Some(client_ip.to_string()),
    attachments: vec![vaka_sdk::Attachment { filename: "screenshot.png".into(), content_type: None, data: png }],
    ..Default::default()
})?;
println!("{}", r.short_id); // "API-F12"
```

Errors are `FeedbackError::NotInitialized`, `Invalid`, `Status(code, message)` (429: rate
limited) or `Transport`. `last_event_id()` returns the id of the last error, message or panic sent.

Link it to a crash with the event id from a capture call, or to an issue by its short id (`API-12`). Pass the end user's IP address: feedback is rate limited per address (10 an hour by default), so without it all feedback from your server shares one limit. Rejected feedback raises an error with the HTTP status; 429 means a limit was hit. See [user feedback](feedback.md).

## Panics and symbols

Panics are captured in the panic hook and sent synchronously, so they arrive even when
`panic = "abort"`. Backtraces are resolved in-process when the binary has symbols. For stripped
binaries, the SDK sends instruction addresses and the binary's build id; upload the unstripped
binary or its debug file with `vaka upload debug-files`.

See `crates/vaka-sdk-rust/examples/demo.rs` for a runnable example.
