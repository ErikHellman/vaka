package se.hellsoft.vaka.gradle

import org.gradle.api.provider.Property

/**
 * Configuration for mapping upload:
 *
 * ```kotlin
 * vaka {
 *     url.set("https://vaka.example.com")   // or VAKA_URL / -Pvaka.url
 *     project.set("my-app")                 // or VAKA_PROJECT / -Pvaka.project
 *     // token: VAKA_TOKEN env var or -Pvaka.token (never commit it)
 * }
 * ```
 */
abstract class VakaExtension {
    /** Base URL of the Vaka server, e.g. `https://vaka.example.com`. */
    abstract val url: Property<String>

    /** Project slug or id. */
    abstract val project: Property<String>

    /** API token with write scope (`vt_...`). Prefer the `VAKA_TOKEN` environment variable. */
    abstract val token: Property<String>

    /**
     * Release to upload the mapping for. Defaults to the variant's `versionName`, which is what
     * `VakaConfig(release = BuildConfig.VERSION_NAME)` sends. Must match the SDK's `release`.
     */
    abstract val release: Property<String>

    /** Set to false to skip uploads (for example on local builds). Default: true. */
    abstract val uploadMapping: Property<Boolean>

    /** Fail the build when the upload fails. Default: false (a warning is logged instead). */
    abstract val failOnError: Property<Boolean>
}
