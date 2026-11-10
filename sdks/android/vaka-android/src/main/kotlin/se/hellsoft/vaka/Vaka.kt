package se.hellsoft.vaka

import android.app.Activity
import android.app.Application
import android.content.Context
import android.os.Build
import android.os.Bundle
import android.util.Log
import se.hellsoft.vaka.internal.Client
import se.hellsoft.vaka.internal.HttpTransport
import se.hellsoft.vaka.internal.KeyValueStore
import se.hellsoft.vaka.internal.Logger
import java.io.File

/**
 * Vaka crash reporting and analytics.
 *
 * ```kotlin
 * class App : Application() {
 *     override fun onCreate() {
 *         super.onCreate()
 *         Vaka.init(this, VakaConfig(key = "vk_...", endpoint = "https://vaka.example.com", release = BuildConfig.VERSION_NAME))
 *     }
 * }
 * ```
 *
 * Every method is safe to call before [init] (it does nothing) and from any thread.
 */
public object Vaka {
    @Volatile private var client: Client? = null

    /** True once [init] has run with an enabled config. */
    @JvmStatic public val isInitialized: Boolean get() = client != null

    /** Initializes the SDK. Call once, from `Application.onCreate`. Later calls are ignored. */
    @JvmStatic
    @Synchronized
    public fun init(context: Context, config: VakaConfig) {
        if (client != null || !config.enabled) return
        val app = context.applicationContext as Application
        val log = if (config.debug) LogcatLogger else Logger.NONE
        if (config.key.isBlank() || config.endpoint.isBlank()) {
            Log.w(TAG, "Vaka.init: key and endpoint are required; the SDK stays disabled")
            return
        }
        val cfg = if (config.inAppPackages.isEmpty()) config.copy(inAppPackages = listOf(app.packageName)) else config
        val dir = File(app.filesDir, "vaka")
        val c = Client(
            config = cfg,
            queueDir = File(dir, "queue"),
            store = PrefsStore(app),
            device = deviceInfo(cfg),
            contexts = contexts(app),
            transport = HttpTransport(cfg.endpoint, cfg.key, "${Client.SDK_NAME}/${BuildConfig.SDK_VERSION}", log),
            log = log,
            sdkVersion = BuildConfig.SDK_VERSION,
        )
        client = c
        if (cfg.captureUncaughtExceptions) CrashHandler.install(c)
        Lifecycle(c, cfg).register(app)
        log.debug("initialized for release ${cfg.release} (${cfg.environment})")
    }

    /** Records a `screen_view`. Activities are tracked automatically unless `autoScreenViews = false`. */
    @JvmStatic @JvmOverloads
    public fun screen(name: String, props: Map<String, Any?> = emptyMap()) {
        client?.screen(name, props)
    }

    /** Records a custom analytics event. Props: flat string/number/boolean values, at most 64. */
    @JvmStatic @JvmOverloads
    public fun track(name: String, props: Map<String, Any?> = emptyMap()) {
        client?.track(name, props)
    }

    /** Reports a handled exception. Returns the event id, or null before [init]. */
    @JvmStatic @JvmOverloads
    public fun captureException(t: Throwable, level: VakaLevel = VakaLevel.ERROR, tags: Map<String, String> = emptyMap()): String? =
        client?.captureException(t, level, tags)

    /** Reports a message without an exception. */
    @JvmStatic @JvmOverloads
    public fun captureMessage(message: String, level: VakaLevel = VakaLevel.INFO): String? = client?.captureMessage(message, level)

    /** Adds a breadcrumb; the last 100 are attached to every error. */
    @JvmStatic @JvmOverloads
    public fun addBreadcrumb(category: String, message: String, level: VakaLevel = VakaLevel.INFO, data: Map<String, Any?> = emptyMap()) {
        client?.addBreadcrumb(category, message, level, data)
    }

    /**
     * Opt-in user identification: attaches [userId] to later errors, events and sessions.
     * Pass null to clear it (e.g. on logout).
     */
    @JvmStatic
    public fun identify(userId: String?) {
        client?.identify(userId)
    }

    /** Sets a tag on later errors; null removes it. */
    @JvmStatic
    public fun setTag(key: String, value: String?) {
        client?.setTag(key, value)
    }

    /** Sends queued items now instead of waiting for the next interval. */
    @JvmStatic
    public fun flush() {
        client?.flush()
    }

    internal const val TAG = "Vaka"

    private fun deviceInfo(cfg: VakaConfig): Map<String, Any?> = linkedMapOf(
        "model" to Build.MODEL,
        "manufacturer" to Build.MANUFACTURER,
        "os_name" to "Android",
        "os_version" to Build.VERSION.RELEASE,
        "app_version" to cfg.release,
    )

    private fun contexts(app: Application): Map<String, Any?> {
        @Suppress("DEPRECATION")
        val build = try {
            val info = app.packageManager.getPackageInfo(app.packageName, 0)
            if (Build.VERSION.SDK_INT >= 28) info.longVersionCode.toString() else info.versionCode.toString()
        } catch (e: Exception) {
            null
        }
        return linkedMapOf(
            "app" to linkedMapOf("id" to app.packageName, "build" to build),
            "runtime" to linkedMapOf("name" to "Android", "version" to Build.VERSION.RELEASE, "api_level" to Build.VERSION.SDK_INT),
            "device" to linkedMapOf("brand" to Build.BRAND, "abi" to Build.SUPPORTED_ABIS.firstOrNull()),
        )
    }
}

private object LogcatLogger : Logger {
    override fun debug(msg: String) {
        Log.d(Vaka.TAG, msg)
    }

    override fun warn(msg: String, t: Throwable?) {
        Log.w(Vaka.TAG, msg, t)
    }
}

private class PrefsStore(context: Context) : KeyValueStore {
    private val prefs = context.getSharedPreferences("se.hellsoft.vaka", Context.MODE_PRIVATE)

    override fun getString(key: String): String? = prefs.getString(key, null)

    override fun getLong(key: String): Long = try {
        prefs.getLong(key, 0L)
    } catch (e: ClassCastException) {
        0L
    }

    override fun put(values: Map<String, Any?>, commit: Boolean) {
        val e = prefs.edit()
        for ((k, v) in values) {
            when (v) {
                null -> e.remove(k)
                is String -> e.putString(k, v)
                is Long -> e.putLong(k, v)
                is Int -> e.putLong(k, v.toLong())
                is Boolean -> e.putBoolean(k, v)
                else -> e.putString(k, v.toString())
            }
        }
        if (commit) e.commit() else e.apply()
    }
}

/** Writes the crash to disk synchronously, then hands over to the previous handler. */
internal class CrashHandler private constructor(
    private val client: Client,
    private val previous: Thread.UncaughtExceptionHandler?,
) : Thread.UncaughtExceptionHandler {
    override fun uncaughtException(t: Thread, e: Throwable) {
        client.captureCrash(e)
        previous?.uncaughtException(t, e)
    }

    companion object {
        fun install(client: Client) {
            val prev = Thread.getDefaultUncaughtExceptionHandler()
            if (prev is CrashHandler) return
            Thread.setDefaultUncaughtExceptionHandler(CrashHandler(client, prev))
        }
    }
}

/**
 * Foreground/background detection from started Activities (the same signal ProcessLifecycleOwner
 * uses, without the androidx dependency), automatic screen views and breadcrumbs.
 */
private class Lifecycle(private val client: Client, private val cfg: VakaConfig) : Application.ActivityLifecycleCallbacks {
    private var started = 0
    private var changingConfig = false

    fun register(app: Application) {
        app.registerActivityLifecycleCallbacks(this)
        if (cfg.autoBreadcrumbs) Connectivity.register(app, client)
    }

    private fun crumb(a: Activity, what: String) {
        if (cfg.autoBreadcrumbs) client.addBreadcrumb("lifecycle", "${a.javaClass.simpleName} $what")
    }

    override fun onActivityCreated(a: Activity, b: Bundle?) = crumb(a, "created")

    override fun onActivityStarted(a: Activity) {
        if (started++ == 0 && !changingConfig) client.onForeground()
        changingConfig = false
        crumb(a, "started")
    }

    override fun onActivityResumed(a: Activity) {
        crumb(a, "resumed")
        if (cfg.autoScreenViews) client.screen(a.javaClass.simpleName)
    }

    override fun onActivityPaused(a: Activity) = crumb(a, "paused")

    override fun onActivityStopped(a: Activity) {
        crumb(a, "stopped")
        changingConfig = a.isChangingConfigurations
        if (--started == 0 && !changingConfig) client.onBackground()
    }

    override fun onActivitySaveInstanceState(a: Activity, b: Bundle) {}

    override fun onActivityDestroyed(a: Activity) = crumb(a, "destroyed")
}
