package se.hellsoft.vaka.internal

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class AnrWatchdogTest {
    private val executor = Executors.newSingleThreadExecutor { r -> Thread(r, "fake-main") }
    private var mainThread: Thread? = null
    private val main = object : MainThread {
        override fun post(r: Runnable) {
            executor.execute(r)
        }

        override fun stackTrace(): Array<StackTraceElement> = mainThread!!.stackTrace
    }
    private val anrs = CopyOnWriteArrayList<Array<StackTraceElement>>()
    private var recovered = 0
    private var watchdog: AnrWatchdog? = null

    init {
        executor.submit { mainThread = Thread.currentThread() }.get()
    }

    private fun start(debugging: Boolean = false) = AnrWatchdog(100, main, { anrs.add(it) }, { recovered++ }, { debugging })
        .also { watchdog = it; it.start() }

    @After fun tearDown() {
        watchdog?.stop()
        executor.shutdownNow()
        executor.awaitTermination(1, TimeUnit.SECONDS)
    }

    @Suppress("SameParameterValue")
    private fun blockMain(millis: Long) = executor.execute { busyWait(millis) }

    private fun busyWait(millis: Long) {
        val end = System.nanoTime() + millis * 1_000_000
        while (System.nanoTime() < end) Thread.sleep(5)
    }

    @Test fun responsiveMainThreadIsNotReported() {
        start()
        Thread.sleep(500)
        assertEquals(0, anrs.size)
    }

    @Test fun blockedMainThreadIsReportedOnceWithItsStack() {
        start()
        blockMain(700)
        Thread.sleep(1_100)
        assertEquals(1, anrs.size)
        assertTrue(anrs[0].any { it.methodName == "busyWait" })
        assertEquals(1, recovered)
        // Re-armed: a second hang is reported again.
        blockMain(700)
        Thread.sleep(1_100)
        assertEquals(2, anrs.size)
    }

    @Test fun nothingIsReportedWhileDebugging() {
        start(debugging = true)
        blockMain(700)
        Thread.sleep(1_000)
        assertEquals(0, anrs.size)
    }
}
