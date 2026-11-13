using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace Vaka;

/// <summary>
/// Entry point of the Vaka SDK. Call <see cref="Init(VakaOptions)"/> once at startup and keep the
/// returned handle until exit; disposing it ends the session and flushes queued data.
/// Every method is a no-op before <c>Init</c> or when no ingest key is configured.
/// </summary>
public static class VakaSdk
{
    /// <summary>SDK name sent with every batch.</summary>
    public const string SdkName = "vaka-dotnet";

    /// <summary>SDK version sent with every batch.</summary>
    public static readonly string SdkVersion =
        typeof(VakaSdk).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion?.Split('+')[0] ?? "0.0.0";

    internal const string CapturedKey = "vaka.captured";

    /// <summary>
    /// OpenTelemetry attribute set on spans and log records whose error the SDK already reported, so the
    /// server does not create a duplicate error from the OTLP data.
    /// </summary>
    public const string ErrorCapturedAttribute = "vaka.error_captured";
    private const string Platform = "dotnet";

    private static readonly object InitLock = new();
    private static readonly AsyncLocal<Scope?> CurrentScope = new();
    private static Client? _client;
    private static Scope _root = new();

    /// <summary>True after a successful <c>Init</c> with a key and endpoint.</summary>
    public static bool IsEnabled => _client is { Transport.Disabled: false };

    /// <summary>The options the SDK was initialised with, or <c>null</c>.</summary>
    public static VakaOptions? Options => _client?.Options;

    /// <summary>Initialise the SDK with options from the environment, changed by <paramref name="configure"/>.</summary>
    public static IDisposable Init(Action<VakaOptions> configure)
    {
        var o = new VakaOptions();
        configure(o);
        return Init(o);
    }

    /// <summary>Initialise the SDK. Calling it again replaces the previous client (and ends its session).</summary>
    public static IDisposable Init(VakaOptions options)
    {
        lock (InitLock)
        {
            _client?.Shutdown();
            _client = null;
            _root = new Scope { MaxBreadcrumbs = options.MaxBreadcrumbs };
            CurrentScope.Value = null;
            if (!options.IsEnabled)
            {
                if (options.Debug) Console.Error.WriteLine("[vaka] no key or endpoint configured; SDK disabled");
                return new Handle(null);
            }
            var c = new Client(options);
            _client = c;
            if (!string.IsNullOrEmpty(options.ServiceName)) _root.SetTag("service", options.ServiceName!);
            if (options.CaptureUnhandledExceptions) AppDomain.CurrentDomain.UnhandledException += c.OnUnhandled;
            if (options.CaptureUnobservedTaskExceptions) TaskScheduler.UnobservedTaskException += c.OnUnobserved;
            AppDomain.CurrentDomain.ProcessExit += c.OnProcessExit;
            if (options.AutoSessions) c.StartSession();
            return new Handle(c);
        }
    }

    /// <summary>End the session, send everything queued (waiting up to <see cref="VakaOptions.ShutdownTimeout"/>) and stop.</summary>
    public static void Close()
    {
        lock (InitLock)
        {
            _client?.Shutdown();
            _client = null;
        }
    }

    /// <summary>Send everything queued and wait up to <paramref name="timeout"/> (default 2 s). Returns false on timeout.</summary>
    public static bool Flush(TimeSpan? timeout = null) => _client?.Transport.Flush(timeout ?? TimeSpan.FromSeconds(2)) ?? false;

    /// <summary>Report an exception. Returns the event id, or <c>null</c> when nothing was sent.</summary>
    /// <param name="exception">The exception, with its inner exceptions.</param>
    /// <param name="level">Severity; <see cref="VakaLevel.Fatal"/> marks it as a crash.</param>
    /// <param name="handled">False for exceptions your code did not catch.</param>
    /// <param name="configure">Change the scope for this event only (tags, fingerprint context, flags).</param>
    public static string? CaptureException(Exception exception, VakaLevel level = VakaLevel.Error, bool handled = true, Action<Scope>? configure = null)
    {
        var c = _client;
        if (c is null || exception is null) return null;
        try
        {
            if (exception.Data.Contains(CapturedKey)) return exception.Data[CapturedKey] as string;
        }
        catch { /* read-only Data */ }
        var id = c.SendError(exception, null, level, handled, configure);
        try { exception.Data[CapturedKey] = id; } catch { /* read-only Data */ }
        return id;
    }

    /// <summary>Report a message without an exception.</summary>
    public static string? CaptureMessage(string message, VakaLevel level = VakaLevel.Info, Action<Scope>? configure = null) =>
        _client?.SendError(null, message, level, handled: true, configure);

    /// <summary>Record a breadcrumb, sent with the next error from this scope.</summary>
    public static void AddBreadcrumb(string message, string category = "default", VakaLevel level = VakaLevel.Info, IReadOnlyDictionary<string, object?>? data = null)
    {
        if (_client is null) return;
        Current.AddBreadcrumb(new Breadcrumb(message, category, level, data));
    }

    /// <summary>Set a tag on the current scope.</summary>
    public static void SetTag(string key, string value) { if (_client is not null) Current.SetTag(key, value); }

    /// <summary>Set (or clear with <c>null</c>) the identified user id. Opt-in: only if your privacy policy allows.</summary>
    public static void SetUser(string? userId) { if (_client is not null) Current.UserId = userId; }

    /// <summary>Track a custom analytics event.</summary>
    public static void Track(string name, IReadOnlyDictionary<string, object?>? props = null) => _client?.SendEvent(name, props);

    /// <summary>Change the current scope.</summary>
    public static void ConfigureScope(Action<Scope> configure) { if (_client is not null) configure(Current); }

    /// <summary>
    /// Start a child scope that copies the current one and flows with async calls. Dispose it to return
    /// to the parent. Web frameworks push one per request.
    /// </summary>
    public static IDisposable PushScope()
    {
        var parent = CurrentScope.Value;
        CurrentScope.Value = (parent ?? _root).Clone();
        return new ScopeHandle(parent);
    }

    /// <summary>
    /// Evaluate feature flags for one person with <c>POST /ingest/v1/flags</c>. The values are recorded on
    /// the current scope (so errors, events and the session carry them) unless <paramref name="recordExposure"/> is false.
    /// Returns <see cref="VakaFlags.Empty"/> when the SDK is disabled or the request fails.
    /// </summary>
    public static async Task<VakaFlags> EvaluateFlagsAsync(string distinctId, string? userId = null, IReadOnlyDictionary<string, object?>? properties = null,
        bool recordExposure = true, CancellationToken cancellationToken = default)
    {
        var c = _client;
        if (c is null) return VakaFlags.Empty;
        var flags = await c.EvaluateFlagsAsync(distinctId, userId, properties, cancellationToken).ConfigureAwait(false);
        if (recordExposure) Current.SetFlags(flags);
        return flags;
    }

    /// <summary>
    /// Evaluate flags for this process (the session's anonymous id, or <paramref name="userId"/>) and keep them
    /// on the root scope for <see cref="IsFlagEnabled"/> and <see cref="GetFlag"/>.
    /// </summary>
    public static async Task<VakaFlags> LoadFlagsAsync(string? userId = null, IReadOnlyDictionary<string, object?>? properties = null, CancellationToken cancellationToken = default)
    {
        var c = _client;
        if (c is null) return VakaFlags.Empty;
        var flags = await c.EvaluateFlagsAsync(userId ?? c.AnonId, userId, properties, cancellationToken).ConfigureAwait(false);
        c.Flags = flags;
        _root.SetFlags(flags);
        return flags;
    }

    /// <summary>True when a flag loaded with <see cref="LoadFlagsAsync"/> is on.</summary>
    public static bool IsFlagEnabled(string key) => _client?.Flags.IsEnabled(key) ?? false;

    /// <summary>A flag loaded with <see cref="LoadFlagsAsync"/>: its variant, <c>"true"</c> or <c>"false"</c>.</summary>
    public static string? GetFlag(string key, string? fallback = null) => _client?.Flags.GetVariant(key, fallback) ?? fallback;

    internal static Scope Current => CurrentScope.Value ?? _root;

    internal static Client? ClientForTests => _client;

    private sealed class Handle(Client? client) : IDisposable
    {
        public void Dispose()
        {
            lock (InitLock)
            {
                if (client is not null && ReferenceEquals(_client, client))
                {
                    client.Shutdown();
                    _client = null;
                }
            }
        }
    }

    private sealed class ScopeHandle(Scope? parent) : IDisposable
    {
        public void Dispose() => CurrentScope.Value = parent;
    }

    internal static string LevelName(VakaLevel l) => l switch
    {
        VakaLevel.Debug => "debug",
        VakaLevel.Info => "info",
        VakaLevel.Warning => "warning",
        VakaLevel.Error => "error",
        _ => "fatal",
    };

    internal static long NowMs() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

    internal static string NewId() => Guid.NewGuid().ToString();

    /// <summary>One initialised SDK instance.</summary>
    internal sealed class Client
    {
        public readonly VakaOptions Options;
        public readonly Transport Transport;
        public readonly string AnonId = NewId();
        public VakaFlags Flags = VakaFlags.Empty;
        private readonly object _sessionLock = new();
        private Session? _session;
        private int _shutdown;

        private sealed class Session
        {
            public string Id = NewId();
            public long StartedMs = NowMs();
            public Stopwatch Clock = Stopwatch.StartNew();
            public int Errors;
        }

        public Client(VakaOptions options)
        {
            Options = options;
            Transport = new Transport(options);
        }

        public string? SessionId { get { lock (_sessionLock) return _session?.Id; } }

        private Dictionary<string, object?> Common(string type, Scope scope)
        {
            var m = new Dictionary<string, object?> { ["type"] = type, ["platform"] = Platform, ["environment"] = Options.Environment, ["anon_id"] = AnonId };
            if (!string.IsNullOrEmpty(Options.Release)) m["release"] = Options.Release;
            if (scope.UserId is { } u) m["user_id"] = u;
            if (SessionId is { } sid) m["session_id"] = sid;
            var flags = scope.Flags;
            if (flags.Count > 0) m["flags"] = flags;
            return m;
        }

        public void StartSession()
        {
            Dictionary<string, object?> item;
            lock (_sessionLock)
            {
                _session = new Session();
                item = SessionItem(_session, "ok");
            }
            Enqueue(item);
        }

        private Dictionary<string, object?> SessionItem(Session s, string status)
        {
            var m = Common("session", _root);
            m["session_id"] = s.Id;
            m["status"] = status;
            m["started"] = s.StartedMs;
            m["timestamp"] = NowMs();
            m["duration_ms"] = s.Clock.ElapsedMilliseconds;
            m["errors"] = s.Errors;
            return m;
        }

        public void EndSession(string status)
        {
            Dictionary<string, object?> item;
            lock (_sessionLock)
            {
                if (_session is null) return;
                if (status == "exited" && _session.Errors > 0) status = "errored";
                item = SessionItem(_session, status);
                _session = null;
            }
            Enqueue(item);
        }

        private void Enqueue(Dictionary<string, object?> item)
        {
            if (Options.BeforeSend is { } hook)
            {
                try { item = hook(item)!; }
                catch (Exception e) { Transport.Log("BeforeSend threw: " + e.Message); }
                if (item is null) return;
            }
            Transport.Enqueue(item);
        }

        public string? SendError(Exception? ex, string? message, VakaLevel level, bool handled, Action<Scope>? configure)
        {
            var scope = Current;
            if (configure is not null || scope.BeforeCapture is not null)
            {
                var hook = scope.BeforeCapture;
                scope = scope.Clone();
                try { hook?.Invoke(scope); } catch (Exception e) { Transport.Log("scope hook threw: " + e.Message); }
                configure?.Invoke(scope);
            }
            var m = Common("error", scope);
            var id = NewId();
            m["event_id"] = id;
            m["timestamp"] = NowMs();
            m["level"] = LevelName(level == VakaLevel.Debug ? VakaLevel.Info : level);
            m["fatal"] = level == VakaLevel.Fatal;
            m["handled"] = handled;
            if (ex is not null)
            {
                m["exceptions"] = ExceptionConverter.Convert(ex, Options);
                if (message is not null) m["message"] = message;
            }
            else
            {
                m["message"] = message ?? "";
                m["exceptions"] = new List<object>();
            }
            m["breadcrumbs"] = scope.Breadcrumbs.Select(b =>
            {
                var d = new Dictionary<string, object?>
                {
                    ["timestamp"] = b.Timestamp.ToUnixTimeMilliseconds(),
                    ["category"] = b.Category,
                    ["level"] = LevelName(b.Level),
                    ["message"] = b.Message,
                };
                if (b.Data is not null) d["data"] = b.Data;
                return d;
            }).ToList();
            var tags = scope.Tags;
            if (scope.Transaction is { } tx) tags["transaction"] = tx;
            m["tags"] = tags;
            if (scope.UserId is { } u) m["user"] = new Dictionary<string, object?> { ["id"] = u };
            if (scope.Url is { } url) m["url"] = url;
            var contexts = scope.Contexts;
            contexts["runtime"] = new Dictionary<string, object?> { ["name"] = ".NET", ["version"] = RuntimeInformation.FrameworkDescription };
            contexts["os"] = new Dictionary<string, object?> { ["name"] = OsName(), ["version"] = RuntimeInformation.OSDescription, ["arch"] = RuntimeInformation.OSArchitecture.ToString().ToLowerInvariant() };
            m["contexts"] = contexts;
            m["device"] = new Dictionary<string, object?> { ["os_name"] = OsName(), ["os_version"] = System.Environment.OSVersion.Version.ToString(), ["app_version"] = Options.Release };
            // Link errors to the OpenTelemetry trace they happened in.
            if (Activity.Current is { } act && act.TraceId != default)
            {
                m["trace_id"] = act.TraceId.ToHexString();
                m["span_id"] = act.SpanId.ToHexString();
                // Tells the server not to derive a second error from this span's exception events.
                act.SetTag(ErrorCapturedAttribute, true);
            }
            lock (_sessionLock) { if (_session is not null) _session.Errors++; }
            Enqueue(m);
            return id;
        }

        public void SendEvent(string name, IReadOnlyDictionary<string, object?>? props)
        {
            var m = Common("event", Current);
            m["name"] = name;
            m["timestamp"] = NowMs();
            if (props is not null && props.Count > 0) m["props"] = props.Take(64).ToDictionary(kv => kv.Key, kv => kv.Value);
            Enqueue(m);
        }

        public async Task<VakaFlags> EvaluateFlagsAsync(string distinctId, string? userId, IReadOnlyDictionary<string, object?>? properties, CancellationToken ct)
        {
            try
            {
                var body = new Dictionary<string, object?> { ["distinct_id"] = distinctId, ["environment"] = Options.Environment };
                if (userId is not null) body["user_id"] = userId;
                if (properties is not null) body["properties"] = properties;
                using var req = new HttpRequestMessage(HttpMethod.Post, Options.BaseUrl + "/ingest/v1/flags")
                {
                    Content = new StringContent(JsonSerializer.Serialize(body, Transport.Json), Encoding.UTF8, "application/json"),
                };
                req.Headers.Add("X-Vaka-Key", Options.Key);
                using var resp = await Transport.Http.SendAsync(req, ct).ConfigureAwait(false);
                var text = await resp.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
                if (!resp.IsSuccessStatusCode)
                {
                    Transport.Log($"flags returned {(int)resp.StatusCode}: {text}");
                    return VakaFlags.Empty;
                }
                return VakaFlags.Parse(text);
            }
            catch (Exception e) when (e is not OperationCanceledException || !ct.IsCancellationRequested)
            {
                Transport.Log("flags request failed: " + e.Message);
                return VakaFlags.Empty;
            }
        }

        public void OnUnhandled(object? sender, UnhandledExceptionEventArgs e)
        {
            if (e.ExceptionObject is not Exception ex) return;
            CaptureException(ex, e.IsTerminating ? VakaLevel.Fatal : VakaLevel.Error, handled: false, s => s.SetTag("mechanism", "AppDomain.UnhandledException"));
            if (e.IsTerminating)
            {
                EndSession("crashed");
                Transport.Flush(Options.ShutdownTimeout);
            }
        }

        public void OnUnobserved(object? sender, UnobservedTaskExceptionEventArgs e)
        {
            var ex = e.Exception.InnerExceptions.Count == 1 ? e.Exception.InnerExceptions[0] : e.Exception;
            CaptureException(ex, VakaLevel.Error, handled: false, s => s.SetTag("mechanism", "UnobservedTaskException"));
        }

        public void OnProcessExit(object? sender, EventArgs e) => Shutdown();

        public void Shutdown()
        {
            if (Interlocked.Exchange(ref _shutdown, 1) == 1) return;
            AppDomain.CurrentDomain.UnhandledException -= OnUnhandled;
            TaskScheduler.UnobservedTaskException -= OnUnobserved;
            AppDomain.CurrentDomain.ProcessExit -= OnProcessExit;
            EndSession("exited");
            Transport.Stop(Options.ShutdownTimeout);
        }

        private static string OsName() =>
            OperatingSystem.IsWindows() ? "Windows" : OperatingSystem.IsMacOS() ? "macOS" : OperatingSystem.IsLinux() ? "Linux" : RuntimeInformation.OSDescription;
    }
}
