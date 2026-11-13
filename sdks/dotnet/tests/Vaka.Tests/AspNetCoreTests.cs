using System.Net;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.TestHost;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Vaka.AspNetCore;
using Vaka.OpenTelemetry;
using Xunit;

namespace Vaka.Tests;

public class AspNetCoreTests
{
    private static async Task<(WebApplication App, FakeIngest Fake)> StartApp(bool useExceptionHandler, bool otel = false)
    {
        var fake = new FakeIngest();
        var builder = WebApplication.CreateBuilder(new WebApplicationOptions { EnvironmentName = "Staging" });
        builder.WebHost.UseTestServer();
        builder.Logging.ClearProviders();
        builder.Configuration["Vaka:Key"] = "vk_test";
        builder.Configuration["Vaka:Endpoint"] = "http://vaka.test";
        builder.Configuration["Vaka:Release"] = "9.9.9";
        builder.AddVaka(o =>
        {
            o.Sdk.HttpMessageHandler = fake;
            o.Sdk.FlushInterval = TimeSpan.FromMilliseconds(50);
            o.Sdk.InAppInclude.Add("Vaka.Tests");
            o.OpenTelemetry = otel;
        });
        var app = builder.Build();
        if (useExceptionHandler) app.UseExceptionHandler(e => e.Run(ctx => { ctx.Response.StatusCode = 500; return ctx.Response.WriteAsync("oops"); }));
        app.MapGet("/carts/{id}", (string id, ILogger<AspNetCoreTests> log) =>
        {
            log.LogInformation("loading cart {Id}", id);
            if (id == "boom") throw new InvalidOperationException("cart exploded");
            if (id == "logged") log.LogError(new TimeoutException("db"), "could not load cart {Id}", id);
            return Results.Ok(id);
        });
        await app.StartAsync();
        return (app, fake);
    }

    [Fact]
    public async Task UnhandledRequestExceptionIsReportedWithRequestContext()
    {
        var (app, fake) = await StartApp(useExceptionHandler: false);
        await using (app)
        {
            var client = app.GetTestClient();
            await Assert.ThrowsAsync<InvalidOperationException>(() => client.GetAsync("/carts/boom?token=secret"));
            VakaSdk.Flush(TimeSpan.FromSeconds(5));
            await app.StopAsync();
        }
        var err = fake.Items("error").Single();
        Assert.False((bool)err["handled"]!);
        Assert.Equal("staging", (string?)err["environment"]);
        Assert.Equal("9.9.9", (string?)err["release"]);
        Assert.Equal("http://localhost/carts/boom", (string?)err["url"]);
        Assert.Equal("GET /carts/{id}", (string?)err["tags"]!["transaction"]);
        Assert.Equal("GET", (string?)err["contexts"]!["request"]!["method"]);
        Assert.Contains(err["breadcrumbs"]!.AsArray(), b => (string?)b!["message"] == "loading cart boom");
        Assert.Equal("System.InvalidOperationException", (string?)err["exceptions"]![0]!["type"]);
        Assert.Equal("errored", (string?)fake.Items("session").Last()["status"]);
    }

    [Fact]
    public async Task ExceptionHandlerPathReportsOnceAsUnhandled()
    {
        var (app, fake) = await StartApp(useExceptionHandler: true);
        await using (app)
        {
            var resp = await app.GetTestClient().GetAsync("/carts/boom");
            Assert.Equal(HttpStatusCode.InternalServerError, resp.StatusCode);
            VakaSdk.Flush(TimeSpan.FromSeconds(5));
            await app.StopAsync();
        }
        var err = fake.Items("error").Single();
        Assert.False((bool)err["handled"]!);
        Assert.Equal("GET /carts/{id}", (string?)err["tags"]!["transaction"]);
    }

    [Fact]
    public async Task LoggedErrorsBecomeHandledErrors()
    {
        var (app, fake) = await StartApp(useExceptionHandler: false);
        await using (app)
        {
            var resp = await app.GetTestClient().GetAsync("/carts/logged");
            Assert.Equal(HttpStatusCode.OK, resp.StatusCode);
            VakaSdk.Flush(TimeSpan.FromSeconds(5));
            await app.StopAsync();
        }
        var err = fake.Items("error").Single();
        Assert.True((bool)err["handled"]!);
        Assert.Equal("System.TimeoutException", (string?)err["exceptions"]![0]!["type"]);
        Assert.Equal("Vaka.Tests.AspNetCoreTests", (string?)err["tags"]!["logger"]);
        Assert.Equal("http://localhost/carts/logged", (string?)err["url"]);
        // Breadcrumbs belong to this request only.
        Assert.DoesNotContain(err["breadcrumbs"]!.AsArray(), b => ((string?)b!["message"])?.Contains("boom") == true);
    }

    [Fact]
    public async Task OpenTelemetrySpansGoToVakaOtlpEndpoint()
    {
        var (app, fake) = await StartApp(useExceptionHandler: false, otel: true);
        await using (app)
        {
            await app.GetTestClient().GetAsync("/carts/1");
            await app.StopAsync(); // disposes the tracer provider, which flushes the exporter
        }
        var otlp = fake.Requests.Where(r => r.Path.StartsWith("/ingest/otlp/")).ToList();
        Assert.Contains(otlp, r => r.Path == "/ingest/otlp/v1/traces");
        var req = otlp.First(r => r.Path == "/ingest/otlp/v1/traces").Request;
        Assert.Equal("vk_test", req.Headers.GetValues("x-vaka-key").Single());
    }

    [Fact]
    public void OtlpEndpointsAreBuiltFromTheVakaEndpoint()
    {
        var o = new VakaOptions { Endpoint = "https://vaka.example.com/" };
        Assert.Equal("https://vaka.example.com/ingest/otlp/v1/traces", VakaOpenTelemetryExtensions.OtlpEndpoint(o, "traces").ToString());
        Assert.Equal("https://vaka.example.com/ingest/otlp/v1/logs", VakaOpenTelemetryExtensions.OtlpEndpoint(o, "logs").ToString());
    }
}
