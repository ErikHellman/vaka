# Vaka iOS SDK

Crash reporting (signals, uncaught exceptions and app hangs), release health and privacy-friendly
analytics for iOS, macOS and tvOS apps.

- Install (agent-followable): [docs/install/ios.md](../../docs/install/ios.md)
- API and behaviour: [docs/sdk-ios.md](../../docs/sdk-ios.md)

Layout: `Sources/VakaNative` is the C core (async-signal-safe crash handler, loaded-image table, gzip);
`Sources/Vaka` is the Swift API, disk queue, sessions and transport. `swift test` runs on macOS and Linux.
