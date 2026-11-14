using System.Text;
using System.Text.Json;

namespace Vaka;

/// <summary>
/// Feedback a user wrote (a support form, in-app chat), sent with <see cref="VakaSdk.CaptureFeedbackAsync"/>.
/// </summary>
public sealed class VakaFeedback
{
    /// <summary>Most images one feedback item can carry.</summary>
    public const int MaxAttachments = 3;

    /// <summary>Create feedback with the user's message.</summary>
    /// <param name="message">What the user wrote (required). Longer than 5000 characters is cut.</param>
    public VakaFeedback(string message) => Message = message ?? throw new ArgumentNullException(nameof(message));

    /// <summary>What the user wrote.</summary>
    public string Message { get; }

    /// <summary>Contact email, only when the user gave it.</summary>
    public string? Email { get; init; }

    /// <summary>Contact name.</summary>
    public string? Name { get; init; }

    /// <summary>The error event this feedback is about (the id <see cref="VakaSdk.CaptureException"/> returned).</summary>
    public string? EventId { get; init; }

    /// <summary>The issue this feedback is about: its id or short id like <c>API-12</c>.</summary>
    public string? Issue { get; init; }

    /// <summary>Defaults to the current scope's user.</summary>
    public string? UserId { get; init; }

    /// <summary>The end user's session, when your app has one.</summary>
    public string? SessionId { get; init; }

    /// <summary>Page or screen the feedback is about. Defaults to the current scope's URL.</summary>
    public string? Url { get; init; }

    /// <summary>
    /// The end user's IP address. Feedback is rate limited per address, so without it all feedback from this
    /// server shares one limit.
    /// </summary>
    public string? IpAddress { get; init; }

    /// <summary>Tags, added to the current scope's.</summary>
    public IDictionary<string, string> Tags { get; init; } = new Dictionary<string, string>();

    /// <summary>Up to 3 PNG, JPEG or WebP images, 5 MB each.</summary>
    public IList<VakaFeedbackAttachment> Attachments { get; init; } = new List<VakaFeedbackAttachment>();
}

/// <summary>An image sent with feedback.</summary>
/// <param name="FileName">File name, e.g. <c>screenshot.png</c>.</param>
/// <param name="Data">The file's bytes.</param>
/// <param name="ContentType">Defaults to one guessed from the file name.</param>
public sealed record VakaFeedbackAttachment(string FileName, byte[] Data, string? ContentType = null);

/// <summary>The stored feedback's ids.</summary>
/// <param name="Id">The feedback id.</param>
/// <param name="ShortId">Short id like <c>API-F12</c>.</param>
public sealed record VakaFeedbackResult(string Id, string ShortId);

/// <summary>Thrown by <see cref="VakaSdk.CaptureFeedbackAsync"/> when the feedback was not stored.</summary>
public sealed class VakaFeedbackException : Exception
{
    /// <summary>Create the exception.</summary>
    public VakaFeedbackException(string message, int statusCode = 0, Exception? inner = null) : base(message, inner) => StatusCode = statusCode;

    /// <summary>The HTTP status, or 0 when nothing was sent. 429 means a rate limit was hit.</summary>
    public int StatusCode { get; }
}

internal static class FeedbackSender
{
    private const int MaxMessageChars = 5000;

    internal static string ContentTypeOf(string fileName)
    {
        var f = fileName.ToLowerInvariant();
        return f.EndsWith(".png", StringComparison.Ordinal) ? "image/png" : f.EndsWith(".webp", StringComparison.Ordinal) ? "image/webp" : "image/jpeg";
    }

    internal static byte[] Multipart(string boundary, byte[] json, IEnumerable<VakaFeedbackAttachment> files)
    {
        using var ms = new MemoryStream();
        void W(string s)
        {
            var b = Encoding.UTF8.GetBytes(s);
            ms.Write(b, 0, b.Length);
        }
        W($"--{boundary}\r\nContent-Disposition: form-data; name=\"feedback\"\r\nContent-Type: application/json\r\n\r\n");
        ms.Write(json, 0, json.Length);
        W("\r\n");
        foreach (var a in files)
        {
            var name = a.FileName.Replace("\"", "").Replace("\r", "").Replace("\n", "");
            W($"--{boundary}\r\nContent-Disposition: form-data; name=\"attachment\"; filename=\"{name}\"\r\nContent-Type: {a.ContentType ?? ContentTypeOf(a.FileName)}\r\n\r\n");
            ms.Write(a.Data, 0, a.Data.Length);
            W("\r\n");
        }
        W($"--{boundary}--\r\n");
        return ms.ToArray();
    }

    internal static async Task<VakaFeedbackResult> SendAsync(VakaSdk.Client c, Scope scope, VakaFeedback f, CancellationToken ct)
    {
        var message = f.Message.Trim();
        if (message.Length == 0) throw new VakaFeedbackException("feedback needs a message");
        if (f.Attachments.Count > VakaFeedback.MaxAttachments)
            throw new VakaFeedbackException($"at most {VakaFeedback.MaxAttachments} attachments per feedback");
        var si = new System.Globalization.StringInfo(message);
        if (si.LengthInTextElements > MaxMessageChars) message = si.SubstringByTextElements(0, MaxMessageChars);

        var id = VakaSdk.NewId();
        var item = new Dictionary<string, object?>
        {
            ["id"] = id,
            ["timestamp"] = VakaSdk.NowMs(),
            ["platform"] = "dotnet",
            ["source"] = "api",
            ["message"] = message,
            ["environment"] = c.Options.Environment,
            ["anon_id"] = c.AnonId,
        };
        void Put(string key, string? value)
        {
            if (!string.IsNullOrEmpty(value)) item[key] = value;
        }
        Put("release", c.Options.Release);
        Put("email", f.Email?.Trim());
        Put("name", f.Name?.Trim());
        Put("event_id", f.EventId);
        Put("issue", f.Issue);
        Put("user_id", f.UserId ?? scope.UserId);
        Put("session_id", f.SessionId);
        Put("url", f.Url ?? scope.Url);
        var tags = scope.Tags;
        foreach (var (k, v) in f.Tags) tags[k] = v;
        item["tags"] = tags;
        var flags = scope.Flags;
        if (flags.Count > 0) item["flags"] = flags;

        var json = JsonSerializer.SerializeToUtf8Bytes(item, Transport.Json);
        using var req = new HttpRequestMessage(HttpMethod.Post, c.Options.BaseUrl + "/ingest/v1/feedback");
        if (f.Attachments.Count == 0)
        {
            req.Content = new ByteArrayContent(json);
            req.Content.Headers.TryAddWithoutValidation("Content-Type", "application/json");
        }
        else
        {
            var boundary = "vaka-" + Guid.NewGuid().ToString("N");
            req.Content = new ByteArrayContent(Multipart(boundary, json, f.Attachments));
            req.Content.Headers.TryAddWithoutValidation("Content-Type", "multipart/form-data; boundary=" + boundary);
        }
        req.Headers.TryAddWithoutValidation("X-Vaka-Key", c.Options.Key);
        req.Headers.TryAddWithoutValidation("X-Vaka-Sdk", VakaSdk.SdkName + "/" + VakaSdk.SdkVersion);
        if (!string.IsNullOrEmpty(f.IpAddress)) req.Headers.TryAddWithoutValidation("X-Forwarded-For", f.IpAddress);

        HttpResponseMessage resp;
        try
        {
            resp = await c.Transport.Http.SendAsync(req, ct).ConfigureAwait(false);
        }
        catch (Exception e) when (e is not OperationCanceledException || !ct.IsCancellationRequested)
        {
            throw new VakaFeedbackException("vaka feedback: " + e.Message, 0, e);
        }
        using (resp)
        {
            var text = await resp.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
            JsonElement body = default;
            try
            {
                if (text.Length > 0) body = JsonDocument.Parse(text).RootElement;
            }
            catch (JsonException) { /* not JSON */ }
            string? Str(string name) =>
                body.ValueKind == JsonValueKind.Object && body.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;
            var status = (int)resp.StatusCode;
            if (status < 200 || status >= 300)
            {
                var err = Str("error");
                throw new VakaFeedbackException($"vaka feedback: HTTP {status}" + (err is null ? "" : ": " + err), status);
            }
            scope.AddBreadcrumb(new Breadcrumb("user feedback sent", "feedback"));
            return new VakaFeedbackResult(Str("id") ?? id, Str("short_id") ?? "");
        }
    }
}
