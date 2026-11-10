package se.hellsoft.vaka

/**
 * Configuration passed to [Vaka.init].
 *
 * @property key project ingest key (`vk_...`). Write-only, safe to ship in the app.
 * @property endpoint base URL of the Vaka server, e.g. `https://vaka.example.com`.
 * @property release app version as users see it; must match the release used for mapping
 *   uploads. Use `BuildConfig.VERSION_NAME` (the Gradle plugin uploads with `versionName`).
 * @property environment e.g. `production`, `staging`, `debug`.
 * @property inAppPackages package prefixes of your own code, used to mark stack frames as
 *   in-app. Defaults to the application's package name. Obfuscated classes count as in-app.
 * @property autoScreenViews send a `screen_view` for every resumed Activity.
 * @property autoBreadcrumbs record Activity lifecycle and connectivity breadcrumbs.
 * @property captureUncaughtExceptions report crashes from uncaught JVM exceptions.
 * @property sessionTracking report sessions for crash-free users/sessions (release health).
 * @property flushIntervalMillis how often queued items are sent.
 * @property maxBatchSize send as soon as this many items are queued.
 * @property maxQueueSize oldest items are dropped beyond this many queued on disk.
 * @property sessionTimeoutMillis background time after which a session ends.
 * @property tags tags added to every error.
 * @property enabled set to false to turn the SDK into a no-op (e.g. for tests).
 * @property debug log SDK activity to Logcat (tag `Vaka`).
 */
public data class VakaConfig(
    val key: String,
    val endpoint: String,
    val release: String,
    val environment: String = "production",
    val inAppPackages: List<String> = emptyList(),
    val autoScreenViews: Boolean = true,
    val autoBreadcrumbs: Boolean = true,
    val captureUncaughtExceptions: Boolean = true,
    val sessionTracking: Boolean = true,
    val flushIntervalMillis: Long = 10_000,
    val maxBatchSize: Int = 50,
    val maxQueueSize: Int = 1000,
    val sessionTimeoutMillis: Long = 30_000,
    val tags: Map<String, String> = emptyMap(),
    val enabled: Boolean = true,
    val debug: Boolean = false,
)

/** Breadcrumb / error severity. */
public enum class VakaLevel(internal val wire: String) {
    FATAL("fatal"), ERROR("error"), WARNING("warning"), INFO("info"),
}
