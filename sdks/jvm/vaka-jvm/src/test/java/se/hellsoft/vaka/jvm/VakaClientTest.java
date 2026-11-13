package se.hellsoft.vaka.jvm;

import static org.junit.jupiter.api.Assertions.*;

import io.opentelemetry.api.trace.Span;
import io.opentelemetry.context.Scope;
import io.opentelemetry.sdk.trace.ReadableSpan;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

@SuppressWarnings("unchecked")
class VakaClientTest {
    private final FakeTransport transport = new FakeTransport();
    private VakaClient client;

    private VakaClient start(VakaOptions.Builder b) {
        client = new VakaClient(b.transport(transport).build());
        return client;
    }

    private static VakaOptions.Builder options() {
        return VakaOptions.builder().key("vk_test").endpoint("http://127.0.0.1:1").release("1.2.3")
            .environment("test").serviceName("svc").inAppPackages("se.hellsoft.vaka.jvm.app")
            .flushInterval(Duration.ofHours(1)).openTelemetry(false).shutdownHook(false);
    }

    @AfterEach
    void tearDown() {
        if (client != null) client.close();
    }

    @Test
    void capturesExceptionWithCauseChainScopeAndContext() {
        start(options().tag("region", "eu"));
        client.setUser("user-1");
        client.addBreadcrumb("db", "connected", Level.INFO, Map.of("pool", 4));
        Exception e = new IllegalStateException("Cart is empty", new IllegalArgumentException("items=0"));
        String id = client.captureException(e, Level.ERROR);
        assertTrue(client.flush(Duration.ofSeconds(5)));

        Map<String, Object> err = transport.items("error").get(0);
        assertEquals(id, err.get("event_id"));
        assertEquals("jvm", err.get("platform"));
        assertEquals("1.2.3", err.get("release"));
        assertEquals("test", err.get("environment"));
        assertEquals("error", err.get("level"));
        assertEquals(true, err.get("handled"));
        assertEquals(false, err.get("fatal"));
        assertEquals("user-1", err.get("user_id"));
        assertEquals(Map.of("id", "user-1"), err.get("user"));
        Map<String, Object> tags = (Map<String, Object>) err.get("tags");
        assertEquals("eu", tags.get("region"));
        assertEquals("svc", tags.get("service"));
        List<Map<String, Object>> ex = (List<Map<String, Object>>) err.get("exceptions");
        assertEquals(2, ex.size());
        assertEquals("java.lang.IllegalStateException", ex.get(0).get("type"));
        assertEquals("Cart is empty", ex.get(0).get("value"));
        assertEquals("java.lang.IllegalArgumentException", ex.get(1).get("type"));
        List<Map<String, Object>> frames = (List<Map<String, Object>>) ex.get(0).get("stacktrace");
        assertEquals("capturesExceptionWithCauseChainScopeAndContext", frames.get(0).get("function"));
        assertEquals(VakaClientTest.class.getName(), frames.get(0).get("module"));
        assertEquals("VakaClientTest.java", frames.get(0).get("filename"));
        // inAppPackages is set and does not include this test class.
        assertEquals(false, frames.get(0).get("in_app"));
        List<Map<String, Object>> crumbs = (List<Map<String, Object>>) err.get("breadcrumbs");
        assertEquals("db", crumbs.get(0).get("category"));
        assertEquals(Map.of("pool", 4L), crumbs.get(0).get("data"));
        Map<String, Object> contexts = (Map<String, Object>) err.get("contexts");
        assertNotNull(((Map<String, Object>) contexts.get("runtime")).get("version"));
        assertEquals(client.sessionId(), err.get("session_id"));
    }

    @Test
    void requestScopeOverridesGlobalAndIsolatesBreadcrumbs() {
        start(options());
        client.setUser("global-user");
        client.addBreadcrumb("boot", "started", Level.INFO, null);
        VakaScope request = VakaScope.create().setUser("request-user").setTag("route", "/carts");
        try (Scope ignored = request.makeCurrent()) {
            client.addBreadcrumb("http", "GET /carts", Level.INFO, null);
            client.captureMessage("in request", Level.WARNING);
        }
        VakaScope other = VakaScope.create();
        try (Scope ignored = other.makeCurrent()) {
            client.captureMessage("other request", Level.WARNING);
        }
        client.flush(Duration.ofSeconds(5));
        List<Map<String, Object>> errors = transport.items("error");
        Map<String, Object> first = errors.get(0);
        assertEquals("request-user", first.get("user_id"));
        assertEquals("/carts", ((Map<String, Object>) first.get("tags")).get("route"));
        List<Map<String, Object>> crumbs = (List<Map<String, Object>>) first.get("breadcrumbs");
        assertEquals(List.of("boot", "http"), List.of(crumbs.get(0).get("category"), crumbs.get(1).get("category")));
        Map<String, Object> second = errors.get(1);
        assertEquals("global-user", second.get("user_id"));
        assertEquals(1, ((List<?>) second.get("breadcrumbs")).size());
        assertFalse(((Map<String, Object>) second.get("tags")).containsKey("route"));
    }

    @Test
    void breadcrumbsAreCappedAt100() {
        start(options());
        for (int i = 0; i < 150; i++) client.addBreadcrumb("n", "crumb " + i, Level.INFO, null);
        client.captureMessage("x", Level.ERROR);
        client.flush(Duration.ofSeconds(5));
        List<Map<String, Object>> crumbs = (List<Map<String, Object>>) transport.items("error").get(0).get("breadcrumbs");
        assertEquals(100, crumbs.size());
        assertEquals("crumb 50", crumbs.get(0).get("message"));
    }

    @Test
    void sessionStartsOkAndEndsErroredAfterErrors() {
        start(options());
        client.captureMessage("boom", Level.ERROR);
        client.close();
        List<Map<String, Object>> sessions = transport.items("session");
        assertEquals(2, sessions.size());
        assertEquals("ok", sessions.get(0).get("status"));
        assertEquals("errored", sessions.get(1).get("status"));
        assertEquals(1L, sessions.get(1).get("errors"));
        assertEquals(sessions.get(0).get("session_id"), sessions.get(1).get("session_id"));
        assertNotNull(sessions.get(1).get("duration_ms"));
    }

    @Test
    void cleanExitEndsSessionExited() {
        start(options());
        client.close();
        assertEquals("exited", transport.items("session").get(1).get("status"));
    }

    @Test
    void uncaughtExceptionOnMainThreadIsFatalAndCrashesSession() {
        start(options());
        Thread main = new Thread(() -> {}, "main");
        client.captureCrash(main, new RuntimeException("fatal"));
        Map<String, Object> err = transport.items("error").get(0);
        assertEquals(true, err.get("fatal"));
        assertEquals(false, err.get("handled"));
        assertEquals("fatal", err.get("level"));
        assertEquals("uncaught", ((Map<String, Object>) err.get("tags")).get("mechanism"));
        List<Map<String, Object>> sessions = transport.items("session");
        assertEquals("crashed", sessions.get(sessions.size() - 1).get("status"));
        client.close();
        // No second session end after the crash.
        assertEquals(2, transport.items("session").size());
    }

    @Test
    void uncaughtExceptionOnWorkerThreadKeepsSession() {
        start(options());
        client.captureCrash(new Thread(() -> {}, "worker-1"), new RuntimeException("fatal"));
        assertEquals(1, transport.items("session").size());
        client.close();
        assertEquals("errored", transport.items("session").get(1).get("status"));
    }

    @Test
    void uncaughtHandlerChainsToPrevious() throws Exception {
        Thread.UncaughtExceptionHandler before = Thread.getDefaultUncaughtExceptionHandler();
        Throwable[] seen = new Throwable[1];
        Thread.setDefaultUncaughtExceptionHandler((t, e) -> seen[0] = e);
        try {
            start(options().shutdownHook(false));
            client.installHooks();
            Thread t = new Thread(() -> { throw new IllegalStateException("from thread"); }, "worker");
            t.start();
            t.join();
            assertEquals("from thread", seen[0].getMessage());
            Map<String, Object> err = transport.items("error").get(0);
            assertEquals("worker", ((Map<String, Object>) err.get("tags")).get("thread"));
            client.close();
            Thread.getDefaultUncaughtExceptionHandler().uncaughtException(Thread.currentThread(), new RuntimeException("restored"));
            assertEquals("restored", seen[0].getMessage());
        } finally {
            Thread.setDefaultUncaughtExceptionHandler(before);
        }
    }

    @Test
    void retriesAfterServerErrorAndKeepsItems() {
        start(options());
        transport.respond(503);
        client.captureMessage("a", Level.ERROR);
        client.flush(Duration.ofSeconds(5));
        assertEquals(2, client.pending()); // session + error kept
        client.flush(Duration.ofSeconds(5)); // forced flush ignores the backoff
        assertEquals(0, client.pending());
        assertEquals(2, transport.batchRequests());
    }

    @Test
    void unauthorizedPausesSending() {
        start(options());
        transport.respond(401);
        client.captureMessage("a", Level.ERROR);
        client.flush(Duration.ofSeconds(5));
        client.captureMessage("b", Level.ERROR);
        client.flush(Duration.ofSeconds(5));
        assertEquals(1, transport.batchRequests());
        assertEquals(0, client.pending());
    }

    @Test
    void tooLargeSplitsTheBatch() {
        start(options());
        for (int i = 0; i < 9; i++) client.track("e" + i, Map.of());
        transport.respond(413);
        client.flush(Duration.ofSeconds(5));
        assertEquals(0, client.pending());
        // First attempt with all 10 items failed, then halves were sent.
        assertEquals(10, transport.items().size() - 10);
    }

    @Test
    void queueDropsOldestWhenFull() {
        start(options().maxQueueSize(5).sessions(false));
        transport.respond(503);
        for (int i = 0; i < 8; i++) client.track("e" + i, Map.of());
        assertEquals(5, client.pending());
        client.flush(Duration.ofSeconds(5));
        client.flush(Duration.ofSeconds(5));
        List<Map<String, Object>> events = transport.items("event");
        assertEquals("e3", events.get(events.size() - 5).get("name"));
    }

    @Test
    void tracksEventsWithProps() {
        start(options().sessions(false));
        client.track("signup", Map.of("plan", "pro", "seats", 3, "trial", true));
        client.flush(Duration.ofSeconds(5));
        Map<String, Object> ev = transport.items("event").get(0);
        assertEquals("signup", ev.get("name"));
        assertEquals(Map.of("plan", "pro", "seats", 3L, "trial", true), ev.get("props"));
        assertNotNull(ev.get("anon_id"));
        assertFalse(ev.containsKey("session_id"));
    }

    @Test
    void flagsAreEvaluatedCachedAndExposed() {
        start(options());
        transport.flagsResponse = "{\"flags\":{\"new_checkout\":\"one_page\",\"dark\":true,\"beta\":false},\"details\":{}}";
        VakaScope request = VakaScope.create();
        try (Scope ignored = request.makeCurrent()) {
            Flags flags = client.flags(FlagRequest.forUser("u1").property("plan", "pro"));
            assertTrue(flags.isLoaded());
            assertEquals("one_page", flags.variant("new_checkout", "control"));
            assertTrue(flags.isEnabled("dark"));
            assertFalse(flags.isEnabled("beta"));
            assertFalse(flags.isEnabled("unknown"));
            assertEquals("fallback", flags.variant("dark", "fallback"));
            client.flags(FlagRequest.forUser("u1").property("plan", "pro"));
            client.captureMessage("with flags", Level.ERROR);
        }
        long flagCalls = transport.requests.stream().filter(r -> r.path.equals("/ingest/v1/flags")).count();
        assertEquals(1, flagCalls);
        Map<String, Object> req = transport.requests.stream().filter(r -> r.path.equals("/ingest/v1/flags")).findFirst().get().body();
        assertEquals("u1", req.get("distinct_id"));
        assertEquals("u1", req.get("user_id"));
        assertEquals("test", req.get("environment"));
        assertEquals(Map.of("plan", "pro"), req.get("properties"));
        client.flush(Duration.ofSeconds(5));
        Map<String, Object> flags = (Map<String, Object>) transport.items("error").get(0).get("flags");
        assertEquals(Map.of("new_checkout", "one_page", "dark", "true", "beta", "false", "unknown", "false"), flags);
        // The exposures were recorded on the request scope, not globally.
        assertTrue(client.globalScope().flags().isEmpty());
    }

    @Test
    void flagFailureReturnsFallbacks() {
        start(options());
        transport.flagsResponse = "not json";
        Flags flags = client.flags(FlagRequest.forDistinctId("d"));
        assertFalse(flags.isLoaded());
        assertEquals("control", flags.variant("x", "control"));
    }

    @Test
    void errorCarriesTraceIdAndMarksSpan() {
        client = new VakaClient(options().openTelemetry(true).registerGlobalOpenTelemetry(false).transport(transport).build());
        Span span = client.openTelemetry().getTracer("t").spanBuilder("op").startSpan();
        try (Scope ignored = span.makeCurrent(); Scope v = VakaScope.create().setUser("u9").makeCurrent()) {
            Span child = client.openTelemetry().getTracer("t").spanBuilder("child").startSpan();
            assertEquals("u9", ((ReadableSpan) child).getAttribute(VakaSpanProcessor.ENDUSER_ID));
            assertEquals(client.sessionId(), ((ReadableSpan) child).getAttribute(VakaSpanProcessor.SESSION_ID));
            child.end();
            client.captureException(new RuntimeException("traced"), Level.ERROR);
            assertEquals(Boolean.TRUE, ((ReadableSpan) span).getAttribute(VakaClient.ERROR_CAPTURED));
        } finally {
            span.end();
        }
        client.flush(Duration.ofSeconds(5));
        Map<String, Object> err = transport.items("error").get(0);
        assertEquals(span.getSpanContext().getTraceId(), err.get("trace_id"));
        assertEquals(span.getSpanContext().getSpanId(), err.get("span_id"));
    }

    @Test
    void disabledWithoutKey() {
        assertNull(Vaka.init(VakaOptions.builder().key("").endpoint("").build()));
        assertFalse(Vaka.isEnabled());
        assertNull(Vaka.captureException(new RuntimeException()));
        assertFalse(Vaka.flags("x").isEnabled("anything"));
        assertTrue(Vaka.flush(Duration.ofMillis(10)));
    }
}
