# vaka-sdk

Rust SDK for [Vaka](https://github.com/ErikHellman/vaka), an analytics and crash-reporting
service built for agents. It captures panics (with crash-site-first backtraces), errors,
release health sessions, breadcrumbs, analytics events and performance spans.

Blocking HTTP on a background thread, so it works with or without an async runtime.

```toml
[dependencies]
vaka-sdk = { version = "0.3", features = ["tracing"] }
```

Drop `features = ["tracing"]` if the app does not use the `tracing` crate.

```rust
fn main() {
    let _vaka = vaka_sdk::init(vaka_sdk::Config {
        key: "your-ingest-key".into(),
        endpoint: "https://your-vaka-server".into(),
        release: Some(env!("CARGO_PKG_VERSION").into()),
        ..Default::default()
    });
    // Keep `_vaka` alive until exit: dropping it ends the session and flushes pending data.
}
```

`Config::default()` reads `VAKA_KEY`, `VAKA_ENDPOINT`, `VAKA_RELEASE` and `VAKA_ENVIRONMENT`.
With an empty key the SDK is a no-op.

```rust
vaka_sdk::capture_error(&err);                     // any std::error::Error, with source chain
vaka_sdk::capture_message("text", vaka_sdk::Level::Warning);
vaka_sdk::add_breadcrumb("category", "message", vaka_sdk::Level::Info);
vaka_sdk::track("event_name", &[("prop", "value")]);
```

Full documentation: [docs/sdk-rust.md](https://github.com/ErikHellman/vaka/blob/main/docs/sdk-rust.md).

## License

MIT OR Apache-2.0
