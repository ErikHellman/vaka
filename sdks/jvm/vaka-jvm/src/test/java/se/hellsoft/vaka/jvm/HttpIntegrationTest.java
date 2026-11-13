package se.hellsoft.vaka.jvm;

import static org.junit.jupiter.api.Assertions.*;

import com.sun.net.httpserver.HttpServer;
import io.opentelemetry.api.GlobalOpenTelemetry;
import io.opentelemetry.api.trace.Span;
import io.opentelemetry.context.Scope;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.zip.GZIPInputStream;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import se.hellsoft.vaka.jvm.internal.Json;

/** The real HTTP transport and OTLP exporters against a local fake server. */
class HttpIntegrationTest {
    static final class Received {
        final String path;
        final String key;
        final String sdk;
        final byte[] body;

        Received(String path, String key, String sdk, byte[] body) {
            this.path = path;
            this.key = key;
            this.sdk = sdk;
            this.body = body;
        }
    }

    private HttpServer server;
    private final List<Received> received = new CopyOnWriteArrayList<>();

    @BeforeEach
    void startServer() throws Exception {
        server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", ex -> {
            byte[] raw;
            try (InputStream in = ex.getRequestBody()) { raw = in.readAllBytes(); }
            String enc = ex.getRequestHeaders().getFirst("Content-Encoding");
            if ("gzip".equals(enc)) {
                try (GZIPInputStream gz = new GZIPInputStream(new java.io.ByteArrayInputStream(raw))) {
                    ByteArrayOutputStream out = new ByteArrayOutputStream();
                    gz.transferTo(out);
                    raw = out.toByteArray();
                }
            }
            String key = ex.getRequestHeaders().getFirst("X-Vaka-Key");
            received.add(new Received(ex.getRequestURI().getPath(), key, ex.getRequestHeaders().getFirst("X-Vaka-Sdk"), raw));
            String path = ex.getRequestURI().getPath();
            byte[] res = path.equals("/ingest/v1/flags")
                ? "{\"flags\":{\"new_checkout\":\"one_page\"}}".getBytes(StandardCharsets.UTF_8)
                : "{\"accepted\":1,\"rejected\":[]}".getBytes(StandardCharsets.UTF_8);
            int status = path.startsWith("/ingest/otlp") ? 200 : path.equals("/ingest/v1/flags") ? 200 : 202;
            ex.sendResponseHeaders(status, res.length);
            ex.getResponseBody().write(res);
            ex.close();
        });
        server.start();
    }

    @AfterEach
    void stop() {
        Vaka.close();
        server.stop(0);
        GlobalOpenTelemetry.resetForTest();
    }

    @Test
    void sendsBatchesFlagsAndOtlpTraces() {
        String endpoint = "http://127.0.0.1:" + server.getAddress().getPort() + "/";
        Vaka.init(VakaOptions.builder().key("vk_http").endpoint(endpoint).release("9.9.9").serviceName("http-test")
            .flushInterval(Duration.ofHours(1)).shutdownHook(false).captureUncaughtExceptions(false).build());
        assertNotNull(GlobalOpenTelemetry.get().getTracer("t"));

        Span span = Vaka.openTelemetry().getTracer("t").spanBuilder("GET /x").startSpan();
        try (Scope ignored = span.makeCurrent()) {
            assertEquals("one_page", Vaka.flags("d1").variant("new_checkout", "control"));
            Vaka.captureException(new IllegalStateException("over http"));
        } finally {
            span.end();
        }
        assertTrue(Vaka.flush(Duration.ofSeconds(10)));

        Received batch = received.stream().filter(r -> r.path.equals("/ingest/v1/batch")).findFirst().orElseThrow();
        assertEquals("vk_http", batch.key);
        assertTrue(batch.sdk.startsWith("vaka-jvm/"));
        @SuppressWarnings("unchecked")
        Map<String, Object> body = (Map<String, Object>) Json.parse(new String(batch.body, StandardCharsets.UTF_8));
        assertEquals("vaka-jvm", ((Map<?, ?>) body.get("sdk")).get("name"));
        assertEquals(2, ((List<?>) body.get("items")).size());

        Received traces = received.stream().filter(r -> r.path.equals("/ingest/otlp/v1/traces")).findFirst().orElseThrow();
        String proto = new String(traces.body, StandardCharsets.ISO_8859_1);
        assertTrue(proto.contains("GET /x"));
        assertTrue(proto.contains("http-test"));
        assertTrue(proto.contains("9.9.9"));
        assertTrue(proto.contains("vaka.error_captured"));
        assertTrue(received.stream().anyMatch(r -> r.path.equals("/ingest/v1/flags")));
    }
}
