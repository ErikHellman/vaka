package se.hellsoft.vaka.jvm;

import io.opentelemetry.api.common.AttributeKey;
import io.opentelemetry.api.common.Attributes;
import io.opentelemetry.api.common.AttributesBuilder;
import io.opentelemetry.exporter.otlp.http.logs.OtlpHttpLogRecordExporter;
import io.opentelemetry.exporter.otlp.http.trace.OtlpHttpSpanExporter;
import io.opentelemetry.sdk.logs.export.LogRecordExporter;
import io.opentelemetry.sdk.resources.Resource;
import io.opentelemetry.sdk.trace.SpanProcessor;
import io.opentelemetry.sdk.trace.export.SpanExporter;
import java.time.Duration;

/**
 * Building blocks for apps that configure OpenTelemetry themselves (Spring Boot, Quarkus, a custom
 * {@code OpenTelemetrySdk}). With {@code openTelemetry(false)} in {@link VakaOptions}, add these to
 * your own SDK:
 *
 * <pre>{@code
 * SdkTracerProvider.builder()
 *     .setResource(Resource.getDefault().merge(VakaOpenTelemetry.resource(options)))
 *     .addSpanProcessor(VakaOpenTelemetry.spanProcessor())
 *     .addSpanProcessor(BatchSpanProcessor.builder(VakaOpenTelemetry.spanExporter(options)).build())
 * }</pre>
 *
 * Apps using the OpenTelemetry Java agent need none of this: point its OTLP exporter at Vaka with
 * the environment variables in docs/opentelemetry.md.
 */
public final class VakaOpenTelemetry {
    private VakaOpenTelemetry() {}

    /** OTLP/HTTP span exporter to {@code {endpoint}/ingest/otlp/v1/traces}. */
    public static SpanExporter spanExporter(VakaOptions options) {
        return OtlpHttpSpanExporter.builder()
            .setEndpoint(options.endpoint + "/ingest/otlp/v1/traces")
            .addHeader("x-vaka-key", options.key)
            .setCompression("gzip")
            .setTimeout(Duration.ofSeconds(10))
            .build();
    }

    /** OTLP/HTTP log record exporter to {@code {endpoint}/ingest/otlp/v1/logs}. */
    public static LogRecordExporter logRecordExporter(VakaOptions options) {
        return OtlpHttpLogRecordExporter.builder()
            .setEndpoint(options.endpoint + "/ingest/otlp/v1/logs")
            .addHeader("x-vaka-key", options.key)
            .setCompression("gzip")
            .setTimeout(Duration.ofSeconds(10))
            .build();
    }

    /**
     * Adds {@code enduser.id} and {@code session.id} from the running Vaka SDK to spans. Works
     * once {@link Vaka#init} has run; before that it does nothing.
     */
    public static SpanProcessor spanProcessor() { return new DeferredSpanProcessor(); }

    /** {@code service.name}, {@code service.version} (the release) and {@code deployment.environment.name}. */
    public static Resource resource(VakaOptions options) {
        AttributesBuilder a = Attributes.builder();
        if (options.serviceName != null) a.put(AttributeKey.stringKey("service.name"), options.serviceName);
        if (options.release != null) a.put(AttributeKey.stringKey("service.version"), options.release);
        a.put(AttributeKey.stringKey("deployment.environment.name"), options.environment);
        return Resource.create(a.build());
    }

    private static final class DeferredSpanProcessor implements SpanProcessor {
        @Override
        public void onStart(io.opentelemetry.context.Context parentContext, io.opentelemetry.sdk.trace.ReadWriteSpan span) {
            VakaClient c = Vaka.client();
            if (c != null) new VakaSpanProcessor(c).onStart(parentContext, span);
        }

        @Override
        public boolean isStartRequired() { return true; }

        @Override
        public void onEnd(io.opentelemetry.sdk.trace.ReadableSpan span) {}

        @Override
        public boolean isEndRequired() { return false; }
    }
}
