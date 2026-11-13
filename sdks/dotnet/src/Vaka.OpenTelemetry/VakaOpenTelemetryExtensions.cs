using Microsoft.Extensions.Logging;
using OpenTelemetry;
using OpenTelemetry.Exporter;
using OpenTelemetry.Logs;
using OpenTelemetry.Resources;
using OpenTelemetry.Trace;

namespace Vaka.OpenTelemetry;

/// <summary>
/// Wires OpenTelemetry .NET to Vaka's OTLP/HTTP ingest (<c>{endpoint}/ingest/otlp/v1/traces</c> and
/// <c>/v1/logs</c>), with the Vaka release and environment as resource attributes.
/// Every method takes the options passed to <see cref="VakaSdk.Init(VakaOptions)"/>; without them it
/// uses the initialised SDK's options, or reads <c>VAKA_*</c> environment variables.
/// </summary>
public static class VakaOpenTelemetryExtensions
{
    /// <summary>
    /// Configure resource, traces and logs for Vaka on an <c>AddOpenTelemetry()</c> builder.
    /// Add your instrumentation (<c>AddAspNetCoreInstrumentation()</c>, <c>AddHttpClientInstrumentation()</c>,
    /// <c>AddSource("MyApp")</c>) with <c>WithTracing</c> as usual.
    /// </summary>
    /// <param name="builder">The OpenTelemetry builder.</param>
    /// <param name="options">Vaka options (key, endpoint, release, environment, service name).</param>
    /// <param name="sdkCapturesErrorsFrom">Log level from which the Vaka logger provider reports errors itself;
    /// such log records are marked so the server does not create a duplicate error. <c>null</c>: not marked.</param>
    public static TBuilder UseVaka<TBuilder>(this TBuilder builder, VakaOptions? options = null, LogLevel? sdkCapturesErrorsFrom = null)
        where TBuilder : IOpenTelemetryBuilder
    {
        var o = Resolve(options);
        builder.ConfigureResource(r => r.AddVaka(o));
        builder.WithTracing(t => t.AddVakaExporter(o));
        builder.WithLogging(l => l.AddVakaExporter(o, sdkCapturesErrorsFrom), lo => { lo.IncludeFormattedMessage = true; lo.IncludeScopes = true; });
        return builder;
    }

    /// <summary>Add <c>service.name</c>, <c>service.version</c> (the release) and <c>deployment.environment.name</c>.</summary>
    public static ResourceBuilder AddVaka(this ResourceBuilder resource, VakaOptions? options = null)
    {
        var o = Resolve(options);
        var attrs = new List<KeyValuePair<string, object>> { new("deployment.environment.name", o.Environment) };
        if (!string.IsNullOrEmpty(o.ServiceName)) resource.AddService(o.ServiceName!, serviceVersion: o.Release, autoGenerateServiceInstanceId: true);
        else if (!string.IsNullOrEmpty(o.Release)) attrs.Add(new("service.version", o.Release!));
        return resource.AddAttributes(attrs);
    }

    /// <summary>Export spans to Vaka over OTLP/HTTP (protobuf).</summary>
    public static TracerProviderBuilder AddVakaExporter(this TracerProviderBuilder tracing, VakaOptions? options = null)
    {
        var o = Resolve(options);
        if (!o.IsEnabled) return tracing;
        return tracing.AddOtlpExporter(e => Configure(e, o, "traces"));
    }

    /// <summary>Export log records to Vaka over OTLP/HTTP (protobuf).</summary>
    public static LoggerProviderBuilder AddVakaExporter(this LoggerProviderBuilder logging, VakaOptions? options = null, LogLevel? sdkCapturesErrorsFrom = null)
    {
        var o = Resolve(options);
        if (!o.IsEnabled) return logging;
        if (sdkCapturesErrorsFrom is { } level && level != LogLevel.None) logging.AddProcessor(new MarkCapturedProcessor(level));
        return logging.AddOtlpExporter(e => Configure(e, o, "logs"));
    }

    /// <summary>Export log records to Vaka over OTLP/HTTP, for <c>logging.AddOpenTelemetry(o => o.AddVakaExporter())</c>.</summary>
    public static OpenTelemetryLoggerOptions AddVakaExporter(this OpenTelemetryLoggerOptions logging, VakaOptions? options = null, LogLevel? sdkCapturesErrorsFrom = null)
    {
        var o = Resolve(options);
        if (!o.IsEnabled) return logging;
        if (sdkCapturesErrorsFrom is { } level && level != LogLevel.None) logging.AddProcessor(new MarkCapturedProcessor(level));
        return logging.AddOtlpExporter(e => Configure(e, o, "logs"));
    }

    /// <summary>The OTLP URL for a signal (<c>traces</c> or <c>logs</c>).</summary>
    public static Uri OtlpEndpoint(VakaOptions options, string signal) => new(options.BaseUrl + "/ingest/otlp/v1/" + signal);

    private static void Configure(OtlpExporterOptions e, VakaOptions o, string signal)
    {
        e.Protocol = OtlpExportProtocol.HttpProtobuf;
        e.Endpoint = OtlpEndpoint(o, signal);
        e.Headers = "x-vaka-key=" + o.Key;
        if (o.HttpMessageHandler is { } handler) e.HttpClientFactory = () => new HttpClient(handler, disposeHandler: false);
    }

    private static VakaOptions Resolve(VakaOptions? options) => options ?? VakaSdk.Options ?? new VakaOptions();

    /// <summary>Marks log records the Vaka logger provider has already reported as errors.</summary>
    private sealed class MarkCapturedProcessor(LogLevel from) : BaseProcessor<LogRecord>
    {
        public override void OnEnd(LogRecord record)
        {
            if (record.LogLevel < from || !VakaSdk.IsEnabled) return;
            var attrs = record.Attributes is { } a ? new List<KeyValuePair<string, object?>>(a) : new List<KeyValuePair<string, object?>>();
            attrs.Add(new(VakaSdk.ErrorCapturedAttribute, true));
            record.Attributes = attrs;
        }
    }
}
