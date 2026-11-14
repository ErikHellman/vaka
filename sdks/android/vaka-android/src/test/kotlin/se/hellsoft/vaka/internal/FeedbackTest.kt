package se.hellsoft.vaka.internal

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import se.hellsoft.vaka.VakaConfig
import se.hellsoft.vaka.VakaFeedback
import se.hellsoft.vaka.VakaFeedbackResult
import se.hellsoft.vaka.VakaFeedbackSource
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class FeedbackTest {
    @get:Rule val tmp = TemporaryFolder()

    private class FakeTransport : Transport {
        val feedback = ArrayList<Pair<String, List<Attachment>>>()
        var next: FeedbackSendResult = FeedbackSendResult.Accepted("fb-1", "AND-F1")
        override fun send(body: String): SendResult = SendResult.Done
        override fun sendFeedback(json: String, attachments: List<Attachment>): FeedbackSendResult =
            synchronized(this) { feedback.add(json to attachments); next }
    }

    private val config = VakaConfig(key = "vk_test", endpoint = "http://localhost", release = "1.0.0", flushIntervalMillis = 60_000)
    private val jpeg = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xE0.toByte(), 1, 2, 3)
    private val png = byteArrayOf(0x89.toByte(), 'P'.code.toByte(), 'N'.code.toByte(), 'G'.code.toByte(), 0x0D, 0x0A, 0x1A, 0x0A)

    private fun client(t: Transport, store: KeyValueStore = MemoryStore()) = Client(
        config, tmp.root.resolve("queue"), store, mapOf("model" to "Test"), emptyMap(), t,
        executor = Executors.newSingleThreadScheduledExecutor(), feedbackDir = tmp.root.resolve("feedback"),
    )

    private fun capture(c: Client, fb: VakaFeedback): VakaFeedbackResult {
        val latch = CountDownLatch(1)
        var result: VakaFeedbackResult? = null
        c.captureFeedback(fb) { result = it; latch.countDown() }
        assertTrue(latch.await(5, TimeUnit.SECONDS))
        return result!!
    }

    @Test fun sendsFeedbackWithContextAndAttachments() {
        val t = FakeTransport()
        val c = client(t)
        c.identify("u-42")
        c.setTag("plan", "pro")
        c.screen("Checkout")
        val err = c.captureException(IllegalStateException("boom"))
        val r = capture(c, VakaFeedback("  Export does nothing  ", email = " ann@example.com ", eventId = c.lastEventId,
            source = VakaFeedbackSource.CRASH_PROMPT, attachments = listOf(jpeg, png, byteArrayOf(1, 2, 3))))
        assertEquals(VakaFeedbackResult.Status.SENT, r.status)
        assertEquals("AND-F1", r.shortId)
        val (json, atts) = t.feedback.single()
        assertTrue(json, json.contains("\"type\":\"feedback\""))
        assertTrue(json, json.contains("\"id\":\"${r.id}\""))
        assertTrue(json, json.contains("\"message\":\"Export does nothing\""))
        assertTrue(json, json.contains("\"email\":\"ann@example.com\""))
        assertTrue(json, json.contains("\"event_id\":\"$err\""))
        assertTrue(json, json.contains("\"source\":\"crash_prompt\""))
        assertTrue(json, json.contains("\"platform\":\"android\""))
        assertTrue(json, json.contains("\"screen\":\"Checkout\""))
        assertTrue(json, json.contains("\"user_id\":\"u-42\""))
        assertTrue(json, json.contains("\"plan\":\"pro\""))
        assertTrue(json, json.contains("\"release\":\"1.0.0\""))
        // The non-image attachment is dropped.
        assertEquals(listOf("screenshot-1.jpg", "screenshot-2.png"), atts.map { it.filename })
        assertEquals(listOf("image/jpeg", "image/png"), atts.map { it.contentType })
        assertEquals(0, c.pendingFeedback())
    }

    @Test fun rejectsEmptyMessageWithoutSending() {
        val t = FakeTransport()
        val r = capture(client(t), VakaFeedback("   "))
        assertEquals(VakaFeedbackResult.Status.REJECTED, r.status)
        assertTrue(t.feedback.isEmpty())
    }

    @Test fun truncatesLongMessages() {
        val t = FakeTransport()
        capture(client(t), VakaFeedback("x".repeat(6000)))
        assertTrue(t.feedback.single().first.contains("\"message\":\"" + "x".repeat(5000) + "\""))
    }

    @Test fun queuesOfflineAndSendsLaterAcrossRestarts() {
        val t = FakeTransport().apply { next = FeedbackSendResult.Retry }
        val c = client(t)
        val r = capture(c, VakaFeedback("offline", attachments = listOf(jpeg)))
        assertEquals(VakaFeedbackResult.Status.QUEUED, r.status)
        c.awaitIdle()
        assertEquals(1, c.pendingFeedback())

        // A new process picks it up from disk, with its image.
        val t2 = FakeTransport()
        val c2 = client(t2)
        c2.flush(); c2.awaitIdle()
        val (json, atts) = t2.feedback.single()
        assertTrue(json.contains("\"id\":\"${r.id}\""))
        assertArrayEquals(jpeg, atts.single().bytes)
        assertEquals(0, c2.pendingFeedback())
    }

    @Test fun dropsRejectedFeedback() {
        val t = FakeTransport().apply { next = FeedbackSendResult.Rejected("rate limited") }
        val c = client(t)
        val r = capture(c, VakaFeedback("too many"))
        assertEquals(VakaFeedbackResult.Status.REJECTED, r.status)
        assertEquals("rate limited", r.error)
        c.awaitIdle()
        assertEquals(0, c.pendingFeedback())
    }

    @Test fun remembersCrashOfPreviousRunForThePrompt() {
        val store = MemoryStore()
        val c = client(FakeTransport(), store)
        c.captureCrash(RuntimeException("boom"))
        val crashId = store.getString(Client.K_CRASH_EVENT)!!
        val next = client(FakeTransport(), store)
        assertEquals(crashId, next.takePreviousCrash())
        assertNull(next.takePreviousCrash())
        assertNull(client(FakeTransport(), store).takePreviousCrash())
    }

    @Test fun multipartHasJsonPartAndFiles() {
        val body = String(Multipart.build("B", "{\"message\":\"hi\"}", listOf(Attachment("screenshot-1.jpg", "image/jpeg", "JPG".toByteArray()))))
        assertEquals(
            "--B\r\nContent-Disposition: form-data; name=\"feedback\"\r\nContent-Type: application/json\r\n\r\n{\"message\":\"hi\"}\r\n" +
                "--B\r\nContent-Disposition: form-data; name=\"screenshot\"; filename=\"screenshot-1.jpg\"\r\nContent-Type: image/jpeg\r\n\r\nJPG\r\n" +
                "--B--\r\n",
            body,
        )
    }

    @Test fun parsesServerResponses() {
        assertEquals(FeedbackSendResult.Accepted("abc", "AND-F3"), feedbackResult(202, "{\"id\":\"abc\",\"short_id\":\"AND-F3\"}"))
        assertEquals(FeedbackSendResult.Rejected("feedback rate limit \"hit\""), feedbackResult(429, "{\"error\": \"feedback rate limit \\\"hit\\\"\"}"))
        assertEquals(FeedbackSendResult.Rejected("HTTP 400"), feedbackResult(400, ""))
        assertEquals(FeedbackSendResult.Unauthorized, feedbackResult(401, ""))
        assertEquals(FeedbackSendResult.Retry, feedbackResult(503, ""))
    }
}
