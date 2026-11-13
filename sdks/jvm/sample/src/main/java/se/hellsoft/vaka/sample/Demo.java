package se.hellsoft.vaka.sample;

import io.opentelemetry.api.trace.Span;
import io.opentelemetry.api.trace.SpanKind;
import io.opentelemetry.api.trace.Tracer;
import io.opentelemetry.context.Scope;
import java.time.Duration;
import java.util.Map;
import se.hellsoft.vaka.jvm.Flags;
import se.hellsoft.vaka.jvm.FlagRequest;
import se.hellsoft.vaka.jvm.Level;
import se.hellsoft.vaka.jvm.Vaka;
import se.hellsoft.vaka.jvm.VakaOptions;
import se.hellsoft.vaka.jvm.VakaScope;

/**
 * Sends a handled error inside a traced "request", an event and a flag evaluation, then crashes
 * the main thread. Run with VAKA_KEY and VAKA_ENDPOINT set: {@code ./gradlew :sample:run}.
 */
public final class Demo {
    public static void main(String[] args) {
        Vaka.init(VakaOptions.builder()
            .release("demo@1.0.0")
            .environment("development")
            .serviceName("vaka-jvm-demo")
            .inAppPackages("se.hellsoft.vaka.sample")
            .debug(true)
            .build());

        Tracer tracer = Vaka.openTelemetry().getTracer("demo");
        Span span = tracer.spanBuilder("GET /carts/{id}").setSpanKind(SpanKind.SERVER)
            .setAttribute("http.request.method", "GET").setAttribute("http.route", "/carts/{id}").startSpan();
        try (Scope s = span.makeCurrent(); Scope v = VakaScope.create().setUser("demo-user").makeCurrent()) {
            Flags flags = Vaka.flags(FlagRequest.forUser("demo-user").property("plan", "pro"));
            Vaka.addBreadcrumb("cart", "loaded cart " + flags.variant("new_checkout", "control"));
            try {
                checkout(0);
            } catch (IllegalStateException e) {
                Vaka.captureException(e);
            }
            Vaka.track("checkout_started", Map.of("items", 0));
            span.setAttribute("http.response.status_code", 500L);
        } finally {
            span.end();
        }
        Vaka.captureMessage("demo finished", Level.INFO);
        Vaka.flush(Duration.ofSeconds(5));
        if (args.length > 0 && args[0].equals("crash")) throw new IllegalStateException("demo crash");
    }

    private static void checkout(int items) {
        if (items == 0) throw new IllegalStateException("Cart is empty", new IllegalArgumentException("items=0"));
    }
}
