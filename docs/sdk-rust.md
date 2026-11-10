# Rust SDK

Crate `vaka-sdk` (source: `crates/vaka-sdk-rust`). Blocking HTTP on a background thread, so it
works with or without an async runtime.

Agent-followable install guide: [`/docs/install/rust`]({{ENDPOINT}}/docs/install/rust).

```toml
vaka-sdk = { git = "https://github.com/ErikHellman/vaka", features = ["tracing"] }
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

With feature `tracing`, `vaka_sdk::tracing_layer::VakaLayer` turns `tracing` events into
breadcrumbs, and `error!` events into Vaka errors.

## Panics and symbols

Panics are captured in the panic hook and sent synchronously, so they arrive even when
`panic = "abort"`. Backtraces are resolved in-process when the binary has symbols. For stripped
binaries, the SDK sends instruction addresses and the binary's build id; upload the unstripped
binary or its debug file with `vaka upload debug-files`.

See `crates/vaka-sdk-rust/examples/demo.rs` for a runnable example.
