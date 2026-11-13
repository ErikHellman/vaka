namespace Vaka;

/// <summary>Severity of an error, message or breadcrumb.</summary>
public enum VakaLevel
{
    /// <summary>Debug detail (breadcrumbs only; sent as <c>info</c> on errors).</summary>
    Debug,
    /// <summary>Informational.</summary>
    Info,
    /// <summary>Warning.</summary>
    Warning,
    /// <summary>Error.</summary>
    Error,
    /// <summary>The process is going down.</summary>
    Fatal,
}

/// <summary>A breadcrumb: something that happened before an error.</summary>
/// <param name="Message">What happened.</param>
/// <param name="Category"><c>navigation</c>, <c>http</c>, <c>ui.click</c>, <c>console</c>, <c>lifecycle</c>, or your own.</param>
/// <param name="Level">Severity.</param>
/// <param name="Data">Optional structured data.</param>
public sealed record Breadcrumb(string Message, string Category = "default", VakaLevel Level = VakaLevel.Info, IReadOnlyDictionary<string, object?>? Data = null)
{
    /// <summary>When it happened.</summary>
    public DateTimeOffset Timestamp { get; init; } = DateTimeOffset.UtcNow;
}

/// <summary>
/// Context attached to everything sent while the scope is current: user, tags, breadcrumbs, flags,
/// request URL and contexts. <see cref="VakaSdk.PushScope"/> starts a child scope that flows with
/// async calls, so each request in a web server has its own.
/// </summary>
public sealed class Scope
{
    private readonly object _lock = new();
    private readonly Dictionary<string, string> _tags = new();
    private readonly Dictionary<string, string> _flags = new();
    private readonly Dictionary<string, object?> _contexts = new();
    private readonly LinkedList<Breadcrumb> _breadcrumbs = new();
    private string? _userId;
    private string? _url;
    private string? _transaction;
    internal int MaxBreadcrumbs = 100;

    /// <summary>Called on a copy of the scope just before an error is sent (frameworks fill in late request data).</summary>
    internal Action<Scope>? BeforeCapture;

    internal Scope() { }

    /// <summary>Identified user id (opt-in; only set it if your privacy policy allows).</summary>
    public string? UserId { get { lock (_lock) return _userId; } set { lock (_lock) _userId = value; } }

    /// <summary>URL of the current request or page, sent on errors.</summary>
    public string? Url { get { lock (_lock) return _url; } set { lock (_lock) _url = value; } }

    /// <summary>Name of the current operation (such as <c>GET /carts/{id}</c>), sent as the <c>transaction</c> tag.</summary>
    public string? Transaction { get { lock (_lock) return _transaction; } set { lock (_lock) _transaction = value; } }

    /// <summary>Set a tag sent with every error.</summary>
    public void SetTag(string key, string value) { lock (_lock) _tags[key] = value; }

    /// <summary>Remove a tag.</summary>
    public void RemoveTag(string key) { lock (_lock) _tags.Remove(key); }

    /// <summary>Set a named context object (such as <c>request</c>) sent with every error.</summary>
    public void SetContext(string key, object? value) { lock (_lock) _contexts[key] = value; }

    /// <summary>Record the value of a feature flag the code used, so health and conversion split by it.</summary>
    public void SetFlag(string key, string value) { lock (_lock) { if (_flags.Count < 32 || _flags.ContainsKey(key)) _flags[key] = value; } }

    /// <summary>Record all values of an evaluated flag set.</summary>
    public void SetFlags(VakaFlags flags) { foreach (var (k, v) in flags.Exposure()) SetFlag(k, v); }

    /// <summary>Add a breadcrumb. The oldest are dropped past the limit.</summary>
    public void AddBreadcrumb(Breadcrumb crumb)
    {
        lock (_lock)
        {
            _breadcrumbs.AddLast(crumb);
            while (_breadcrumbs.Count > Math.Clamp(MaxBreadcrumbs, 0, 100)) _breadcrumbs.RemoveFirst();
        }
    }

    /// <summary>Remove all breadcrumbs.</summary>
    public void ClearBreadcrumbs() { lock (_lock) _breadcrumbs.Clear(); }

    internal Scope Clone()
    {
        lock (_lock)
        {
            var s = new Scope { _userId = _userId, _url = _url, _transaction = _transaction, MaxBreadcrumbs = MaxBreadcrumbs, BeforeCapture = BeforeCapture };
            foreach (var kv in _tags) s._tags[kv.Key] = kv.Value;
            foreach (var kv in _flags) s._flags[kv.Key] = kv.Value;
            foreach (var kv in _contexts) s._contexts[kv.Key] = kv.Value;
            foreach (var b in _breadcrumbs) s._breadcrumbs.AddLast(b);
            return s;
        }
    }

    internal Dictionary<string, string> Tags { get { lock (_lock) return new(_tags); } }
    internal Dictionary<string, string> Flags { get { lock (_lock) return new(_flags); } }
    internal Dictionary<string, object?> Contexts { get { lock (_lock) return new(_contexts); } }
    internal List<Breadcrumb> Breadcrumbs { get { lock (_lock) return _breadcrumbs.ToList(); } }
}
