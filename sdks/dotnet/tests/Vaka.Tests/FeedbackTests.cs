using System.Net;
using System.Text;
using Xunit;

namespace Vaka.Tests;

public class FeedbackTests
{
    [Fact]
    public async Task SendsJsonWithScopeAndLinks()
    {
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            VakaSdk.SetUser("user-7");
            VakaSdk.SetTag("tenant", "acme");
            VakaSdk.ConfigureScope(s => s.SetFlag("new_export", "on"));
            var eventId = VakaSdk.CaptureMessage("export failed", VakaLevel.Error);
            var r = await VakaSdk.CaptureFeedbackAsync(new VakaFeedback("  The export button does nothing  ")
            {
                Email = "ann@example.com",
                EventId = eventId,
                Issue = "API-12",
                SessionId = "s-1",
                IpAddress = "203.0.113.9",
                Tags = { ["plan"] = "pro" },
            });
            Assert.Equal(new VakaFeedbackResult("fb-1", "API-F1"), r);

            var req = fake.Requests.Single(x => x.Path == "/ingest/v1/feedback");
            Assert.Equal("vk_test", req.Request.Headers.GetValues("X-Vaka-Key").Single());
            Assert.Equal("203.0.113.9", req.Request.Headers.GetValues("X-Forwarded-For").Single());
            var b = req.Body;
            Assert.Equal("The export button does nothing", (string?)b["message"]);
            Assert.Equal("ann@example.com", (string?)b["email"]);
            Assert.Equal(eventId, (string?)b["event_id"]);
            Assert.Equal("API-12", (string?)b["issue"]);
            Assert.Equal("s-1", (string?)b["session_id"]);
            Assert.Equal("user-7", (string?)b["user_id"]);
            Assert.Equal("api", (string?)b["source"]);
            Assert.Equal("dotnet", (string?)b["platform"]);
            Assert.Equal("1.2.3", (string?)b["release"]);
            Assert.Equal("acme", (string?)b["tags"]!["tenant"]);
            Assert.Equal("pro", (string?)b["tags"]!["plan"]);
            Assert.Equal("on", (string?)b["flags"]!["new_export"]);
        }
    }

    [Fact]
    public async Task SendsAttachmentsAsMultipart()
    {
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            var png = Encoding.Latin1.GetBytes("\u0089PNG\r\n\u001a\n0000");
            await VakaSdk.CaptureFeedbackAsync(new VakaFeedback("with a screenshot")
            {
                Attachments = { new VakaFeedbackAttachment("shot.png", png) },
            });
            var req = fake.Requests.Single(x => x.Path == "/ingest/v1/feedback");
            var ct = req.Request.Content!.Headers.ContentType!;
            Assert.Equal("multipart/form-data", ct.MediaType);
            var boundary = ct.Parameters.Single(p => p.Name == "boundary").Value!;
            var raw = (string)req.Body["raw"]!;
            Assert.StartsWith($"--{boundary}\r\nContent-Disposition: form-data; name=\"feedback\"", raw);
            Assert.Contains("\"message\":\"with a screenshot\"", raw);
            Assert.Contains("filename=\"shot.png\"\r\nContent-Type: image/png\r\n\r\n" + Encoding.Latin1.GetString(png) + "\r\n", raw);
            Assert.EndsWith($"--{boundary}--\r\n", raw);
        }
    }

    [Fact]
    public async Task RejectsInvalidFeedbackAndReportsServerErrors()
    {
        await Assert.ThrowsAsync<VakaFeedbackException>(() => VakaSdk.CaptureFeedbackAsync(new VakaFeedback("hi")));
        var (fake, h) = FakeIngest.Init();
        using (h)
        {
            await Assert.ThrowsAsync<VakaFeedbackException>(() => VakaSdk.CaptureFeedbackAsync(new VakaFeedback("   ")));
            var many = new VakaFeedback("x");
            for (var i = 0; i < 4; i++) many.Attachments.Add(new VakaFeedbackAttachment("a.png", new byte[1]));
            await Assert.ThrowsAsync<VakaFeedbackException>(() => VakaSdk.CaptureFeedbackAsync(many));
            fake.Status = r => r.RequestUri!.AbsolutePath == "/ingest/v1/feedback" ? (HttpStatusCode)429 : HttpStatusCode.Accepted;
            var e = await Assert.ThrowsAsync<VakaFeedbackException>(() => VakaSdk.CaptureFeedbackAsync(new VakaFeedback("again")));
            Assert.Equal(429, e.StatusCode);
            Assert.Contains("too much feedback", e.Message);
        }
    }
}
