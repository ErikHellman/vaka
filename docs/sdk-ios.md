# iOS SDK (Swift)

Package: `sdks/ios` (Swift Package `Vaka`, iOS 15+, macOS 12+).

## API

```swift
// App init / application(_:didFinishLaunchingWithOptions:)
Vaka.start(Vaka.Options(
    key: "vk_...",
    endpoint: URL(string: "https://vaka.example.com")!,
    release: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String,
    environment: "production"
))

Vaka.screen("Checkout")                      // screen_view
Vaka.track("purchase", ["plan": "pro"])      // custom event
Vaka.capture(error)                          // handled Error
Vaka.addBreadcrumb(category: "cart", message: "Added item 42")
Vaka.identify("user-123")                    // opt-in; Vaka.identify(nil) clears
```

## Behaviour

- Crashes: Mach exception / POSIX signal handlers and `NSSetUncaughtExceptionHandler` write a
  minimal crash report (instruction addresses, loaded images with `LC_UUID` debug ids) to disk
  using only async-signal-safe calls; it is sent as a `fatal` error with `debug_images` on next
  launch, and the previous session is marked `crashed`.
- Sessions: start on `didBecomeActive`, end after 30 s in background.
- Offline queue, batching, gzip, backoff like the Android SDK.
- `anon_id`: random UUID in `UserDefaults`.
- dSYMs: upload in an Xcode build phase with `vaka upload dsym --release "$MARKETING_VERSION" "$DWARF_DSYM_FOLDER_PATH"`.

See [ingest.md](ingest.md) for the wire format, especially "Native crashes".
