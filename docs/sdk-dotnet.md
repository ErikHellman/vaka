# .NET SDK

NuGet packages (source: `sdks/dotnet`), for .NET 8 and later:

| Package | What it does |
| --- | --- |
| `Vaka` | Core: unhandled exception capture, errors with breadcrumbs, a session per process, analytics events, feature flags, `ILogger` provider. No dependencies besides `Microsoft.Extensions.Logging.Abstractions`. |
| `Vaka.OpenTelemetry` | Points OpenTelemetry .NET traces and logs at Vaka's OTLP/HTTP ingest, with release and environment as resource attributes. |
| `Vaka.AspNetCore` | `builder.AddVaka()` for ASP.NET Core and generic-host workers: everything above, plus per-request scopes, request exceptions, and ASP.NET Core + HttpClient tracing. |

Tracing and logs go through the standard OpenTelemetry SDK ([opentelemetry.md](opentelemetry.md));
the Vaka packages add what OpenTelemetry does not have: crash reporting with grouped issues,
breadcrumbs, release health sessions and feature flags.

Agent-followable install guide: [`/docs/install/dotnet`]({{ENDPOINT}}/docs/install/dotnet).

## ASP.NET Core

```sh
dotnet add package Vaka.AspNetCore
```

```csharp
var builder = WebApplication.CreateBuilder(args);
builder.AddVaka();               // reads Vaka:Key, Vaka:Endpoint, … or VAKA_* env vars
var app = builder.Build();
```

`appsettings.json` (or `Vaka__Key` / `VAKA_KEY` environment variables):

```json
{ "Vaka": { "Key": "vk_...", "Endpoint": "https://vaka.example.com" } }
```

`AddVaka` initialises the SDK at once (so startup crashes are reported), adds the `ILogger`
provider, adds the request middleware at the start of the pipeline, sends OpenTelemetry traces
(ASP.NET Core and HttpClient instrumentation) and logs to Vaka, and flushes on shutdown.

| `VakaAspNetCoreOptions` | Default | Meaning |
| --- | --- | --- |
| `Sdk` | from config | The `VakaOptions` below |
| `Logging.MinimumBreadcrumbLevel` | `Information` | Log entries from this level become breadcrumbs |
| `Logging.MinimumEventLevel` | `Error` | Log entries from this level become Vaka errors (`None` turns it off) |
| `OpenTelemetry` | `true` | Configure OpenTelemetry tracing and logs for Vaka |
| `ConfigureTracing` | none | Extra tracing setup, e.g. `t => t.AddSource("MyApp")` |
| `AutoMiddleware` | `true` | Add the request middleware automatically (else call `app.UseVaka()` first in the pipeline) |
| `CaptureUser` | `false` | Send the authenticated user's id with errors (opt-in) |
| `CaptureQueryString` | `false` | Include the query string in the request URL |

Each request gets its own scope: breadcrumbs from that request only, the URL (without query
string), method and route (`GET /carts/{id}`) as `transaction`, and the trace id. Unhandled request
exceptions are reported as `handled: false`, once, whether or not `UseExceptionHandler` is used.
Release defaults to the entry assembly's informational version (without the `+commit` suffix);
environment to the host environment name in lower case.

## Other apps (console, workers, desktop)

```csharp
using Vaka;

using var vaka = VakaSdk.Init(o =>
{
    o.Key = "vk_...";
    o.Endpoint = "https://vaka.example.com";
    o.Release = "2.4.0";          // default: entry assembly version
});
```

Keep the handle alive until exit: disposing it ends the session and flushes. Generic-host workers
can use `builder.AddVaka()` from `Vaka.AspNetCore` instead.

## Options (`VakaOptions`)

| Option | Default | Meaning |
| --- | --- | --- |
| `Key` | `$VAKA_KEY` | Ingest key; empty disables the SDK |
| `Endpoint` | `$VAKA_ENDPOINT` | Server URL |
| `Release` | `$VAKA_RELEASE`, else entry assembly version | App version |
| `Environment` | `$VAKA_ENVIRONMENT`, `$ASPNETCORE_ENVIRONMENT`, `$DOTNET_ENVIRONMENT`, else `production` | |
| `ServiceName` | `$OTEL_SERVICE_NAME`, else entry assembly name | `service` tag and OpenTelemetry `service.name` |
| `CaptureUnhandledExceptions` | `true` | `AppDomain.UnhandledException` → fatal error, session `crashed` |
| `CaptureUnobservedTaskExceptions` | `true` | `TaskScheduler.UnobservedTaskException` → error |
| `AutoSessions` | `true` | One session per process, for crash-free rates |
| `InAppInclude` | empty | Namespace prefixes that are your code, e.g. `"MyCompany."` |
| `InAppExclude` | `System.`, `Microsoft.`, common libraries | Never your code |
| `MaxBreadcrumbs` | 100 | |
| `FlushInterval` | 5 s | Batch interval (errors are sent at once) |
| `ShutdownTimeout` | 2 s | Wait for queued data on exit and crashes |
| `BeforeSend` | none | Change or drop (`null`) any item before it is queued |
| `Debug` | `$VAKA_DEBUG` set | Log SDK activity to stderr |

## API

```csharp
VakaSdk.CaptureException(ex);                          // inner exceptions become the cause chain
VakaSdk.CaptureException(ex, VakaLevel.Warning, configure: s => s.SetTag("job", "import"));
VakaSdk.CaptureMessage("cache rebuilt", VakaLevel.Info);
VakaSdk.AddBreadcrumb("connected", "db", VakaLevel.Info, new Dictionary<string, object?> { ["host"] = "pg1" });
VakaSdk.SetTag("region", "eu-north-1");
VakaSdk.SetUser("user-123");                           // opt-in identification
VakaSdk.Track("job_finished", new Dictionary<string, object?> { ["kind"] = "import" });

using (VakaSdk.PushScope())                            // child scope, flows with async calls
{
    VakaSdk.ConfigureScope(s => { s.Transaction = "nightly import"; s.SetTag("batch", "42"); });
    // ...
}
VakaSdk.Flush(TimeSpan.FromSeconds(2));
```

`ILogger`: without `Vaka.AspNetCore`, add `logging.AddProvider(new VakaLoggerProvider())`.
Information and above become breadcrumbs; Error and Critical become Vaka errors with the logged
exception's stack trace. The same exception is never reported twice (it is marked when captured).

## Feature flags

```csharp
// Per person (e.g. in a request): evaluates, and records the values on the current scope,
// so errors, events and health split by variant.
var flags = await VakaSdk.EvaluateFlagsAsync(distinctId: visitorId, userId: null,
    properties: new Dictionary<string, object?> { ["plan"] = "pro" });
if (flags.GetVariant("new_checkout") == "one_page") { /* ... */ }

// Per process (CLI tools, desktop apps): load once, then read synchronously.
await VakaSdk.LoadFlagsAsync();
if (VakaSdk.IsFlagEnabled("dark_mode")) { /* ... */ }
```

See [flags.md](flags.md) for targeting, rollouts and experiments.

## OpenTelemetry without ASP.NET Core

```csharp
using Vaka.OpenTelemetry;

services.AddOpenTelemetry()
    .UseVaka()                                   // resource + OTLP traces and logs to Vaka
    .WithTracing(t => t.AddSource("MyApp"));

// or piece by piece:
Sdk.CreateTracerProviderBuilder()
    .SetResourceBuilder(ResourceBuilder.CreateDefault().AddVaka())
    .AddSource("MyApp")
    .AddVakaExporter()
    .Build();
```

The OTLP endpoint is `{Endpoint}/ingest/otlp/v1/traces` (and `/v1/logs`) with the `x-vaka-key`
header. Errors the SDK reports inside an activity carry its `trace_id`, and the activity gets the
attribute `vaka.error_captured = true` so the server doesn't create a second error from the
span's exception event; error log records the `ILogger` provider reported are marked the same way.

## Stack traces

Frames come from `System.Diagnostics.StackTrace`, crash site first, with file and line when the
portable PDB is next to the assembly (the default for `dotnet build` and `dotnet publish`; keep
`<DebugType>portable</DebugType>` or `embedded` in Release). Async state machine frames are shown
as the method that was written (`Cart.LoadAsync`, not `<LoadAsync>d__4.MoveNext`). No debug file
upload is needed.

## Releasing the packages

See [releasing/dotnet.md](releasing/dotnet.md).
