# iOS SDK (Swift)

Package: `sdks/ios` (Swift Package `Vaka`, iOS 15+, macOS 12+, tvOS 15+). Apps depend on it by repository URL
(`https://github.com/ErikHellman/vaka`, product `Vaka`) through the root `Package.swift`.
Install steps: [install/ios.md](install/ios.md).

## API

```swift
// App init / application(_:didFinishLaunchingWithOptions:)
Vaka.start(Vaka.Options(
    key: "vk_...",
    endpoint: URL(string: "https://vaka.example.com")!,
    release: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String,   // default
    environment: "production"                                                          // default
))

Vaka.screen("Checkout")                      // screen_view
Vaka.track("purchase", ["plan": "pro"])      // custom event
Vaka.capture(error)                          // handled Error, with stack trace
Vaka.capture(message: "Cache rebuilt", level: .warning)
Vaka.addBreadcrumb(category: "cart", message: "Added item 42")
Vaka.identify("user-123")                    // opt-in; Vaka.identify(nil) clears
Vaka.setTag("flavor", "free")                // added to later errors
Vaka.flush()                                 // send now
```

`Vaka.Options` also takes `crashReporting`, `sessionTracking`, `appHangDetection` (on), `appHangThreshold` (2 s),
`sessionTimeout` (30 s), `flushInterval` (10 s),
`maxBatchSize` (50), `debug` (console logging), `enabled` (false = no-op), `tags` and `storageDirectory`.
All calls are thread-safe and do nothing before `start`.

## User feedback

Users can tell you what went wrong in their own words, with a screenshot. Feedback is linked to the crash
or hang it is about. See [feedback](feedback.md).

```swift
// After Vaka.start: options for the form, and ask about crashes and hangs.
Vaka.installFeedback(Vaka.FeedbackOptions(crashPrompt: true, email: currentUser?.email))

// UIKit, or anywhere: open the form over the current screen, with a screenshot of it.
Vaka.showFeedback()

// SwiftUI
VakaFeedbackButton()                              // "Send feedback" button
Button("Report a problem") { show = true }
    .vakaFeedback(isPresented: $show)

// From code (support forms, your own UI). Completion runs on the main queue.
let r = try await Vaka.captureFeedback(Vaka.Feedback(message: text, email: email, eventId: Vaka.lastEventId))
// r.shortId == "IOS-F12", or r.queued == true when offline (sent later)
```

| `FeedbackOptions` | Default | Meaning |
| --- | --- | --- |
| `screenshot` | `true` | Attach a screenshot of the key window; the user can remove it |
| `askEmail` | `true` | Show the optional email field |
| `email` | | Prefill the email, e.g. for signed-in users |
| `crashPrompt` | `false` | With `installFeedback`: after a crash, ask on the next launch what the user was doing |
| `hangPrompt` | `true` | With `crashPrompt`: also ask when the app recovers from a hang (or was killed while hung) |
| `tags` | | Tags added to feedback sent from the form |
| `strings` | English | `Vaka.FeedbackStrings`: every text in the form, for localization |

- The form is SwiftUI on every platform: a sheet on iOS and tvOS, a sheet on the key window on macOS.
  UIKit apps can present `VakaFeedbackViewController` themselves; SwiftUI apps can embed `VakaFeedbackView`.
- Screenshots: `Vaka.screenshot()` renders the key window to a JPEG (at most 2x, 4096 px, 5 MB) before the form
  opens. Secure text fields are blacked out, and so are views marked with `.vakaMask()` (SwiftUI) or
  `view.vakaMask = true` (UIKit, AppKit). Metal and video content may come out blank.
- Up to 3 images. On iOS 16+ and macOS 13+ users can add photos from their library; `Vaka.Attachment.image(data)`
  converts any image (HEIC included) to a JPEG within the limits, with metadata removed.
- Crash prompt: shown at most once per app run, when the app is active and has a window. Feedback from it has
  `source` `crash_prompt` and the crash's `event_id`. Build your own prompt with `Vaka.onCrashOrHang { report in ... }`
  (`report.kind`, `report.eventId`, `report.previousRun`) and `VakaFeedbackView(crash: report)`.
- Offline: feedback that can't be sent (no network, 5xx) is saved under `Application Support/Vaka/feedback/` and sent
  on the next launch, foreground or `flush()`; the server stores retries once. 429 (hourly limit) and other 4xx
  are not retried. At most 20 are kept.
- The UI APIs are unavailable in app extensions; `captureFeedback` works everywhere.

## Behaviour

- Crashes: POSIX signal handlers (SIGSEGV, SIGBUS, SIGILL, SIGTRAP, SIGABRT, SIGFPE, SIGSYS, on an alternate stack)
  and `NSSetUncaughtExceptionHandler` write a small text report to disk using only async-signal-safe calls
  (frame-pointer stack walk with `vm_read_overwrite`, loaded images with `LC_UUID` collected beforehand from dyld).
  On the next launch it is sent as a `fatal` error with `instruction_addr` frames and `debug_images`
  (only the images that frames point into), plus the breadcrumbs, user, tags, release and session of the run that
  crashed. That session is reported as `crashed`. Previous handlers are restored and the signal re-raised, so the
  system crash report is still produced.
- Signals map to the Mach exception names users know: SIGSEGV/SIGBUS → `EXC_BAD_ACCESS`, SIGTRAP → `EXC_BREAKPOINT`
  (Swift runtime traps: force-unwrap of nil, out-of-range index, `fatalError`), SIGILL → `EXC_BAD_INSTRUCTION`,
  SIGFPE → `EXC_ARITHMETIC`. A Mach exception server is not used in v1: every crash type above already arrives
  as a signal, and a Mach handler conflicts with debuggers.
- Function names: the SDK fills `function` in-process where `dladdr` can (for crash reports, by matching image
  UUIDs in the new process). Release builds are stripped, so file and line come from server-side symbolication with
  the uploaded dSYM.
- `in_app`: frames in images inside the app bundle (main executable, embedded frameworks, `*.debug.dylib`).
- App hangs: a watchdog thread pings the main queue. If it gets no answer within `appHangThreshold` (2 s), it
  samples the main thread's stack (SIGURG to the main thread, frame-pointer walk from the interrupted context) and
  writes an error to disk right away, so it survives the app being killed: `exceptions[0].type` `AppHang`, value
  `Application not responding for 2000 ms`, stack trace crash site first, `tags.mechanism` `hang`, `level` `error`,
  `handled` and `fatal` false. One report per hang. Skipped while the app is in background or a debugger is
  attached. If the app is killed while still hung (e.g. by the iOS watchdog), that session is sent as `abnormal` on
  the next launch.
- Sessions: start on `didBecomeActive`, end after 30 s in background (`exited`, or `errored` if handled errors were
  captured). A session that was never ended (the app was killed in the foreground) is sent as `abnormal` on the next
  launch. On macOS a session lasts from `start` to termination.
- Queue: items are batched in memory and written to `Application Support/Vaka/pending/` before each send; a batch file
  is deleted only after a 2xx (or a permanent 4xx). Sends are gzipped, every 10 s, at 50 items and when the app goes
  to the background. 429/5xx/offline retry with exponential backoff (5 s doubling to 5 min); 401 stops sending until
  the next launch; 413 splits the batch. At most 200 batch files are kept.
- `anon_id`: random UUID in `UserDefaults`.
- dSYMs: upload in an Xcode build phase with
  `vaka upload dsym --release "$MARKETING_VERSION" "$DWARF_DSYM_FOLDER_PATH"`, or
  `POST /api/v1/projects/{p}/artifacts` with `kind=native`.

## Development

The package builds and its tests run on Linux too (the crash handler uses ELF build ids there), which is how CI
and agents without a Mac test it:

```sh
cd sdks/ios && swift test
# end-to-end against a local server:
VAKA_TEST_ENDPOINT=http://localhost:8080 VAKA_TEST_KEY=vk_... swift test --filter IntegrationTests
```

See [ingest.md](ingest.md) for the wire format, especially "Native crashes".
