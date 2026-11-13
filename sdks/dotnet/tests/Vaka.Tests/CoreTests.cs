using System.Net;
using System.Runtime.CompilerServices;
using Xunit;

namespace Vaka.Tests;

public class CoreTests
{
    [Fact]
    public void DisabledWithoutKey()
    {
        using var h = VakaSdk.Init(o => { o.Key = ""; o.Endpoint = "http://x"; });
        Assert.False(VakaSdk.IsEnabled);
        Assert.Null(VakaSdk.CaptureMessage("nothing"));
    }

    [Fact]
    public void ErrorItemHasExceptionChainFramesAndContext()
    {
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            VakaSdk.SetTag("region", "eu");
            VakaSdk.AddBreadcrumb("opened cart", "navigation");
            try { Outer(); }
            catch (Exception ex) { Assert.NotNull(VakaSdk.CaptureException(ex)); }
            Assert.True(VakaSdk.Flush(TimeSpan.FromSeconds(5)));
        }
        var batch = fake.Requests.First(r => r.Path == "/ingest/v1/batch");
        Assert.Equal("vk_test", batch.Request.Headers.GetValues("X-Vaka-Key").Single());
        Assert.Equal("vaka-dotnet", (string?)batch.Body["sdk"]!["name"]);

        var err = fake.Items("error").Single();
        Assert.Equal("dotnet", (string?)err["platform"]);
        Assert.Equal("1.2.3", (string?)err["release"]);
        Assert.Equal("test", (string?)err["environment"]);
        Assert.Equal("error", (string?)err["level"]);
        Assert.True((bool)err["handled"]!);
        Assert.False((bool)err["fatal"]!);
        Assert.Equal("eu", (string?)err["tags"]!["region"]);
        Assert.Equal("tests", (string?)err["tags"]!["service"]);
        Assert.Equal("opened cart", (string?)err["breadcrumbs"]![0]!["message"]);
        Assert.Equal(".NET", (string?)err["contexts"]!["runtime"]!["name"]);
        Assert.NotNull((string?)err["session_id"]);

        var exceptions = err["exceptions"]!.AsArray();
        Assert.Equal(2, exceptions.Count);
        Assert.Equal("System.InvalidOperationException", (string?)exceptions[0]!["type"]);
        Assert.Equal("checkout failed", (string?)exceptions[0]!["value"]);
        Assert.Equal("System.ArgumentException", (string?)exceptions[1]!["type"]);

        // Crash site first: the inner exception was thrown in Inner(), called from Outer().
        var inner = exceptions[1]!["stacktrace"]!.AsArray();
        Assert.Equal("Inner", (string?)inner[0]!["function"]);
        Assert.Equal("Vaka.Tests.CoreTests", (string?)inner[0]!["module"]);
        Assert.True((bool)inner[0]!["in_app"]!);
        Assert.Equal("CoreTests.cs", (string?)inner[0]!["filename"]);
        Assert.True((int)inner[0]!["lineno"]! > 0);
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void Outer()
    {
        try { Inner(); }
        catch (Exception e) { throw new InvalidOperationException("checkout failed", e); }
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void Inner() => throw new ArgumentException("cart is empty");

    [Fact]
    public async Task AsyncFramesGetReadableNames()
    {
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            try { await LoadCartAsync(); }
            catch (Exception ex) { VakaSdk.CaptureException(ex); }
            VakaSdk.Flush(TimeSpan.FromSeconds(5));
        }
        var frames = fake.Items("error").Single()["exceptions"]![0]!["stacktrace"]!.AsArray();
        Assert.Equal("LoadCartAsync", (string?)frames[0]!["function"]);
        Assert.Equal("Vaka.Tests.CoreTests", (string?)frames[0]!["module"]);
        Assert.Contains(frames, f => (bool)f!["in_app"]! == false);
    }

    private static async Task LoadCartAsync()
    {
        await Task.Yield();
        throw new TimeoutException("db timeout");
    }

    [Fact]
    public void SameExceptionIsReportedOnce()
    {
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            var ex = new InvalidOperationException("x");
            var a = VakaSdk.CaptureException(ex);
            var b = VakaSdk.CaptureException(ex);
            Assert.Equal(a, b);
            VakaSdk.Flush(TimeSpan.FromSeconds(5));
        }
        Assert.Single(fake.Items("error"));
    }

    [Fact]
    public void SessionStartsAndEndsErroredAfterErrors()
    {
        var (fake, h) = FakeIngest.Init();
        using (h) { VakaSdk.CaptureMessage("handled thing", VakaLevel.Warning); }
        var sessions = fake.Items("session");
        Assert.Equal(2, sessions.Count);
        Assert.Equal("ok", (string?)sessions[0]["status"]);
        Assert.Equal("errored", (string?)sessions[1]["status"]);
        Assert.Equal(1, (int)sessions[1]["errors"]!);
        Assert.Equal((string?)sessions[0]["session_id"], (string?)sessions[1]["session_id"]);
        var msg = fake.Items("error").Single();
        Assert.Equal("handled thing", (string?)msg["message"]);
        Assert.Equal("warning", (string?)msg["level"]);
    }

    [Fact]
    public void CleanExitEndsSessionExited()
    {
        var (fake, h) = FakeIngest.Init();
        h.Dispose();
        Assert.Equal("exited", (string?)fake.Items("session").Last()["status"]);
        Assert.False(VakaSdk.IsEnabled);
    }

    [Fact]
    public void EventsCarryPropsUserAndFlags()
    {
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            VakaSdk.SetUser("user-1");
            VakaSdk.ConfigureScope(s => s.SetFlag("beta", "false"));
            VakaSdk.Track("checkout", new Dictionary<string, object?> { ["plan"] = "pro", ["items"] = 3 });
        }
        var ev = fake.Items("event").Single();
        Assert.Equal("checkout", (string?)ev["name"]);
        Assert.Equal("pro", (string?)ev["props"]!["plan"]);
        Assert.Equal(3, (int)ev["props"]!["items"]!);
        Assert.Equal("user-1", (string?)ev["user_id"]);
        Assert.Equal("false", (string?)ev["flags"]!["beta"]);
    }

    [Fact]
    public async Task ScopesAreIsolatedPerAsyncFlow()
    {
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            VakaSdk.AddBreadcrumb("global");
            async Task Request(string name)
            {
                using var _ = VakaSdk.PushScope();
                VakaSdk.SetTag("request", name);
                VakaSdk.AddBreadcrumb("in " + name);
                await Task.Delay(20);
                VakaSdk.CaptureMessage("from " + name);
            }
            await Task.WhenAll(Request("a"), Request("b"));
            VakaSdk.CaptureMessage("outside");
            VakaSdk.Flush(TimeSpan.FromSeconds(5));
        }
        var errors = fake.Items("error").ToDictionary(e => (string)e["message"]!);
        Assert.Equal("a", (string?)errors["from a"]["tags"]!["request"]);
        Assert.Equal(new[] { "global", "in a" }, errors["from a"]["breadcrumbs"]!.AsArray().Select(b => (string)b!["message"]!));
        Assert.Equal(new[] { "global", "in b" }, errors["from b"]["breadcrumbs"]!.AsArray().Select(b => (string)b!["message"]!));
        Assert.Null(errors["outside"]["tags"]!["request"]);
    }

    [Fact]
    public void BreadcrumbsAreCapped()
    {
        var (fake, h) = FakeIngest.Init(o => o.MaxBreadcrumbs = 5);
        using (h)
        {
            for (var i = 0; i < 20; i++) VakaSdk.AddBreadcrumb("b" + i);
            VakaSdk.CaptureMessage("m");
        }
        var crumbs = fake.Items("error").Single()["breadcrumbs"]!.AsArray();
        Assert.Equal(5, crumbs.Count);
        Assert.Equal("b19", (string?)crumbs[4]!["message"]);
    }

    [Fact]
    public void BeforeSendCanDropAndChange()
    {
        var (fake, h) = FakeIngest.Init(o => o.BeforeSend = item =>
        {
            if ((string?)item["type"] == "event") return null;
            item["tags"] = new Dictionary<string, string> { ["scrubbed"] = "yes" };
            return item;
        });
        using (h)
        {
            VakaSdk.Track("dropped");
            VakaSdk.CaptureMessage("kept");
        }
        Assert.Empty(fake.Items("event"));
        Assert.Equal("yes", (string?)fake.Items("error").Single()["tags"]!["scrubbed"]);
    }

    [Fact]
    public void UnauthorizedStopsSending()
    {
        var (fake, h) = FakeIngest.Init();
        fake.Status = _ => HttpStatusCode.Unauthorized;
        using (h)
        {
            VakaSdk.CaptureMessage("one");
            VakaSdk.Flush(TimeSpan.FromSeconds(5));
            Assert.False(VakaSdk.IsEnabled);
            var count = fake.Requests.Count;
            VakaSdk.CaptureMessage("two");
            VakaSdk.Flush(TimeSpan.FromSeconds(1));
            Assert.Equal(count, fake.Requests.Count);
        }
    }

    [Fact]
    public void RetriesAfterServerUnavailable()
    {
        var (fake, h) = FakeIngest.Init();
        var calls = 0;
        fake.Status = _ => Interlocked.Increment(ref calls) == 1 ? HttpStatusCode.ServiceUnavailable : HttpStatusCode.Accepted;
        using (h)
        {
            VakaSdk.CaptureMessage("retry me");
            VakaSdk.Flush(TimeSpan.FromSeconds(5)); // first attempt: 503, kept queued
            VakaSdk.Flush(TimeSpan.FromSeconds(5)); // explicit flush retries
        }
        Assert.Equal(2, fake.Items("error").Count(e => (string?)e["message"] == "retry me"));
    }

    [Fact]
    public async Task FlagsAreEvaluatedAndAttached()
    {
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            var flags = await VakaSdk.LoadFlagsAsync("user-9", new Dictionary<string, object?> { ["plan"] = "pro" });
            Assert.True(flags.IsEnabled("new_checkout"));
            Assert.True(VakaSdk.IsFlagEnabled("dark_mode"));
            Assert.False(VakaSdk.IsFlagEnabled("beta"));
            Assert.False(VakaSdk.IsFlagEnabled("unknown"));
            Assert.Equal("one_page", VakaSdk.GetFlag("new_checkout"));
            VakaSdk.CaptureMessage("with flags");
        }
        var req = fake.Requests.Single(r => r.Path == "/ingest/v1/flags");
        Assert.Equal("user-9", (string?)req.Body["distinct_id"]);
        Assert.Equal("user-9", (string?)req.Body["user_id"]);
        Assert.Equal("test", (string?)req.Body["environment"]);
        Assert.Equal("pro", (string?)req.Body["properties"]!["plan"]);
        var err = fake.Items("error").Single();
        Assert.Equal("one_page", (string?)err["flags"]!["new_checkout"]);
        Assert.Equal("false", (string?)err["flags"]!["beta"]);
        Assert.Equal("true", (string?)fake.Items("session").Last()["flags"]!["dark_mode"]);
    }

    [Fact]
    public async Task FlagFailureReturnsEmpty()
    {
        var (fake, h) = FakeIngest.Init();
        fake.Status = r => r.RequestUri!.AbsolutePath.EndsWith("flags") ? HttpStatusCode.InternalServerError : HttpStatusCode.Accepted;
        using (h)
        {
            var flags = await VakaSdk.EvaluateFlagsAsync("d1");
            Assert.Empty(flags.Values);
        }
    }

    [Fact]
    public void ErrorsCarryActiveTraceAndMarkTheSpan()
    {
        var (fake, h) = FakeIngest.Init();
        using var source = new System.Diagnostics.ActivitySource("test");
        using var listener = new System.Diagnostics.ActivityListener
        {
            ShouldListenTo = _ => true,
            Sample = (ref System.Diagnostics.ActivityCreationOptions<System.Diagnostics.ActivityContext> _) => System.Diagnostics.ActivitySamplingResult.AllDataAndRecorded,
        };
        System.Diagnostics.ActivitySource.AddActivityListener(listener);
        string traceId;
        using (h)
        {
            using var act = source.StartActivity("work")!;
            traceId = act.TraceId.ToHexString();
            VakaSdk.CaptureException(new InvalidOperationException("in span"));
            Assert.Equal(true, act.GetTagItem(VakaSdk.ErrorCapturedAttribute));
        }
        Assert.Equal(traceId, (string?)fake.Items("error").Single()["trace_id"]);
    }

    [Fact]
    public void InAppRules()
    {
        var o = new VakaOptions();
        Assert.True(ExceptionConverter.IsInApp("MyApp.Cart", o));
        Assert.False(ExceptionConverter.IsInApp("System.Linq.Enumerable", o));
        Assert.False(ExceptionConverter.IsInApp("Microsoft.AspNetCore.Routing", o));
        o.InAppInclude.Add("Shop.");
        Assert.True(ExceptionConverter.IsInApp("Shop.Cart", o));
        Assert.False(ExceptionConverter.IsInApp("MyApp.Cart", o));
    }

    [Fact]
    public void ReleaseDefaultStripsCommitSuffix()
    {
        var r = VakaOptions.DefaultRelease();
        Assert.NotNull(r);
        Assert.DoesNotContain("+", r);
    }
}

public class NameTests
{
    [Theory]
    [InlineData("<DoWork>d__3", "DoWork")]
    [InlineData("<Main>b__0_0", "Main.lambda")]
    [InlineData("<<Main>$>b__0_1", "Main.lambda")]
    [InlineData("<Main>$", "Main")]
    [InlineData("<Run>g__Local|0_0", "Run.Local")]
    [InlineData("Checkout", "Checkout")]
    public void CompilerGeneratedNamesAreCleaned(string raw, string expected) => Assert.Equal(expected, ExceptionConverter.Clean(raw));

    [Fact]
    public void LambdaFramesUseTheEnclosingType()
    {
        Func<int> f = () => throw new InvalidOperationException();
        var ex = Assert.Throws<InvalidOperationException>(() => f());
        var frame = ExceptionConverter.Frames(ex, new VakaOptions())[0];
        Assert.Equal("Vaka.Tests.NameTests", frame["module"]);
        Assert.Equal("LambdaFramesUseTheEnclosingType.lambda", frame["function"]);
    }
}
