# Install Vaka in a Rust application

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.

## 1. Add the dependency

```toml
[dependencies]
vaka-sdk = { version = "0.2", features = ["tracing"] }
```

Drop `features = ["tracing"]` if the app does not use the `tracing` crate.

## 2. Initialise at the top of `main`

```rust
fn main() {
    let _vaka = vaka_sdk::init(vaka_sdk::Config {
        key: "{{KEY}}".into(),
        endpoint: "{{ENDPOINT}}".into(),
        release: Some(env!("CARGO_PKG_VERSION").into()),
        ..Default::default()
    });
    // ... the rest of the program. Keep `_vaka` alive until exit:
    // dropping it ends the session and flushes pending data.
}
```

`Config::default()` reads `VAKA_KEY`, `VAKA_ENDPOINT`, `VAKA_RELEASE` and `VAKA_ENVIRONMENT`,
so keys can come from the environment instead of source code. With an empty key the SDK is a no-op.

For `#[tokio::main]` apps, call `init` first thing inside `main`; the SDK uses its own thread
and does not need a runtime.

## 3. What you get automatically

- Panics, with a crash-site-first backtrace, sent synchronously before the process unwinds or aborts.
- A session per process run: `exited` on clean shutdown, `crashed` after a panic, so
  crash-free rates per release work.

## 4. Report errors and events

```rust
if let Err(e) = do_work() {
    vaka_sdk::capture_error(&e);      // includes the `source()` chain
}
vaka_sdk::capture_message("cache rebuilt", vaka_sdk::Level::Info);
vaka_sdk::add_breadcrumb("db", "connected", vaka_sdk::Level::Info);
vaka_sdk::set_tag("region", "eu-north-1");
vaka_sdk::track("job_finished", &[("kind", "import")]);
vaka_sdk::identify(Some("user-123"));  // opt-in only
```

With the `tracing` feature, add the layer so `info!`/`warn!` become breadcrumbs and
`error!` events become Vaka errors:

```rust
use tracing_subscriber::prelude::*;
tracing_subscriber::registry()
    .with(tracing_subscriber::fmt::layer())
    .with(vaka_sdk::tracing_layer::VakaLayer::default())
    .init();
```

For performance tracing, wrap request handlers and other work in spans (see
[performance]({{ENDPOINT}}/docs/performance.md)):

```rust
let mut req = vaka_sdk::start_span("GET /users/:id", "http.server"); // or continue_span(traceparent, ..)
{
    let _q = req.child("SELECT users", "db"); // sent when dropped
}
req.finish();
```

## 5. Symbols for release builds

Backtraces are symbolicated in-process when the binary has symbols (the default for
`cargo build --release` unless you set `strip = true`). If you strip release binaries, keep
debug info and upload it with the `vaka` CLI after each build:

```toml
[profile.release]
debug = "line-tables-only"
split-debuginfo = "packed"   # optional
```

```sh
export VAKA_URL={{ENDPOINT}} VAKA_TOKEN=vt_... VAKA_PROJECT={{PROJECT}}
vaka upload debug-files target/release/myapp
```

## 6. Verify

```sh
VAKA_URL={{ENDPOINT}} VAKA_KEY={{KEY}} vaka send-test-event
```

Then call `vaka_sdk::capture_message("vaka install test", vaka_sdk::Level::Info)` from the app
and check `vaka issues -p {{PROJECT}}` (or the MCP tool `list_issues`). Events appear within a few
seconds. Set `debug: true` in `Config` to log what the SDK sends.
