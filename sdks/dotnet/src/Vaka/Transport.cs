using System.Collections.Concurrent;
using System.IO.Compression;
using System.Net.Http.Headers;
using System.Text.Json;

namespace Vaka;

/// <summary>Batches items on a background thread and posts them to <c>/ingest/v1/batch</c>.</summary>
internal sealed class Transport : IDisposable
{
    private const int MaxBatch = 1000;
    private const int MaxQueued = 10_000;

    private readonly VakaOptions _options;
    private readonly HttpClient _http;
    private readonly BlockingCollection<Msg> _inbox = new(new ConcurrentQueue<Msg>());
    private readonly Thread _worker;
    private readonly List<Dictionary<string, object?>> _queue = new();
    private TimeSpan _backoff = TimeSpan.Zero;
    private DateTime _nextSend = DateTime.MinValue;
    private volatile bool _disabled;

    internal static readonly JsonSerializerOptions Json = new() { DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.Never };

    private abstract record Msg;
    private sealed record ItemMsg(Dictionary<string, object?> Item) : Msg;
    private sealed record FlushMsg(ManualResetEventSlim Done) : Msg;
    private sealed record StopMsg(ManualResetEventSlim Done) : Msg;

    public Transport(VakaOptions options)
    {
        _options = options;
        _http = options.HttpMessageHandler is { } h ? new HttpClient(h, disposeHandler: false) : new HttpClient();
        _http.Timeout = TimeSpan.FromSeconds(15);
        _worker = new Thread(Run) { IsBackground = true, Name = "vaka-transport" };
        _worker.Start();
    }

    public HttpClient Http => _http;

    public bool Disabled => _disabled;

    public void Enqueue(Dictionary<string, object?> item)
    {
        if (_disabled || _inbox.IsAddingCompleted) return;
        try { _inbox.Add(new ItemMsg(item)); } catch (InvalidOperationException) { }
    }

    /// <summary>Send everything queued; wait up to <paramref name="timeout"/>.</summary>
    public bool Flush(TimeSpan timeout)
    {
        if (_inbox.IsAddingCompleted) return false;
        using var done = new ManualResetEventSlim();
        try { _inbox.Add(new FlushMsg(done)); } catch (InvalidOperationException) { return false; }
        return done.Wait(timeout);
    }

    public void Dispose() => Stop(_options.ShutdownTimeout);

    public void Stop(TimeSpan timeout)
    {
        if (_inbox.IsAddingCompleted) return;
        var done = new ManualResetEventSlim();
        try { _inbox.Add(new StopMsg(done)); _inbox.CompleteAdding(); } catch (InvalidOperationException) { return; }
        done.Wait(timeout);
    }

    private void Run()
    {
        while (true)
        {
            var wait = _queue.Count == 0 ? _options.FlushInterval : Max(_nextSend - DateTime.UtcNow, TimeSpan.FromMilliseconds(10));
            Msg? msg = null;
            try { _inbox.TryTake(out msg, Max(wait, TimeSpan.FromMilliseconds(10))); }
            catch (ObjectDisposedException) { return; }
            switch (msg)
            {
                case ItemMsg m:
                    if (_queue.Count < MaxQueued) _queue.Add(m.Item);
                    if (_nextSend == DateTime.MinValue) _nextSend = DateTime.UtcNow + _options.FlushInterval;
                    var urgent = m.Item.TryGetValue("type", out var t) && (t as string) == "error";
                    if (((urgent || _queue.Count >= 50) && _backoff == TimeSpan.Zero) || DateTime.UtcNow >= _nextSend) SendQueued();
                    break;
                case FlushMsg f:
                    SendQueued();
                    f.Done.Set();
                    break;
                case StopMsg s:
                    SendQueued();
                    s.Done.Set();
                    return;
                default:
                    if (_queue.Count > 0 && DateTime.UtcNow >= _nextSend) SendQueued();
                    if (_inbox.IsCompleted) return;
                    break;
            }
        }
    }

    private static TimeSpan Max(TimeSpan a, TimeSpan b) => a > b ? a : b;

    private void SendQueued()
    {
        while (_queue.Count > 0 && !_disabled)
        {
            var n = Math.Min(_queue.Count, MaxBatch);
            var batch = _queue.GetRange(0, n);
            var result = Post(batch);
            switch (result)
            {
                case Result.Sent:
                case Result.Dropped:
                    _queue.RemoveRange(0, n);
                    _backoff = TimeSpan.Zero;
                    break;
                case Result.TooLarge when n > 1:
                    // Split: send the first half on the next loop iteration.
                    var half = n / 2;
                    var first = _queue.GetRange(0, half);
                    var r = Post(first);
                    if (r is Result.Sent or Result.Dropped) { _queue.RemoveRange(0, half); continue; }
                    goto case Result.Retry;
                case Result.TooLarge:
                    _queue.RemoveRange(0, n);
                    break;
                case Result.Retry:
                    _backoff = _backoff == TimeSpan.Zero ? TimeSpan.FromSeconds(5) : TimeSpan.FromTicks(Math.Min(_backoff.Ticks * 2, TimeSpan.FromMinutes(5).Ticks));
                    _nextSend = DateTime.UtcNow + _backoff;
                    if (_queue.Count > MaxQueued) _queue.RemoveRange(0, _queue.Count - MaxQueued);
                    return;
                case Result.Unauthorized:
                    _disabled = true;
                    _queue.Clear();
                    return;
            }
        }
        _nextSend = DateTime.MinValue;
    }

    private enum Result { Sent, Dropped, Retry, TooLarge, Unauthorized }

    private Result Post(List<Dictionary<string, object?>> items)
    {
        try
        {
            var body = new Dictionary<string, object?>
            {
                ["sdk"] = new Dictionary<string, object?> { ["name"] = VakaSdk.SdkName, ["version"] = VakaSdk.SdkVersion },
                ["items"] = items,
            };
            using var req = new HttpRequestMessage(HttpMethod.Post, _options.BaseUrl + "/ingest/v1/batch")
            {
                Content = Gzip(JsonSerializer.SerializeToUtf8Bytes(body, Json)),
            };
            req.Headers.Add("X-Vaka-Key", _options.Key);
            req.Headers.Add("X-Vaka-Sdk", $"{VakaSdk.SdkName}/{VakaSdk.SdkVersion}");
            using var resp = _http.Send(req);
            var code = (int)resp.StatusCode;
            if (_options.Debug || code >= 400)
            {
                var text = SafeRead(resp);
                if (_options.Debug || code != 429) Log($"ingest returned {code} for {items.Count} items: {text}");
            }
            return code switch
            {
                >= 200 and < 300 => Result.Sent,
                401 or 403 => Result.Unauthorized,
                413 => Result.TooLarge,
                429 or >= 500 => Result.Retry,
                _ => Result.Dropped,
            };
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or IOException or OperationCanceledException)
        {
            if (_options.Debug) Log($"send failed: {e.Message}");
            return Result.Retry;
        }
        catch (Exception e)
        {
            Log($"dropping {items.Count} items: {e.Message}");
            return Result.Dropped;
        }
    }

    internal static ByteArrayContent Gzip(byte[] json)
    {
        using var ms = new MemoryStream();
        using (var gz = new GZipStream(ms, CompressionLevel.Fastest, leaveOpen: true)) gz.Write(json);
        var content = new ByteArrayContent(ms.ToArray());
        content.Headers.ContentType = new MediaTypeHeaderValue("application/json");
        content.Headers.ContentEncoding.Add("gzip");
        return content;
    }

    private static string SafeRead(HttpResponseMessage resp)
    {
        try
        {
            using var s = resp.Content.ReadAsStream();
            using var r = new StreamReader(s);
            var t = r.ReadToEnd();
            return t.Length > 500 ? t[..500] : t;
        }
        catch { return ""; }
    }

    internal void Log(string message)
    {
        if (_options.Debug) Console.Error.WriteLine("[vaka] " + message);
    }
}
