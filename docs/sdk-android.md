# Android SDK (Kotlin)

Module: `sdks/android` (library `se.hellsoft.vaka:vaka-android`, plus the Gradle plugin
`se.hellsoft.vaka` for R8 mapping upload). Step-by-step setup: [install/android.md](install/android.md).

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
Vaka.captureException(e)                          // handled exception; returns the event id
Vaka.captureMessage("Slow payment", VakaLevel.WARNING)
Vaka.addBreadcrumb("cart", "Added item 42")
Vaka.setTag("flavor", "free")                     // tag on later errors; null removes it
Vaka.identify("user-123")                         // opt-in user id; Vaka.identify(null) clears it
Vaka.flush()                                      // send the queue now
```

All methods are no-ops before `init` and safe from any thread. Java callers use the same static methods.

### VakaConfig

| Field | Default | |
| --- | --- | --- |
| `key`, `endpoint`, `release` | required | `release` must match the mapping upload (`versionName` by default) |
| `environment` | `production` | |
| `inAppPackages` | app package name | frames from these packages, and any non-framework class (R8-renamed included), are `in_app` |
| `autoScreenViews` | `true` | `screen_view` per resumed Activity, named after its class |
| `autoBreadcrumbs` | `true` | Activity lifecycle and connectivity |
| `captureUncaughtExceptions` | `true` | |
| `sessionTracking` | `true` | |
| `flushIntervalMillis` / `maxBatchSize` | 10 000 / 50 | |
| `maxQueueSize` | 1000 | oldest items dropped beyond this |
| `sessionTimeoutMillis` | 30 000 | background time that ends a session |
| `tags` | empty | tags on every error |
| `enabled` | `true` | `false` makes the SDK a no-op |
| `debug` | `false` | log to Logcat, tag `Vaka` |

## Behaviour

- Uncaught JVM exceptions: a `Thread.UncaughtExceptionHandler` writes the error and a `crashed`
  session update to disk synchronously, then calls the previous handler. Both are sent on next launch.
- Sessions: start when the first Activity starts (foreground), end after 30 s in the background as
  `exited`, or `errored` if handled errors were reported. A session left open by a process that died
  in the foreground is reported as `abnormal` on the next launch; one that died in the background is
  closed as `exited`/`errored`. Foreground detection counts started Activities, the same signal as
  `ProcessLifecycleOwner`, so no androidx dependency is needed.
- Automatic breadcrumbs: Activity lifecycle, network connectivity changes (API 24+).
- Offline queue on disk (one file per item in `filesDir/vaka/queue`), batched sends every 10 s or 50
  items, gzip, exponential backoff up to 5 min on 429/5xx and network errors, batch splitting on 413,
  sending paused until restart on 401.
- `anon_id`: random UUID stored in app-private SharedPreferences (`se.hellsoft.vaka`), as is the
  `identify` user id.
- No dependencies beyond the Android SDK and Kotlin stdlib (`HttpURLConnection`, hand-written JSON).
- minSdk 21.

## Gradle plugin

`id("se.hellsoft.vaka")` registers `uploadVakaMapping<Variant>` for each minified application variant
and runs it after `minify<Variant>WithR8`. It POSTs the mapping to
`/api/v1/projects/{project}/artifacts` (`kind=proguard`, `release=versionName`).

```kotlin
vaka {
    url.set("https://vaka.example.com")   // or VAKA_URL / -Pvaka.url
    project.set("my-app")                 // or VAKA_PROJECT / -Pvaka.project
    token.set(...)                        // default: VAKA_TOKEN / -Pvaka.token (API token, write scope)
    release.set("2.3.1")                  // default: the variant's versionName
    uploadMapping.set(false)              // or VAKA_UPLOAD_MAPPING=false, to skip
    failOnError.set(true)                 // default false: a failed upload only logs a warning
}
```

See [ingest.md](ingest.md) for the wire format.
