package se.hellsoft.vaka.internal

import se.hellsoft.vaka.VakaConfig
import se.hellsoft.vaka.VakaLevel
import java.io.File
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.Callable
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Platform-independent core: scope (user, tags, breadcrumbs), sessions, offline queue and sending.
 * All mutable state is touched under `this` lock; network work runs on [executor].
 */
internal class Client(
    val config: VakaConfig,
    queueDir: File,
    private val store: KeyValueStore,
    /** `device` object for every item (model, os_name, os_version, ...). */
    private val device: Map<String, Any?>,
    /** Extra `contexts` for errors. */
    private val contexts: Map<String, Any?>,
    private val transport: Transport,
    private val log: Logger = Logger.NONE,
    private val clock: Clock = Clock.SYSTEM,
    private val executor: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "vaka").apply { isDaemon = true }
    },
    sdkVersion: String = "0.0.0",
    /** Directory the native signal handler writes reports to; read once at startup. */
    private val nativeCrashDir: File? = null,
    /** Told about every session id change (the native handler records it in its reports). */
    private val sessionObserver: (String?) -> Unit = {},
) {
    private val sdk = mapOf("name" to SDK_NAME, "version" to sdkVersion)
    private val queue = DiskQueue(queueDir, config.maxQueueSize, log)
    private val errors = ErrorBuilder(config.inAppPackages)
    private val breadcrumbs = ArrayDeque<Map<String, Any?>>()
    private val tags = LinkedHashMap(config.tags)

    val anonId: String = store.getString(K_ANON) ?: UUID.randomUUID().toString().also { store.put(mapOf(K_ANON to it)) }
    @Volatile var userId: String? = store.getString(K_USER)
        private set

    // Session state (also persisted so the next launch can close it).
    private var sessionId: String? = null
    private var sessionStarted = 0L
    private var sessionErrors = 0
    private var backgroundedAt = 0L
    private var endTimer: ScheduledFuture<*>? = null
    /** An ANR was reported and the main thread has not recovered yet. */
    private var anrActive = false

    @Volatile private var paused = false
    private var backoffMillis = 0L
    private var nextAttempt = 0L
    private var flushScheduled = false
    private var lastScreen: String? = null

    init {
        val crashedSessions = reportNativeCrashes()
        closePreviousSession(crashedSessions)
        executor.scheduleWithFixedDelay({ tick() }, config.flushIntervalMillis, config.flushIntervalMillis, TimeUnit.MILLISECONDS)
        // Crashes and the previous session's end from the last run go out right away.
        flushSoon()
    }

    // ---------------------------------------------------------------- public operations

    fun identify(id: String?) {
        synchronized(this) { userId = id?.takeIf { it.isNotBlank() } }
        store.put(mapOf(K_USER to userId))
    }

    fun setTag(key: String, value: String?) = synchronized(this) {
        if (value == null) tags.remove(key) else tags[key] = value
    }

    fun addBreadcrumb(category: String, message: String, level: VakaLevel = VakaLevel.INFO, data: Map<String, Any?> = emptyMap()) =
        synchronized(this) {
            breadcrumbs.addLast(linkedMapOf("timestamp" to clock.now(), "category" to category, "message" to message, "level" to level.wire, "data" to data))
            while (breadcrumbs.size > MAX_BREADCRUMBS) breadcrumbs.removeFirst()
        }

    fun screen(name: String, props: Map<String, Any?> = emptyMap()) {
        synchronized(this) {
            if (name == lastScreen && props.isEmpty()) return
            lastScreen = name
        }
        addBreadcrumb("navigation", name)
        enqueue(event("screen_view", props).also { it["screen"] = name })
    }

    fun track(name: String, props: Map<String, Any?> = emptyMap()) {
        enqueue(event(name, props))
    }

    fun captureException(t: Throwable, level: VakaLevel = VakaLevel.ERROR, extraTags: Map<String, String> = emptyMap()): String {
        val item = error(level, fatal = false, handled = true, extraTags = extraTags).also { it["exceptions"] = errors.exceptions(t) }
        countSessionError()
        enqueue(item)
        addBreadcrumb("error", "${t.javaClass.name}: ${t.message ?: ""}", level)
        return item["event_id"] as String
    }

    fun captureMessage(message: String, level: VakaLevel = VakaLevel.INFO): String {
        val item = error(level, fatal = false, handled = true).also { it["message"] = message }
        if (level == VakaLevel.ERROR || level == VakaLevel.FATAL) countSessionError()
        enqueue(item)
        return item["event_id"] as String
    }

    /**
     * Called from the uncaught exception handler: everything happens synchronously on the
     * crashing thread so it is on disk before the process dies.
     */
    fun captureCrash(t: Throwable) {
        try {
            val item = error(VakaLevel.FATAL, fatal = true, handled = false).also { it["exceptions"] = errors.exceptions(t) }
            queue.add(Json.write(item))
            val ended = synchronized(this) {
                val id = sessionId ?: return@synchronized null
                sessionErrors++
                sessionItem(id, "crashed", clock.now()).also { clearSession(commit = true) }
            }
            if (ended != null) queue.add(Json.write(ended))
        } catch (e: Throwable) {
            log.warn("failed to record crash", e)
        }
    }

    /**
     * Called by the ANR watchdog with the main thread's stack. Written to disk synchronously:
     * the system may kill the app at any moment while it is not responding.
     */
    fun captureAnr(stack: Array<StackTraceElement>, timeoutMillis: Long) {
        try {
            val message = "Application not responding for $timeoutMillis ms"
            val item = error(VakaLevel.ERROR, fatal = false, handled = false, extraTags = mapOf("mechanism" to "anr"))
            item["exceptions"] = listOf(errors.exceptionFromStack("ANR", message, stack))
            queue.add(Json.write(item))
            synchronized(this) {
                anrActive = true
                if (sessionId != null) sessionErrors++
                persistSession(commit = true)
            }
            addBreadcrumb("anr", message, VakaLevel.ERROR)
            flushSoon()
        } catch (e: Throwable) {
            log.warn("failed to record ANR", e)
        }
    }

    /** The main thread responds again after an ANR. */
    fun onAnrRecovered() = synchronized(this) {
        if (anrActive) {
            anrActive = false
            persistSession()
        }
    }

    fun flush() {
        executor.execute { drain(force = true) }
    }

    /** For tests: wait for queued work to finish. */
    fun awaitIdle(timeoutMillis: Long = 10_000) {
        executor.submit(Callable { }).get(timeoutMillis, TimeUnit.MILLISECONDS)
    }

    fun queueSize(): Int = queue.size()

    // ---------------------------------------------------------------- sessions

    fun onForeground() {
        if (!config.sessionTracking) return
        val toSend = ArrayList<Map<String, Any?>>(2)
        synchronized(this) {
            endTimer?.cancel(false)
            endTimer = null
            val now = clock.now()
            val current = sessionId
            if (current != null && backgroundedAt > 0 && now - backgroundedAt >= config.sessionTimeoutMillis) {
                toSend.add(sessionItem(current, endStatus(), backgroundedAt))
                clearSession()
            }
            if (sessionId == null) {
                sessionId = UUID.randomUUID().toString()
                sessionStarted = now
                sessionErrors = 0
                lastScreen = null
                toSend.add(sessionItem(sessionId!!, "ok", now))
                sessionObserver(sessionId)
            }
            backgroundedAt = 0
            persistSession()
        }
        toSend.forEach(::enqueue)
    }

    fun onBackground() {
        if (!config.sessionTracking) return
        synchronized(this) {
            val id = sessionId ?: return
            backgroundedAt = clock.now()
            persistSession()
            endTimer?.cancel(false)
            endTimer = executor.schedule(Runnable {
                val item = synchronized(this) {
                    if (sessionId != id || backgroundedAt == 0L) return@Runnable
                    sessionItem(id, endStatus(), backgroundedAt).also { clearSession() }
                }
                enqueue(item)
                executor.execute { drain(force = true) }
            }, config.sessionTimeoutMillis, TimeUnit.MILLISECONDS)
        }
        flush()
    }

    val currentSessionId: String? get() = synchronized(this) { sessionId }

    private fun endStatus() = if (sessionErrors > 0) "errored" else "exited"

    private fun countSessionError() = synchronized(this) {
        if (sessionId != null) {
            sessionErrors++
            persistSession()
        }
    }

    private fun sessionItem(id: String, status: String, end: Long): MutableMap<String, Any?> = linkedMapOf(
        "type" to "session",
        "session_id" to id,
        "status" to status,
        "started" to sessionStarted,
        "timestamp" to end,
        "duration_ms" to (end - sessionStarted).coerceAtLeast(0),
        "errors" to sessionErrors,
        "release" to config.release,
        "environment" to config.environment,
        "user_id" to userId,
        "anon_id" to anonId,
    )

    private fun persistSession(commit: Boolean = false) {
        store.put(
            mapOf(
                K_SID to sessionId, K_STARTED to sessionStarted, K_ERRORS to sessionErrors.toLong(), K_BG to backgroundedAt,
                K_SEEN to clock.now(), K_ANR to if (anrActive) 1L else null,
            ),
            commit,
        )
    }

    private fun clearSession(commit: Boolean = false) {
        if (sessionId != null) sessionObserver(null)
        sessionId = null
        backgroundedAt = 0
        anrActive = false
        store.put(mapOf(K_SID to null, K_STARTED to null, K_ERRORS to null, K_BG to null, K_SEEN to null, K_ANR to null), commit)
    }

    /**
     * A session left open by the previous process: it ended in the background, crashed in native
     * code (reported by [reportNativeCrashes]), was killed while not responding, or just died.
     */
    private fun closePreviousSession(nativeCrashes: Map<String, Long>) {
        val id = store.getString(K_SID) ?: return
        sessionStarted = store.getLong(K_STARTED)
        sessionErrors = store.getLong(K_ERRORS).toInt()
        val bg = store.getLong(K_BG)
        val crashedAt = nativeCrashes[id]
        val item = when {
            crashedAt != null -> {
                sessionErrors++
                sessionItem(id, "crashed", crashedAt)
            }
            store.getLong(K_ANR) != 0L -> sessionItem(id, "abnormal", store.getLong(K_SEEN).takeIf { it > 0 } ?: sessionStarted)
            bg > 0 -> sessionItem(id, endStatus(), bg)
            else -> sessionItem(id, "abnormal", store.getLong(K_SEEN).takeIf { it > 0 } ?: sessionStarted)
        }
        clearSession()
        enqueue(item)
    }

    /**
     * Turns native crash reports from the previous run into fatal error items. Returns the
     * crashed session ids with their crash times.
     */
    private fun reportNativeCrashes(): Map<String, Long> {
        val dir = nativeCrashDir ?: return emptyMap()
        val crashed = HashMap<String, Long>()
        for (r in NativeCrashReport.takeAll(dir, log)) {
            try {
                val item = error(VakaLevel.FATAL, fatal = true, handled = false, extraTags = mapOf("mechanism" to "signal"))
                if (r.timestampMillis > 0) item["timestamp"] = r.timestampMillis
                r.release?.let { item["release"] = it }
                item["session_id"] = r.sessionId
                item["breadcrumbs"] = emptyList<Any>()
                item["exceptions"] = listOf(r.exception())
                item["debug_images"] = r.debugImages()
                item["contexts"] = LinkedHashMap(contexts).apply {
                    put("thread", linkedMapOf("id" to r.tid, "name" to r.thread))
                }
                queue.add(Json.write(item))
                r.sessionId?.let { crashed[it] = if (r.timestampMillis > 0) r.timestampMillis else clock.now() }
            } catch (e: Exception) {
                log.warn("could not report native crash", e)
            }
        }
        return crashed
    }

    // ---------------------------------------------------------------- items

    private fun base(type: String): MutableMap<String, Any?> = linkedMapOf(
        "type" to type,
        "timestamp" to clock.now(),
        "release" to config.release,
        "environment" to config.environment,
        "anon_id" to anonId,
        "session_id" to currentSessionId,
        "device" to device,
    )

    private fun event(name: String, props: Map<String, Any?>): MutableMap<String, Any?> = base("event").also {
        it["name"] = name
        if (props.isNotEmpty()) it["props"] = cleanProps(props)
        userId?.let { u -> it["user_id"] = u }
    }

    private fun error(level: VakaLevel, fatal: Boolean, handled: Boolean, extraTags: Map<String, String> = emptyMap()): MutableMap<String, Any?> =
        base("error").also {
            it["event_id"] = UUID.randomUUID().toString()
            it["platform"] = "android"
            it["level"] = level.wire
            it["fatal"] = fatal
            it["handled"] = handled
            synchronized(this) {
                it["breadcrumbs"] = ArrayList(breadcrumbs)
                it["tags"] = LinkedHashMap(tags).apply { putAll(extraTags) }
            }
            it["contexts"] = contexts
            userId?.let { u -> it["user"] = mapOf("id" to u) }
        }

    private fun cleanProps(props: Map<String, Any?>): Map<String, Any?> =
        props.entries.take(MAX_PROPS).associate { (k, v) ->
            k to when (v) {
                null, is String, is Boolean, is Number -> v
                else -> v.toString()
            }
        }

    // ---------------------------------------------------------------- sending

    private fun enqueue(item: Map<String, Any?>) {
        if (!config.enabled) return
        val json = Json.write(item)
        executor.execute {
            queue.add(json)
            if (queue.size() >= config.maxBatchSize) drain(force = false)
        }
    }

    private fun flushSoon() {
        synchronized(this) {
            if (flushScheduled) return
            flushScheduled = true
        }
        executor.schedule(Runnable {
            synchronized(this) { flushScheduled = false }
            drain(force = true)
        }, 500, TimeUnit.MILLISECONDS)
    }

    private fun tick() {
        if (sessionId != null && backgroundedAt == 0L) synchronized(this) { persistSession() }
        drain(force = false)
    }

    /** Runs on [executor]. Sends queued items in batches until the queue is empty or sending fails. */
    private fun drain(force: Boolean) {
        if (paused || !config.enabled) return
        if (!force && clock.now() < nextAttempt) return
        var batchSize = config.maxBatchSize.coerceAtMost(MAX_ITEMS_PER_REQUEST)
        while (true) {
            val items = queue.peek(batchSize)
            if (items.isEmpty()) return
            val body = StringBuilder("{\"sdk\":").append(Json.write(sdk)).append(",\"items\":[")
            items.forEachIndexed { i, (_, json) -> if (i > 0) body.append(','); body.append(json) }
            body.append("]}")
            when (val r = transport.send(body.toString())) {
                SendResult.Done -> {
                    queue.remove(items.map { it.first })
                    backoffMillis = 0
                    nextAttempt = 0
                }
                SendResult.TooLarge -> {
                    if (batchSize == 1) {
                        log.warn("item too large for the server, dropping it")
                        queue.remove(items.map { it.first })
                    } else {
                        batchSize = (batchSize / 2).coerceAtLeast(1)
                    }
                }
                SendResult.Unauthorized -> {
                    paused = true
                    return
                }
                is SendResult.Retry -> {
                    backoffMillis = (if (backoffMillis == 0L) 1_000 else backoffMillis * 2).coerceAtMost(MAX_BACKOFF)
                    nextAttempt = clock.now() + (r.retryAfterMillis ?: backoffMillis)
                    log.debug("will retry in ${nextAttempt - clock.now()} ms")
                    return
                }
            }
        }
    }

    companion object {
        const val SDK_NAME = "vaka-android"
        const val MAX_BREADCRUMBS = 100
        const val MAX_PROPS = 64
        const val MAX_ITEMS_PER_REQUEST = 1000
        const val MAX_BACKOFF = 5 * 60_000L
        const val K_ANON = "anon_id"
        const val K_USER = "user_id"
        const val K_SID = "session.id"
        const val K_STARTED = "session.started"
        const val K_ERRORS = "session.errors"
        const val K_BG = "session.backgrounded"
        const val K_SEEN = "session.seen"
        const val K_ANR = "session.anr"
    }
}
