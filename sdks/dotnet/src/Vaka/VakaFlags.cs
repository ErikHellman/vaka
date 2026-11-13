using System.Text.Json;

namespace Vaka;

/// <summary>
/// Flag values evaluated by the Vaka server for one person. A value is <c>true</c>/<c>false</c> for
/// plain flags or the variant key (such as <c>"one_page"</c>) for flags with variants.
/// </summary>
public sealed class VakaFlags
{
    private readonly Dictionary<string, object> _values;

    /// <summary>An empty set: every flag is off.</summary>
    public static readonly VakaFlags Empty = new(new Dictionary<string, object>());

    internal VakaFlags(Dictionary<string, object> values) => _values = values;

    /// <summary>All flag keys and values.</summary>
    public IReadOnlyDictionary<string, object> Values => _values;

    /// <summary>True when the flag is on (a variant counts as on). Unknown flags are off.</summary>
    public bool IsEnabled(string key) => _values.TryGetValue(key, out var v) && v switch
    {
        bool b => b,
        string s => s.Length > 0 && s != "false",
        _ => false,
    };

    /// <summary>The variant key, <c>"true"</c> or <c>"false"</c>; <paramref name="fallback"/> for unknown flags.</summary>
    public string? GetVariant(string key, string? fallback = null) => _values.TryGetValue(key, out var v) ? Stringify(v) : fallback;

    /// <summary>Values as strings, the way they are sent on items (<c>flags</c> field).</summary>
    public IEnumerable<KeyValuePair<string, string>> Exposure() => _values.Select(kv => new KeyValuePair<string, string>(kv.Key, Stringify(kv.Value)));

    private static string Stringify(object v) => v switch { bool b => b ? "true" : "false", _ => v.ToString() ?? "" };

    internal static VakaFlags Parse(string json)
    {
        using var doc = JsonDocument.Parse(json);
        var values = new Dictionary<string, object>();
        if (doc.RootElement.TryGetProperty("flags", out var flags) && flags.ValueKind == JsonValueKind.Object)
        {
            foreach (var p in flags.EnumerateObject())
            {
                values[p.Name] = p.Value.ValueKind switch
                {
                    JsonValueKind.True => true,
                    JsonValueKind.False => false,
                    JsonValueKind.String => p.Value.GetString() ?? "",
                    _ => p.Value.ToString(),
                };
            }
        }
        return new VakaFlags(values);
    }
}
