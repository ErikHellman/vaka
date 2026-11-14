package se.hellsoft.vaka.jvm;

import static org.junit.jupiter.api.Assertions.*;

import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.Map;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

@SuppressWarnings("unchecked")
class FeedbackTest {
    private final FakeTransport transport = new FakeTransport();
    private final VakaClient client = new VakaClient(VakaOptions.builder().key("vk_test").endpoint("http://127.0.0.1:1")
        .release("1.2.3").environment("test").tag("region", "eu").flushInterval(Duration.ofHours(1))
        .openTelemetry(false).shutdownHook(false).transport(transport).build());

    @AfterEach
    void tearDown() { client.close(); }

    @Test
    void sendsJsonWithScopeAndLinks() {
        client.setUser("user-7");
        client.currentScope().setFlag("new_export", "on");
        String eventId = client.captureException(new IllegalStateException("export failed"), Level.ERROR);
        FeedbackResult r = client.captureFeedback(Feedback.of("  The export button does nothing  ")
            .email("ann@example.com").eventId(eventId).issue("API-12").sessionId("s-1")
            .ip("203.0.113.9").tag("plan", "pro"));
        assertEquals("fb-1", r.id);
        assertEquals("API-F1", r.shortId);

        FakeTransport.Request req = transport.feedback().get(0);
        assertEquals("application/json", req.contentType);
        assertEquals("203.0.113.9", req.headers.get("X-Forwarded-For"));
        Map<String, Object> item = req.body();
        assertEquals("The export button does nothing", item.get("message"));
        assertEquals("ann@example.com", item.get("email"));
        assertEquals(eventId, item.get("event_id"));
        assertEquals("API-12", item.get("issue"));
        assertEquals("s-1", item.get("session_id"));
        assertEquals("user-7", item.get("user_id"));
        assertEquals("api", item.get("source"));
        assertEquals("jvm", item.get("platform"));
        assertEquals("1.2.3", item.get("release"));
        assertEquals(Map.of("region", "eu", "plan", "pro"), item.get("tags"));
        assertEquals(Map.of("new_export", "on"), item.get("flags"));
    }

    @Test
    void sendsAttachmentsAsMultipart() {
        byte[] png = "\u0089PNG\r\n\u001a\n0000".getBytes(StandardCharsets.ISO_8859_1);
        client.captureFeedback(Feedback.of("with a screenshot").attachment("shot.png", png));
        FakeTransport.Request req = transport.feedback().get(0);
        assertTrue(req.contentType.startsWith("multipart/form-data; boundary="));
        String boundary = req.contentType.substring(req.contentType.indexOf('=') + 1);
        String text = new String(req.raw, StandardCharsets.ISO_8859_1);
        assertTrue(text.startsWith("--" + boundary + "\r\nContent-Disposition: form-data; name=\"feedback\""), text);
        assertTrue(text.contains("\"message\":\"with a screenshot\""), text);
        assertTrue(text.contains("filename=\"shot.png\"\r\nContent-Type: image/png\r\n\r\n" + new String(png, StandardCharsets.ISO_8859_1) + "\r\n"), text);
        assertTrue(text.endsWith("--" + boundary + "--\r\n"));
    }

    @Test
    void rejectsInvalidFeedbackAndReportsServerErrors() {
        assertThrows(FeedbackException.class, () -> client.captureFeedback(Feedback.of("   ")));
        Feedback many = Feedback.of("x");
        for (int i = 0; i < 4; i++) many.attachment("a.png", new byte[1]);
        assertThrows(FeedbackException.class, () -> client.captureFeedback(many));
        transport.respond(429, "{\"error\":\"too much feedback\"}");
        FeedbackException e = assertThrows(FeedbackException.class, () -> client.captureFeedback(Feedback.of("again")));
        assertEquals(429, e.status());
        assertTrue(e.getMessage().contains("too much feedback"), e.getMessage());
    }

    @Test
    void staticApiThrowsWhenNotInitialised() {
        Vaka.close();
        FeedbackException e = assertThrows(FeedbackException.class, () -> Vaka.captureFeedback(Feedback.of("hi")));
        assertEquals(0, e.status());
    }
}
