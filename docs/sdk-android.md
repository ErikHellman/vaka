# Android SDK (Kotlin)

Module: `sdks/android` (Gradle library `vaka-android`, plus a Gradle plugin for R8 mapping upload).

## API

```kotlin
// Application.onCreate
Vaka.init(this, VakaConfig(
    key = "vk_...",
    endpoint = "https://vaka.example.com",
    release = BuildConfig.VERSION_NAME,
    environment = if (BuildConfig.DEBUG) "debug" else "production",
))

Vaka.screen("Checkout")                          // screen_view; automatic for Activities by default
Vaka.track("purchase", mapOf("plan" to "pro"))   // custom event
Vaka.captureException(e)                          // handled exception
Vaka.addBreadcrumb("cart", "Added item 42")
Vaka.identify("user-123")                         // opt-in user id; Vaka.identify(null) clears it
```

## Behaviour

- Uncaught JVM exceptions: install a `Thread.UncaughtExceptionHandler` that writes the event to
  disk synchronously, marks the session `crashed`, then calls the previous handler. Send on next launch.
- Sessions: start on foreground (ProcessLifecycleOwner), end after 30 s in background (`exited`).
- Automatic breadcrumbs: activity lifecycle, network connectivity changes.
- Offline queue on disk, batched sends every 10 s or 50 items, gzip, exponential backoff on 429/5xx.
- `anon_id`: random UUID stored in app-private SharedPreferences.
- No dependencies beyond the Android SDK and Kotlin stdlib (use `HttpURLConnection`).
- R8: the Gradle plugin runs `vaka upload proguard --release <versionName> mapping.txt` (or the
  REST API directly) after `minify<Variant>WithR8`.

See [ingest.md](ingest.md) for the wire format.
