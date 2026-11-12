package se.hellsoft.vaka.internal

import java.util.concurrent.atomic.AtomicBoolean

/** The main thread as the watchdog sees it; an interface so the logic runs on the JVM in tests. */
internal interface MainThread {
    /** Runs [r] on the main thread, after whatever is queued already. */
    fun post(r: Runnable)

    /** The main thread's current stack, innermost frame first. */
    fun stackTrace(): Array<StackTraceElement>
}

/**
 * Detects a blocked main thread: posts a tick to it every [timeoutMillis] and reports when the
 * tick has not run by the next check. One report per hang; it re-arms once the main thread
 * responds again. Nothing is reported while [isDebugging] (a debugger pauses the main thread).
 */
internal class AnrWatchdog(
    private val timeoutMillis: Long,
    private val main: MainThread,
    private val onAnr: (stack: Array<StackTraceElement>) -> Unit,
    private val onRecovered: () -> Unit = {},
    private val isDebugging: () -> Boolean = { false },
) {
    @Volatile private var running = false
    private var thread: Thread? = null

    @Synchronized
    fun start() {
        if (running) return
        running = true
        thread = Thread({ loop() }, "vaka-anr").apply {
            isDaemon = true
            start()
        }
    }

    @Synchronized
    fun stop() {
        running = false
        thread?.interrupt()
        thread = null
    }

    private fun loop() {
        val poll = (timeoutMillis / 10).coerceIn(10, 500)
        try {
            while (running) {
                val ticked = AtomicBoolean(false)
                main.post(Runnable { ticked.set(true) })
                Thread.sleep(timeoutMillis)
                if (ticked.get() || !running) continue
                if (!isDebugging()) {
                    onAnr(main.stackTrace())
                    while (running && !ticked.get()) Thread.sleep(poll)
                    if (running) onRecovered()
                } else {
                    while (running && !ticked.get()) Thread.sleep(poll)
                }
            }
        } catch (e: InterruptedException) {
            // stop()
        } catch (e: Throwable) {
            // Never take the app down from the watchdog.
        }
    }
}
