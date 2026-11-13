# Vaka .NET SDK

Crash and exception reporting, breadcrumbs, release health, analytics events and feature flags for
[Vaka](https://vaka.hellsoft.se), plus OpenTelemetry traces and logs, for .NET 8 and later.

| Package | Use it for |
| --- | --- |
| `Vaka.AspNetCore` | ASP.NET Core apps and generic-host workers: `builder.AddVaka()` sets up everything |
| `Vaka` | Any .NET app: `VakaSdk.Init(...)`, `CaptureException`, breadcrumbs, `Track`, flags, `ILogger` provider |
| `Vaka.OpenTelemetry` | Send OpenTelemetry .NET traces and logs to Vaka (`.UseVaka()`, `AddVakaExporter()`) |

```csharp
var builder = WebApplication.CreateBuilder(args);
builder.AddVaka();   // Vaka:Key / Vaka:Endpoint from configuration, or VAKA_KEY / VAKA_ENDPOINT
```

```csharp
using var vaka = Vaka.VakaSdk.Init(o => { o.Key = "vk_..."; o.Endpoint = "https://vaka.example.com"; });
```

Docs: [SDK reference](https://vaka.hellsoft.se/sdk-dotnet.html) ·
[install guide](https://vaka.hellsoft.se/install/dotnet.html) ·
[source](https://github.com/ErikHellman/vaka/tree/main/sdks/dotnet).

## Development

```sh
cd sdks/dotnet
dotnet test                       # .NET 8 and 10
VAKA_KEY=vk_... VAKA_ENDPOINT=http://localhost:8080 dotnet run --project samples/Vaka.Sample.Web
dotnet run --project samples/Vaka.Sample.Web -- crash   # unhandled exception on a background thread
```

Releasing to NuGet: [docs/releasing/dotnet.md](../../docs/releasing/dotnet.md).
