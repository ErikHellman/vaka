# Install Vaka in an Android app (Kotlin)

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.
Requirements: minSdk 21+, Android Gradle Plugin 8.x. The SDK has no dependencies besides the
Android SDK and the Kotlin standard library.

## 1. Add the SDK and the Gradle plugin

In `settings.gradle.kts`, make sure `mavenCentral()` is listed in both `pluginManagement.repositories`
and `dependencyResolutionManagement.repositories` (new projects already have it).

In the **app module's** `build.gradle.kts`:

```kotlin
plugins {
    id("com.android.application")
    // ...existing plugins...
    id("se.hellsoft.vaka") version "0.4.0"   // uploads the R8 mapping after each minified build
}

dependencies {
    implementation("se.hellsoft.vaka:vaka-android:0.4.0")
}

vaka {
    url.set("{{ENDPOINT}}")
    project.set("{{PROJECT}}")
    // The API token comes from the VAKA_TOKEN environment variable (or -Pvaka.token). Never commit it.
}
```

If the project uses a version catalog (`gradle/libs.versions.toml`), add the same coordinates there
instead and reference them with `alias(libs.plugins.vaka)` / `implementation(libs.vaka.android)`.

**Building from source instead** (if the artifacts cannot be resolved): clone
`https://github.com/ErikHellman/vaka` next to the app, then in the app's `settings.gradle.kts` add
`includeBuild("../vaka/sdks/android/vaka-gradle-plugin")` inside `pluginManagement { }` and
`includeBuild("../vaka/sdks/android")` at the top level, and drop the `version "0.4.0"` from the
plugin line. Gradle substitutes both automatically.

## 2. Initialise in the Application class

If the app has no `Application` subclass, create one and register it with `android:name` on the
`<application>` element in `AndroidManifest.xml`. Then:

```kotlin
import se.hellsoft.vaka.Vaka
import se.hellsoft.vaka.VakaConfig

class App : Application() {
    override fun onCreate() {
        super.onCreate()
        Vaka.init(this, VakaConfig(
            key = "{{KEY}}",
            endpoint = "{{ENDPOINT}}",
            release = BuildConfig.VERSION_NAME,   // must match the release the mapping is uploaded for
            environment = if (BuildConfig.DEBUG) "debug" else "production",
        ))
    }
}
```

Enable `buildFeatures { buildConfig = true }` in the app module if `BuildConfig` is not generated.
The ingest key is write-only and safe to ship in the app. The SDK adds the `INTERNET` and
`ACCESS_NETWORK_STATE` permissions through manifest merging. If `{{ENDPOINT}}` is plain `http://`
(local testing), the app also needs `android:usesCleartextTraffic="true"` or a network security config.

## 3. What you get automatically

- Uncaught exceptions: written to disk synchronously when the app crashes and sent on the next
  launch, with the last 100 breadcrumbs.
- Native (C/C++) crashes: fatal signals are recorded with raw addresses and loaded libraries and
  sent on the next launch (see step 5 for symbols).
- ANRs: the main thread blocked for 5 seconds is reported with its stack (release builds only, by
  default; set `captureAnrs = true` to test in a debug build, `anrTimeoutMillis` to change the limit).
- Sessions for crash-free users and sessions per release: a session starts when the app comes to the
  foreground and ends after 30 seconds in the background.
- A `screen_view` for every resumed Activity (named after the Activity class).
- Breadcrumbs for Activity lifecycle and connectivity changes.
- An offline queue on disk, sent in gzip batches every 10 seconds or 50 items, with backoff.

## 4. Optional calls

```kotlin
Vaka.screen("Checkout")                          // Compose/Fragment screens (Activities are automatic)
Vaka.track("purchase", mapOf("plan" to "pro"))   // custom event (flat string/number/boolean props)
Vaka.captureException(e)                          // handled exception
Vaka.captureMessage("Payment provider slow", VakaLevel.WARNING)
Vaka.addBreadcrumb("cart", "Added item 42")
Vaka.setTag("flavor", BuildConfig.FLAVOR)
Vaka.identify("user-123")                         // opt-in only; Vaka.identify(null) clears it
```

For Jetpack Compose navigation, call `Vaka.screen(route)` from a `NavController.OnDestinationChangedListener`
and consider `autoScreenViews = false` if the app has a single Activity. Do not call `identify` with
emails or names unless the app's privacy policy allows it.

## 5. R8 mapping upload (release builds)

With `isMinifyEnabled = true`, the plugin adds `uploadVakaMapping<Variant>` and runs it after
`minify<Variant>WithR8`. It needs an API token with write access (create one with the MCP tool
`create_token`, the admin UI, or `POST /api/v1/tokens`):

```sh
export VAKA_TOKEN=vt_...
./gradlew :app:assembleRelease   # or bundleRelease; look for "Vaka: uploaded mapping.txt"
```

On CI, store `VAKA_TOKEN` as a secret. Without a token the build still succeeds and logs a warning
(set `failOnError.set(true)` in the `vaka { }` block to fail instead). If `release` in `VakaConfig` is
not `versionName`, set the same value with `release.set(...)` in the `vaka { }` block. Alternative
without the plugin: `vaka upload proguard --release <versionName> app/build/outputs/mapping/release/mapping.txt`.

If the app has its own native code (NDK/CMake), also upload the unstripped libraries after each
release build so native crashes get function names and lines (they are matched by build id):

```sh
vaka upload debug-files --release <versionName> app/build/intermediates/merged_native_libs/release/
```

## 6. Verify

1. Build and run the app (`./gradlew :app:installDebug` on a device or emulator). On an emulator,
   `localhost` on the host machine is `10.0.2.2`.
2. Trigger a test error, e.g. temporarily add `Vaka.captureMessage("vaka install test", VakaLevel.ERROR)`
   after `Vaka.init`, open the app, then send it to the background (or wait 10 seconds).
3. Check with the API: `curl -H "Authorization: Bearer $VAKA_TOKEN" {{ENDPOINT}}/api/v1/projects/{{PROJECT}}/issues`,
   or the MCP tool `list_issues`. Sessions show up in `GET /api/v1/projects/{{PROJECT}}/releases/health`
   and screen views in `GET /api/v1/projects/{{PROJECT}}/analytics/summary`.
4. Remove the test call.

If nothing arrives: set `debug = true` in `VakaConfig` and filter Logcat by the tag `Vaka`
(expect `sent batch: HTTP 202`). An `ingest key rejected` line means the key is wrong or revoked.
