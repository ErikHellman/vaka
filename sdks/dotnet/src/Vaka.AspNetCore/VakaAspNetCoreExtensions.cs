using System.Security.Claims;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Diagnostics;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Routing;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.DependencyInjection.Extensions;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using OpenTelemetry.Trace;
using Vaka.OpenTelemetry;

namespace Vaka.AspNetCore;

/// <summary>Options for <c>builder.AddVaka()</c>.</summary>
public sealed class VakaAspNetCoreOptions
{
    /// <summary>SDK options. Filled from the <c>Vaka</c> configuration section and <c>VAKA_*</c> environment variables.</summary>
    public VakaOptions Sdk { get; } = new();

    /// <summary>Which log entries become breadcrumbs and errors.</summary>
    public VakaLoggingOptions Logging { get; } = new();

    /// <summary>
    /// Send OpenTelemetry traces (ASP.NET Core and HttpClient instrumentation) and logs to Vaka. Default <c>true</c>.
    /// Set to <c>false</c> if you configure OpenTelemetry yourself (then call <c>UseVaka()</c> on your builder).
    /// </summary>
    public bool OpenTelemetry { get; set; } = true;

    /// <summary>Extra tracing setup, such as <c>t =&gt; t.AddSource("MyApp")</c> or database instrumentation.</summary>
    public Action<TracerProviderBuilder>? ConfigureTracing { get; set; }

    /// <summary>Add the request middleware at the start of the pipeline automatically. Default <c>true</c>.</summary>
    public bool AutoMiddleware { get; set; } = true;

    /// <summary>
    /// Send the authenticated user's id (<c>NameIdentifier</c> claim, else <c>Identity.Name</c>) with errors.
    /// Off by default: identification is opt-in in Vaka.
    /// </summary>
    public bool CaptureUser { get; set; }

    /// <summary>Send the request query string as part of the URL. Off by default (it often holds tokens or personal data).</summary>
    public bool CaptureQueryString { get; set; }
}

/// <summary>Vaka setup for ASP.NET Core apps and generic-host workers.</summary>
public static class VakaAspNetCoreExtensions
{
    /// <summary>
    /// Initialise Vaka: crash and exception capture, a session per process, <c>ILogger</c> breadcrumbs and errors,
    /// per-request scopes with request details, and (by default) OpenTelemetry traces and logs sent to Vaka.
    /// Reads <c>Vaka:Key</c>, <c>Vaka:Endpoint</c>, <c>Vaka:Release</c>, <c>Vaka:Environment</c> from configuration.
    /// </summary>
    public static TBuilder AddVaka<TBuilder>(this TBuilder builder, Action<VakaAspNetCoreOptions>? configure = null) where TBuilder : IHostApplicationBuilder
    {
        var options = new VakaAspNetCoreOptions();
        Bind(builder.Configuration.GetSection("Vaka"), options.Sdk);
        if (builder.Configuration["Vaka:Environment"] is null && Environment.GetEnvironmentVariable("VAKA_ENVIRONMENT") is null)
            options.Sdk.Environment = builder.Environment.EnvironmentName.ToLowerInvariant();
        if (string.IsNullOrEmpty(options.Sdk.ServiceName)) options.Sdk.ServiceName = builder.Environment.ApplicationName;
        configure?.Invoke(options);

        // Initialise now, so exceptions during the rest of startup are reported too.
        var handle = VakaSdk.Init(options.Sdk);
        builder.Services.AddSingleton(options);
        builder.Services.AddSingleton(new VakaHandle(handle));
        builder.Services.AddHostedService<VakaLifetime>();
        builder.Logging.AddProvider(new VakaLoggerProvider(options.Logging));

        if (options.AutoMiddleware) builder.Services.TryAddEnumerable(ServiceDescriptor.Transient<IStartupFilter, VakaStartupFilter>());

        if (options.OpenTelemetry && options.Sdk.IsEnabled)
        {
            builder.Services.AddOpenTelemetry()
                .UseVaka(options.Sdk, options.Logging.MinimumEventLevel)
                .WithTracing(t =>
                {
                    t.AddAspNetCoreInstrumentation().AddHttpClientInstrumentation();
                    options.ConfigureTracing?.Invoke(t);
                });
        }
        return builder;
    }

    /// <summary>
    /// Add the Vaka request middleware at this point of the pipeline (only needed with <c>AutoMiddleware = false</c>).
    /// Place it first, so it sees exceptions that later middleware logs or rethrows.
    /// </summary>
    public static IApplicationBuilder UseVaka(this IApplicationBuilder app) => app.UseMiddleware<VakaMiddleware>();

    private static void Bind(IConfigurationSection c, VakaOptions o)
    {
        if (c["Key"] is { Length: > 0 } key) o.Key = key;
        if (c["Endpoint"] is { Length: > 0 } ep) o.Endpoint = ep;
        if (c["Release"] is { Length: > 0 } rel) o.Release = rel;
        if (c["Environment"] is { Length: > 0 } env) o.Environment = env;
        if (c["ServiceName"] is { Length: > 0 } svc) o.ServiceName = svc;
        if (bool.TryParse(c["Debug"], out var debug)) o.Debug = debug;
        if (bool.TryParse(c["AutoSessions"], out var sessions)) o.AutoSessions = sessions;
    }

    internal sealed record VakaHandle(IDisposable Handle);

    private sealed class VakaLifetime(VakaHandle handle) : IHostedService
    {
        public Task StartAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task StopAsync(CancellationToken cancellationToken)
        {
            handle.Handle.Dispose();
            return Task.CompletedTask;
        }
    }

    private sealed class VakaStartupFilter : IStartupFilter
    {
        public Action<IApplicationBuilder> Configure(Action<IApplicationBuilder> next) => app =>
        {
            app.UseMiddleware<VakaMiddleware>();
            next(app);
        };
    }
}

/// <summary>
/// Per-request Vaka scope: request URL, method and route on errors, breadcrumbs isolated per request,
/// and unhandled request exceptions reported (then rethrown).
/// </summary>
public sealed class VakaMiddleware(RequestDelegate next, VakaAspNetCoreOptions? options = null)
{
    private const string ActiveKey = "Vaka.Middleware";

    /// <summary>Handle one request.</summary>
    public async Task InvokeAsync(HttpContext context)
    {
        if (!VakaSdk.IsEnabled || context.Items.ContainsKey(ActiveKey))
        {
            await next(context);
            return;
        }
        context.Items[ActiveKey] = true;
        using var _ = VakaSdk.PushScope();
        var req = context.Request;
        var url = $"{req.Scheme}://{req.Host}{req.PathBase}{req.Path}";
        if (options?.CaptureQueryString == true) url += req.QueryString;
        VakaSdk.ConfigureScope(s =>
        {
            s.ClearBreadcrumbs();
            s.Url = url;
            s.SetTag("http.method", req.Method);
            s.SetContext("request", new Dictionary<string, object?> { ["method"] = req.Method, ["url"] = url });
            s.BeforeCapture = scope => Enrich(scope, context);
        });
        try
        {
            await next(context);
        }
        catch (Exception ex)
        {
            VakaSdk.CaptureException(ex, VakaLevel.Error, handled: false, s => s.SetTag("mechanism", "aspnetcore"));
            throw;
        }
    }

    private void Enrich(Scope scope, HttpContext context)
    {
        // The exception handler middleware clears the endpoint before it logs, but keeps it on its feature.
        var endpoint = context.GetEndpoint() ?? context.Features.Get<IExceptionHandlerFeature>()?.Endpoint;
        if (endpoint is RouteEndpoint route && route.RoutePattern.RawText is { } pattern)
            scope.Transaction = $"{context.Request.Method} /{pattern.TrimStart('/')}";
        else if (endpoint?.DisplayName is { } name)
            scope.Transaction = name;
        if (options?.CaptureUser == true && context.User.Identity?.IsAuthenticated == true)
            scope.UserId ??= context.User.FindFirst(ClaimTypes.NameIdentifier)?.Value ?? context.User.Identity.Name;
        if (context.Response.HasStarted) scope.SetTag("http.status_code", context.Response.StatusCode.ToString());
    }
}
