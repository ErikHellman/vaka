package se.hellsoft.vaka.jvm;

import io.opentelemetry.api.GlobalOpenTelemetry;
import io.opentelemetry.api.trace.propagation.W3CTraceContextPropagator;
import io.opentelemetry.context.propagation.ContextPropagators;
import io.opentelemetry.sdk.OpenTelemetrySdk;
import io.opentelemetry.sdk.logs.SdkLoggerProvider;
import io.opentelemetry.sdk.logs.export.BatchLogRecordProcessor;
import io.opentelemetry.sdk.resources.Resource;
import io.opentelemetry.sdk.trace.SdkTracerProvider;
import io.opentelemetry.sdk.trace.export.BatchSpanProcessor;
import io.opentelemetry.sdk.trace.samplers.Sampler;
import se.hellsoft.vaka.jvm.internal.Logger;

/** Creates the OpenTelemetry SDK that exports traces and logs to Vaka. */
final class OpenTelemetrySetup {
    private OpenTelemetrySetup() {}

    static OpenTelemetrySdk create(VakaOptions options, VakaClient client, Logger log) {
        Resource resource = Resource.getDefault().merge(VakaOpenTelemetry.resource(options));
        SdkTracerProvider tracer = SdkTracerProvider.builder()
            .setResource(resource)
            .setSampler(Sampler.parentBased(Sampler.traceIdRatioBased(options.tracesSampleRate)))
            .addSpanProcessor(new VakaSpanProcessor(client))
            .addSpanProcessor(BatchSpanProcessor.builder(VakaOpenTelemetry.spanExporter(options)).build())
            .build();
        SdkLoggerProvider logger = SdkLoggerProvider.builder()
            .setResource(resource)
            .addLogRecordProcessor(BatchLogRecordProcessor.builder(VakaOpenTelemetry.logRecordExporter(options)).build())
            .build();
        OpenTelemetrySdk sdk = OpenTelemetrySdk.builder()
            .setTracerProvider(tracer)
            .setLoggerProvider(logger)
            .setPropagators(ContextPropagators.create(W3CTraceContextPropagator.getInstance()))
            .build();
        if (options.registerGlobalOpenTelemetry) {
            try {
                GlobalOpenTelemetry.set(sdk);
            } catch (IllegalStateException e) {
                log.warn("GlobalOpenTelemetry was already set; use Vaka.openTelemetry() for the Vaka SDK, "
                    + "or set openTelemetry(false) and add VakaOpenTelemetry exporters to your own SDK");
            }
        }
        return sdk;
    }
}
