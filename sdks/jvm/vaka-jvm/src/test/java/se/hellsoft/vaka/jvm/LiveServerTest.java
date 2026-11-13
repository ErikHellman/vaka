package se.hellsoft.vaka.jvm;

import static org.junit.jupiter.api.Assertions.*;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import io.opentelemetry.api.GlobalOpenTelemetry;
import io.opentelemetry.api.trace.Span;
import io.opentelemetry.api.trace.SpanKind;
import io.opentelemetry.context.Scope;
import java.time.Duration;
import java.util.Map;
import org.junit.jupiter.api.Test;

/** Runs only with VAKA_TEST_ENDPOINT and VAKA_TEST_KEY: sends real data to a Vaka server. */
class LiveServerTest {
    @Test
    void sendsToRealServer() {
        String endpoint = System.getenv("VAKA_TEST_ENDPOINT");
        String key = System.getenv("VAKA_TEST_KEY");
        assumeTrue(endpoint != null && key != null, "VAKA_TEST_ENDPOINT / VAKA_TEST_KEY not set");
        try {
            Vaka.init(VakaOptions.builder().endpoint(endpoint).key(key).release("jvm-live@1.0.0").serviceName("jvm-live")
                .inAppPackages("se.hellsoft.vaka.jvm").shutdownHook(false).debug(true).build());
            Span span = Vaka.openTelemetry().getTracer("live").spanBuilder("GET /live/{id}").setSpanKind(SpanKind.SERVER)
                .setAttribute("http.request.method", "GET").setAttribute("http.route", "/live/{id}").startSpan();
            try (Scope s = span.makeCurrent(); Scope v = VakaScope.create().setUser("live-user").makeCurrent()) {
                Flags flags = Vaka.flags(FlagRequest.forUser("live-user"));
                assertTrue(flags.isLoaded(), "flag evaluation failed");
                Vaka.addBreadcrumb("live", "before error");
                Vaka.captureException(new IllegalStateException("JVM live test", new RuntimeException("cause")));
                Vaka.track("jvm_live_event", Map.of("n", 1));
            } finally {
                span.end();
            }
            assertTrue(Vaka.flush(Duration.ofSeconds(10)));
        } finally {
            Vaka.close();
            GlobalOpenTelemetry.resetForTest();
        }
    }
}
