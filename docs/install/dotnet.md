# Install Vaka in a .NET application

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.
Requires .NET 8 or later.

## 1. Add the package

ASP.NET Core apps and generic-host workers (`Host.CreateApplicationBuilder`):

```sh
dotnet add package Vaka.AspNetCore
```

Console, desktop or other apps without a host: `dotnet add package Vaka` (and
`Vaka.OpenTelemetry` if the app already uses OpenTelemetry).

## 2. Configure the key and endpoint

Add to `appsettings.json` (or set `VAKA_KEY` and `VAKA_ENDPOINT` environment variables in deployment):

```json
{
  "Vaka": {
    "Key": "{{KEY}}",
    "Endpoint": "{{ENDPOINT}}"
  }
}
```

The ingest key is write-only and safe to ship. With no key the SDK does nothing.

## 3. Initialise at startup

ASP.NET Core or worker (`Program.cs`), right after the builder is created:

```csharp
using Vaka.AspNetCore;

var builder = WebApplication.CreateBuilder(args);   // or Host.CreateApplicationBuilder(args)
builder.AddVaka(o => o.Sdk.InAppInclude.Add("MyCompany."));  // your root namespace(s)
```

Do not also add `app.UseVaka()`; the middleware is added automatically.
If the app already calls `builder.Services.AddOpenTelemetry()`, either remove its OTLP exporter
setup or pass `o.OpenTelemetry = false` and call `.UseVaka()` on that OpenTelemetry builder instead
(`using Vaka.OpenTelemetry;`).

Without a host (first line of `Main`):

```csharp
using Vaka;

using var vaka = VakaSdk.Init(o =>
{
    o.Key = "{{KEY}}";
    o.Endpoint = "{{ENDPOINT}}";
    o.InAppInclude.Add("MyCompany.");
});
```

Keep `vaka` alive until the program exits: disposing it ends the session and flushes.

## 4. What you get automatically

- Unhandled exceptions (crashes) with the full cause chain and file/line stack frames,
  sent before the process exits, and a `crashed` session so crash-free rates per release work.
- With `AddVaka`: unhandled request exceptions (once, `handled: false`) with URL, method and route;
  `ILogger` Information+ as breadcrumbs and Error+ as errors; traces for incoming requests and
  `HttpClient` calls, and logs, sent to Vaka through OpenTelemetry.
- Release = the entry assembly version (set `<Version>` in the `.csproj`, or `Vaka:Release`);
  environment = the host environment (`Production`, `Staging`, …) in lower case.

## 5. Report errors, events and flags

```csharp
using Vaka;

try { await importer.RunAsync(); }
catch (Exception e) { VakaSdk.CaptureException(e); }

VakaSdk.AddBreadcrumb("imported 120 rows", "import");
VakaSdk.SetTag("tenant", tenantId);
VakaSdk.Track("import_finished", new Dictionary<string, object?> { ["rows"] = 120 });
VakaSdk.SetUser(userId);   // opt-in identification only

var flags = await VakaSdk.EvaluateFlagsAsync(distinctId: visitorId);
if (flags.IsEnabled("new_checkout")) { /* ... */ }
```

Errors logged with `logger.LogError(ex, "...")` are already reported; don't also call
`CaptureException` for them (it would be deduplicated anyway).

## 6. Verify

1. Run the app with `VAKA_DEBUG=1` and trigger an error (for example a temporary endpoint that
   throws). stderr shows `[vaka] ingest returned 202`.
2. Check it arrived:

   ```sh
   curl -H "Authorization: Bearer $VAKA_TOKEN" "{{ENDPOINT}}/api/v1/projects/{{PROJECT}}/issues?since=1h"
   ```

   or ask an agent with the Vaka MCP server: "list issues in {{PROJECT}} from the last hour".
   The issue's top frames should be your own code (`in_app: true`); if not, fix `InAppInclude`.
3. Remove the temporary error.
