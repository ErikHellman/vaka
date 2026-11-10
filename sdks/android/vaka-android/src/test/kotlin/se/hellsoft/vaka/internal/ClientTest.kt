package se.hellsoft.vaka.internal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import se.hellsoft.vaka.VakaConfig
import java.util.concurrent.Executors

class ClientTest {
    @get:Rule val tmp = TemporaryFolder()

    private class FakeTransport : Transport {
        val bodies = ArrayList<String>()
        var next: SendResult = SendResult.Done
        override fun send(body: String): SendResult = synchronized(this) { bodies.add(body); next }
        fun all() = synchronized(this) { bodies.joinToString("\n") }
    }

    private class FakeClock(var t: Long = 1_000_000) : Clock {
        override fun now() = t
    }

    private val config = VakaConfig(key = "vk_test", endpoint = "http://localhost", release = "1.0.0", flushIntervalMillis = 60_000, sessionTimeoutMillis = 50)

    private fun client(store: KeyValueStore = MemoryStore(), transport: Transport = FakeTransport(), clock: Clock = FakeClock(), cfg: VakaConfig = config) =
        Client(cfg, tmp.root.resolve("queue"), store, mapOf("model" to "Test"), emptyMap(), transport, clock = clock,
            executor = Executors.newSingleThreadScheduledExecutor(), sdkVersion = "0.1.0")

    @Test fun sendsBatchWithSdkAndItems() {
        val t = FakeTransport()
        val c = client(transport = t)
        c.track("purchase", mapOf("plan" to "pro"))
        c.captureException(IllegalStateException("Cart is empty"))
        c.flush(); c.awaitIdle()
        val body = t.all()
        assertTrue(body, body.contains("\"sdk\":{\"name\":\"vaka-android\",\"version\":\"0.1.0\"}"))
        assertTrue(body.contains("\"name\":\"purchase\""))
        assertTrue(body.contains("\"props\":{\"plan\":\"pro\"}"))
        assertTrue(body.contains("\"type\":\"java.lang.IllegalStateException\""))
        assertTrue(body.contains("\"handled\":true"))
        assertEquals(0, c.queueSize())
    }

    @Test fun keepsItemsOnRetryAndBacksOff() {
        val t = FakeTransport().apply { next = SendResult.Retry(null) }
        val c = client(transport = t)
        c.track("a")
        c.flush(); c.awaitIdle()
        assertEquals(1, c.queueSize())
        t.next = SendResult.Done
        c.flush(); c.awaitIdle()
        assertEquals(0, c.queueSize())
    }

    @Test fun splitsOn413() {
        val t = object : Transport {
            var calls = 0
            override fun send(body: String): SendResult {
                calls++
                return if (body.split("\"type\":\"event\"").size - 1 > 1) SendResult.TooLarge else SendResult.Done
            }
        }
        val c = client(transport = t)
        repeat(4) { c.track("e$it") }
        c.flush(); c.awaitIdle()
        assertEquals(0, c.queueSize())
    }

    @Test fun stopsOn401() {
        val t = FakeTransport().apply { next = SendResult.Unauthorized }
        val c = client(transport = t)
        c.track("a"); c.flush(); c.awaitIdle()
        c.track("b"); c.flush(); c.awaitIdle()
        assertEquals(1, t.bodies.size)
        assertEquals(2, c.queueSize())
    }

    @Test fun sessionLifecycle() {
        val t = FakeTransport()
        val clock = FakeClock()
        val c = client(transport = t, clock = clock)
        c.onForeground()
        val sid = c.currentSessionId!!
        c.captureException(RuntimeException("x"))
        clock.t += 5_000
        c.onBackground()
        Thread.sleep(300) // session timeout is 50 ms in this config
        c.awaitIdle(); c.flush(); c.awaitIdle()
        val body = t.all()
        assertTrue(body.contains("\"session_id\":\"$sid\",\"status\":\"ok\""))
        assertTrue(body, body.contains("\"session_id\":\"$sid\",\"status\":\"errored\""))
        assertTrue(body.contains("\"duration_ms\":5000"))
        assertEquals(null, c.currentSessionId)
    }

    @Test fun crashIsWrittenSynchronouslyAndSentOnNextLaunch() {
        val store = MemoryStore()
        val dead = FakeTransport().apply { next = SendResult.Retry(null) }
        val first = client(store = store, transport = dead)
        first.onForeground()
        first.awaitIdle()
        val sid = first.currentSessionId
        first.captureCrash(RuntimeException("boom"))
        // Next launch, network up.
        val t = FakeTransport()
        val second = client(store = store, transport = t)
        Thread.sleep(800); second.awaitIdle()
        val body = t.all()
        assertTrue(body, body.contains("\"fatal\":true"))
        assertTrue(body.contains("\"session_id\":\"$sid\",\"status\":\"crashed\""))
        assertTrue(!body.contains("\"abnormal\""))
    }

    @Test fun unfinishedSessionIsAbnormalOnNextLaunch() {
        val store = MemoryStore()
        val first = client(store = store, transport = FakeTransport().apply { next = SendResult.Retry(null) })
        first.onForeground(); first.awaitIdle()
        val sid = first.currentSessionId
        val t = FakeTransport()
        client(store = store, transport = t).apply { Thread.sleep(800); awaitIdle() }
        assertTrue(t.all().contains("\"session_id\":\"$sid\",\"status\":\"abnormal\""))
    }

    @Test fun identifyIsPersistedAndAttached() {
        val store = MemoryStore()
        val t = FakeTransport()
        val c = client(store = store, transport = t)
        c.identify("user-123")
        c.captureMessage("hello")
        c.flush(); c.awaitIdle()
        assertTrue(t.all().contains("\"user\":{\"id\":\"user-123\"}"))
        assertEquals("user-123", client(store = store).userId)
        assertEquals(store.getString(Client.K_ANON), c.anonId)
    }

    @Test fun breadcrumbsCappedAt100() {
        val t = FakeTransport()
        val c = client(transport = t)
        repeat(150) { c.addBreadcrumb("test", "crumb $it") }
        c.captureMessage("m")
        c.flush(); c.awaitIdle()
        val body = t.all()
        assertTrue(!body.contains("\"crumb 49\""))
        assertTrue(body.contains("\"crumb 50\""))
        assertTrue(body.contains("\"crumb 149\""))
    }
}
