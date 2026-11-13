// Vaka sample: run with
//   VAKA_KEY=vk_... VAKA_ENDPOINT=http://localhost:8080 dotnet run
// then open /, /crash, /logged-error, /flags and /track. `dotnet run -- crash` crashes the process.
using Vaka;
using Vaka.AspNetCore;

if (args.Contains("crash"))
{
    using var vaka = VakaSdk.Init(o => o.Release = "1.0.0");
    VakaSdk.AddBreadcrumb("about to crash", "lifecycle");
    var t = new Thread(() => throw new InvalidOperationException("background thread crashed"));
    t.Start();
    t.Join();
    return;
}

var builder = WebApplication.CreateBuilder(args);
builder.AddVaka(o => o.Sdk.InAppInclude.Add("SampleWeb"));
var app = builder.Build();

app.MapGet("/", () => "Vaka .NET sample");
app.MapGet("/crash", () => SampleWeb.Cart.Checkout(null));
app.MapGet("/logged-error", (ILogger<Program> log) =>
{
    try { SampleWeb.Cart.Checkout(null); }
    catch (Exception e) { log.LogError(e, "checkout failed"); }
    return "logged";
});
app.MapGet("/flags", async () =>
{
    var flags = await VakaSdk.EvaluateFlagsAsync("visitor-1");
    VakaSdk.Track("flags_checked", new Dictionary<string, object?> { ["count"] = flags.Values.Count });
    return flags.Values;
});
app.MapGet("/track", () => { VakaSdk.Track("sample_event", new Dictionary<string, object?> { ["source"] = "sample" }); return "tracked"; });

app.Run();

namespace SampleWeb
{
    public static class Cart
    {
        public static string Checkout(string? cartId) =>
            cartId ?? throw new ArgumentNullException(nameof(cartId), "no cart in session");
    }
}
