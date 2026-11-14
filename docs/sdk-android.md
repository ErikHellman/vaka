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
Vaka.crashNativeForTesting()                      // SIGSEGV in native code, to test the setup

// User feedback (see below)
Vaka.installFeedback(VakaFeedbackOptions(crashPrompt = true))
Vaka.showFeedback()
Vaka.captureFeedback(VakaFeedback("The export button does nothing", eventId = Vaka.lastEventId()))
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
| `captureNativeCrashes` | `true` | fatal signals in C/C++ code (`libvaka.so` signal handler) |
| `captureAnrs` | `null` | ANR watchdog; `null` = on unless the app is debuggable |
| `anrTimeoutMillis` | 5 000 | main thread blocked this long counts as an ANR (min 1 000) |
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
- ANRs: a watchdog thread posts a tick to the main thread every `anrTimeoutMillis`. If it has not
  run by the next check, the main thread's stack is reported as an error with `exceptions[0].type`
  `ANR`, value `Application not responding for 5000 ms`, tag `mechanism: anr`, `fatal: false`,
  `handled: false`. One report per hang; it re-arms when the main thread responds. The item is
  written to disk synchronously; if the app is killed before it recovers, the session ends as
  `abnormal` on the next launch. Off while a debugger is attached, and by default in debuggable builds.
- Native crashes: `libvaka.so` installs a signal handler (SIGSEGV, SIGABRT, SIGBUS, SIGFPE,
  SIGILL, SIGTRAP, SIGSYS) that writes a small text report to `filesDir/vaka/native`: signal,
  code, fault address, the backtrace as raw addresses and the loaded ELF images with their GNU
  build ids (no minidump). It then hands the signal to the previous handler, so the system
  tombstone still happens. On the next launch the report becomes a fatal error (tag
  `mechanism: signal`, type e.g. `SIGSEGV`, `debug_images` with `code_id`) and the session ends as
  `crashed`. Upload unstripped `.so` files (`vaka upload debug-files`) for function names and lines.
  ART's own signal handling (null checks, stack overflow checks) runs first, so only real crashes
  reach the handler. The library adds about 40 KB per ABI.
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

## User feedback

Let users tell you what went wrong, in their own words. See [feedback](feedback.md) for how
feedback is stored, tagged and read by agents.

```kotlin
// Application.onCreate, after Vaka.init
Vaka.installFeedback(VakaFeedbackOptions(
    crashPrompt = true,                 // ask "what happened?" on the next launch after a crash
    email = currentUser?.email,         // prefill the optional email field
))

// Your own "Report a problem" menu item (button = false hides the floating button):
Vaka.showFeedback(this)

// Mask personal data in screenshots (password fields are always masked):
Vaka.maskInScreenshots(binding.accountNumber)

// From code, e.g. an in-app support form. Up to 3 PNG/JPEG/WebP images, 5 MB each.
Vaka.captureFeedback(VakaFeedback(message = text, email = email, attachments = listOf(pngBytes))) { result ->
    // result.status: SENT (result.shortId = "AND-F12"), QUEUED (offline) or REJECTED (result.error)
}
```

| `VakaFeedbackOptions` | Default | |
| --- | --- | --- |
| `button` | `true` | floating "Feedback" button on every Activity, above the navigation bar |
| `crashPrompt` | `false` | after a JVM or native crash, show a dialog on the first screen of the next launch (crashes up to 24 h old); the answer is linked to that crash's issue |
| `screenshot` | `true` | attach a screenshot of the current screen; the user sees it and can untick it |
| `askEmail` | `true` | show the optional email field |
| `email` | | prefill the email |
| `accentColor` | `0xFF2563EB` | button color |
| `bottomLeft` | `false` | button in the bottom-left corner |
| `excludeActivities` | empty | Activity classes without the button |
| `strings` | English | `VakaFeedbackStrings`: `button`, `title`, `crashTitle`, `crashIntro`, `placeholder`, `emailLabel`, `screenshotLabel`, `submit`, `cancel`, `sending`, `thanks`, `error`, `messageRequired` |
| `onSubmit` | | called on the main thread with the `VakaFeedbackResult` |

- The dialog is a plain `AlertDialog` built with framework views, so it follows your app's dialog
  theme and needs no AndroidX dependency. Feedback is sent with the session, user id, current
  screen, release and tags.
- Screenshots: `PixelCopy` of the window (API 26+, so `SurfaceView`, maps and video are included)
  or `View.draw` on older versions, scaled to at most 1600 px and sent as JPEG. The feedback
  button is hidden while capturing. Windows with `FLAG_SECURE` are never captured, and password
  fields and views passed to `maskInScreenshots` are blacked out.
- Feedback is written to `filesDir/vaka/feedback` before it is sent, so it survives being
  offline and restarts (at most 10 pending). Rejected feedback (`400`, or `429` when the hourly
  limit is hit) is dropped, not retried.

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
