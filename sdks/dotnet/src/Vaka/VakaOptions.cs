using System.Reflection;

namespace Vaka;

/// <summary>Configuration for <see cref="VakaSdk.Init(VakaOptions)"/>.</summary>
public sealed class VakaOptions
{
    /// <summary>Project ingest key (<c>vk_…</c>). Defaults to <c>$VAKA_KEY</c>. Empty disables the SDK.</summary>
    public string? Key { get; set; } = Env("VAKA_KEY");

    /// <summary>Vaka server URL, such as <c>https://vaka.example.com</c>. Defaults to <c>$VAKA_ENDPOINT</c>.</summary>
    public string? Endpoint { get; set; } = Env("VAKA_ENDPOINT");

    /// <summary>
    /// App version as users see it. Defaults to <c>$VAKA_RELEASE</c>, then the entry assembly's
    /// informational version without its <c>+commit</c> suffix.
    /// </summary>
    public string? Release { get; set; } = Env("VAKA_RELEASE") ?? DefaultRelease();

    /// <summary>
    /// Deployment environment. Defaults to <c>$VAKA_ENVIRONMENT</c>, then <c>$ASPNETCORE_ENVIRONMENT</c> or
    /// <c>$DOTNET_ENVIRONMENT</c> in lower case, then <c>production</c>.
    /// </summary>
    public string Environment { get; set; } = DefaultEnvironment();

    /// <summary>Logical service name (sent as the <c>service</c> tag and as OpenTelemetry <c>service.name</c>). Defaults to the entry assembly name.</summary>
    public string? ServiceName { get; set; } = Env("OTEL_SERVICE_NAME") ?? Assembly.GetEntryAssembly()?.GetName().Name;

    /// <summary>Report unhandled exceptions (<see cref="AppDomain.UnhandledException"/>) as fatal errors. Default <c>true</c>.</summary>
    public bool CaptureUnhandledExceptions { get; set; } = true;

    /// <summary>Report exceptions from faulted tasks nobody awaited (<see cref="TaskScheduler.UnobservedTaskException"/>). Default <c>true</c>.</summary>
    public bool CaptureUnobservedTaskExceptions { get; set; } = true;

    /// <summary>Track one session per process run, for crash-free rates per release. Default <c>true</c>.</summary>
    public bool AutoSessions { get; set; } = true;

    /// <summary>
    /// Namespace prefixes that count as your code (for example <c>MyCompany.</c>). Empty means every frame
    /// except those matching <see cref="InAppExclude"/>.
    /// </summary>
    public List<string> InAppInclude { get; } = new();

    /// <summary>Namespace prefixes that never count as your code. Framework and common library namespaces are listed by default.</summary>
    public List<string> InAppExclude { get; } = new()
    {
        "System.", "Microsoft.", "Windows.", "Internal.", "Interop.", "Newtonsoft.", "Grpc.", "Google.Protobuf.",
        "Npgsql.", "MySqlConnector.", "Dapper.", "Polly.", "Serilog.", "NLog.", "MediatR.", "StackExchange.",
        "OpenTelemetry.", "Swashbuckle.", "Vaka.",
    };

    /// <summary>Breadcrumbs kept and sent with each error (at most 100). Default 100.</summary>
    public int MaxBreadcrumbs { get; set; } = 100;

    /// <summary>How often queued items are sent. Errors are sent at once. Default 5 seconds.</summary>
    public TimeSpan FlushInterval { get; set; } = TimeSpan.FromSeconds(5);

    /// <summary>How long <see cref="VakaSdk.Close"/> and fatal crashes wait for queued data to be sent. Default 2 seconds.</summary>
    public TimeSpan ShutdownTimeout { get; set; } = TimeSpan.FromSeconds(2);

    /// <summary>Called with each item (error, event or session) before it is queued. Return <c>null</c> to drop it.</summary>
    public Func<Dictionary<string, object?>, Dictionary<string, object?>?>? BeforeSend { get; set; }

    /// <summary>Write SDK diagnostics to stderr. Default <c>$VAKA_DEBUG</c> is set.</summary>
    public bool Debug { get; set; } = !string.IsNullOrEmpty(Env("VAKA_DEBUG"));

    /// <summary>For tests and custom networking: the handler the SDK's <see cref="HttpClient"/> uses.</summary>
    public HttpMessageHandler? HttpMessageHandler { get; set; }

    internal bool IsEnabled => !string.IsNullOrWhiteSpace(Key) && !string.IsNullOrWhiteSpace(Endpoint);

    internal string BaseUrl => (Endpoint ?? "").TrimEnd('/');

    private static string? Env(string name)
    {
        var v = System.Environment.GetEnvironmentVariable(name);
        return string.IsNullOrWhiteSpace(v) ? null : v;
    }

    private static string DefaultEnvironment() =>
        Env("VAKA_ENVIRONMENT")
        ?? (Env("ASPNETCORE_ENVIRONMENT") ?? Env("DOTNET_ENVIRONMENT"))?.ToLowerInvariant()
        ?? "production";

    internal static string? DefaultRelease()
    {
        var asm = Assembly.GetEntryAssembly();
        if (asm is null) return null;
        var info = asm.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion;
        if (!string.IsNullOrEmpty(info))
        {
            var plus = info.IndexOf('+');
            return plus > 0 ? info[..plus] : info;
        }
        return asm.GetName().Version?.ToString(3);
    }
}
