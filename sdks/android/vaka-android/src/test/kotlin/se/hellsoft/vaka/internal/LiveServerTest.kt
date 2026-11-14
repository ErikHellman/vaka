package se.hellsoft.vaka.internal

import org.junit.Assume.assumeTrue
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import se.hellsoft.vaka.VakaConfig
import java.util.concurrent.Executors

/**
 * Sends real data to a running server. Skipped unless VAKA_TEST_ENDPOINT and VAKA_TEST_KEY are set:
 *
 *     VAKA_TEST_ENDPOINT=http://localhost:8080 VAKA_TEST_KEY=vk_... ./gradlew :vaka-android:testDebugUnitTest
 */
class LiveServerTest {
    @get:Rule val tmp = TemporaryFolder()

    @Test fun sendsToServer() {
        val endpoint = System.getenv("VAKA_TEST_ENDPOINT")
        val key = System.getenv("VAKA_TEST_KEY")
        assumeTrue(endpoint != null && key != null)
        val results = ArrayList<SendResult>()
        val http = HttpTransport(endpoint!!, key!!, "vaka-android/test", object : Logger {
            override fun debug(msg: String) = println("vaka: $msg")
            override fun warn(msg: String, t: Throwable?) = println("vaka WARN: $msg $t")
        })
        val transport = object : Transport {
            override fun send(body: String) = http.send(body).also { synchronized(results) { results.add(it) } }
        }
        val release = System.getenv("VAKA_TEST_RELEASE") ?: "0.1.0-test"
        val cfg = VakaConfig(key = key, endpoint = endpoint, release = release, environment = "test",
            inAppPackages = listOf("se.hellsoft.vaka.internal"), flushIntervalMillis = 60_000, sessionTimeoutMillis = 100)
        val store = MemoryStore()
        val device = mapOf("model" to "JVM test", "os_name" to "Android", "os_version" to "15", "app_version" to release)
        val c = Client(cfg, tmp.root.resolve("q"), store, device, mapOf("runtime" to mapOf("name" to "Android", "version" to "15")),
            transport, executor = Executors.newSingleThreadScheduledExecutor(), sdkVersion = "0.1.0")
        c.onForeground()
        c.screen("Checkout")
        c.track("purchase", mapOf("plan" to "pro", "amount" to 9.99))
        c.addBreadcrumb("cart", "Added item 42")
        c.identify("user-123")
        try {
            checkout(emptyList())
        } catch (e: Exception) {
            c.captureException(e)
        }
        c.flush(); c.awaitIdle()
        // Simulate a crash and a relaunch: crash + crashed session go out from the next client.
        c.captureCrash(IllegalArgumentException("fatal crash from LiveServerTest", NullPointerException("inner")))
        val c2 = Client(cfg, tmp.root.resolve("q"), store, device, emptyMap(), transport,
            executor = Executors.newSingleThreadScheduledExecutor(), sdkVersion = "0.1.0")
        Thread.sleep(1000); c2.awaitIdle()
        assertEquals(0, c2.queueSize())
        synchronized(results) { results.forEach { assertEquals(SendResult.Done, it) } }
        println("vaka: sent ${results.size} batches, anon_id=${c.anonId}")
    }

    @Test fun sendsFeedbackToServer() {
        val endpoint = System.getenv("VAKA_TEST_ENDPOINT")
        val key = System.getenv("VAKA_TEST_KEY")
        assumeTrue(endpoint != null && key != null)
        val http = HttpTransport(endpoint!!, key!!, "vaka-android/test", object : Logger {
            override fun debug(msg: String) = println("vaka: $msg")
            override fun warn(msg: String, t: Throwable?) = println("vaka WARN: $msg $t")
        })
        val cfg = VakaConfig(key = key, endpoint = endpoint, release = "0.1.0-test", environment = "test", flushIntervalMillis = 60_000)
        val c = Client(cfg, tmp.root.resolve("q"), MemoryStore(), mapOf("model" to "JVM test", "os_name" to "Android"), emptyMap(),
            http, executor = Executors.newSingleThreadScheduledExecutor(), feedbackDir = tmp.root.resolve("fb"))
        c.onForeground()
        c.screen("Reports")
        val eventId = c.captureException(IllegalStateException("export failed in LiveServerTest"))
        c.flush(); c.awaitIdle()
        // 1x1 PNG.
        val png = java.util.Base64.getDecoder().decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        val done = java.util.concurrent.CountDownLatch(1)
        var result: se.hellsoft.vaka.VakaFeedbackResult? = null
        c.captureFeedback(se.hellsoft.vaka.VakaFeedback("The export button does nothing (LiveServerTest)", email = "ann@example.com",
            eventId = eventId, source = se.hellsoft.vaka.VakaFeedbackSource.WIDGET, attachments = listOf(png))) { result = it; done.countDown() }
        done.await(30, java.util.concurrent.TimeUnit.SECONDS)
        println("vaka: feedback result $result")
        assertEquals(se.hellsoft.vaka.VakaFeedbackResult.Status.SENT, result?.status)
    }

    private fun checkout(items: List<String>) {
        check(items.isNotEmpty()) { "Cart is empty" }
    }
}
