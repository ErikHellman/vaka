using System.IO.Compression;
using System.Net;
using System.Text.Json.Nodes;

[assembly: Xunit.CollectionBehavior(DisableTestParallelization = true)]

namespace Vaka.Tests;

/// <summary>Records requests the SDK makes and answers them like a Vaka server.</summary>
public sealed class FakeIngest : HttpMessageHandler
{
    public readonly List<(string Path, JsonNode Body, HttpRequestMessage Request)> Requests = new();
    public Func<HttpRequestMessage, HttpStatusCode> Status = _ => HttpStatusCode.Accepted;
    public string FlagsResponse = """{"flags": {"new_checkout": "one_page", "dark_mode": true, "beta": false}}""";

    protected override HttpResponseMessage Send(HttpRequestMessage request, CancellationToken cancellationToken) =>
        SendAsync(request, cancellationToken).GetAwaiter().GetResult();

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        var bytes = request.Content is null ? Array.Empty<byte>() : await request.Content.ReadAsByteArrayAsync(cancellationToken);
        if (request.Content?.Headers.ContentEncoding.Contains("gzip") == true)
        {
            using var gz = new GZipStream(new MemoryStream(bytes), CompressionMode.Decompress);
            using var ms = new MemoryStream();
            gz.CopyTo(ms);
            bytes = ms.ToArray();
        }
        var path = request.RequestUri!.AbsolutePath;
        var multipart = request.Content?.Headers.ContentType?.MediaType == "multipart/form-data";
        JsonNode body = path.StartsWith("/ingest/otlp") ? new JsonObject { ["bytes"] = bytes.Length }
            : multipart ? new JsonObject { ["raw"] = System.Text.Encoding.Latin1.GetString(bytes) }
            : JsonNode.Parse(bytes)!;
        lock (Requests) Requests.Add((path, body, request));
        var status = Status(request);
        var text = path == "/ingest/v1/flags" ? FlagsResponse
            : path == "/ingest/v1/feedback" ? (status == HttpStatusCode.Accepted ? """{"id": "fb-1", "short_id": "API-F1"}""" : """{"error": "too much feedback"}""")
            : """{"accepted": 1, "rejected": []}""";
        return new HttpResponseMessage(status) { Content = new StringContent(text) };
    }

    public List<JsonObject> Items(string? type = null)
    {
        lock (Requests)
        {
            return Requests.Where(r => r.Path == "/ingest/v1/batch")
                .SelectMany(r => r.Body["items"]!.AsArray().Select(i => i!.AsObject()))
                .Where(i => type is null || (string?)i["type"] == type)
                .ToList();
        }
    }

    public static (FakeIngest Fake, IDisposable Handle) Init(Action<VakaOptions>? configure = null)
    {
        var fake = new FakeIngest();
        var handle = VakaSdk.Init(o =>
        {
            o.Key = "vk_test";
            o.Endpoint = "http://vaka.test/";
            o.Release = "1.2.3";
            o.Environment = "test";
            o.ServiceName = "tests";
            o.HttpMessageHandler = fake;
            o.FlushInterval = TimeSpan.FromMilliseconds(50);
            o.CaptureUnobservedTaskExceptions = false;
            o.InAppInclude.Add("Vaka.Tests");
            configure?.Invoke(o);
        });
        return (fake, handle);
    }
}
