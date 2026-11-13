using System.Diagnostics;
using System.Reflection;

namespace Vaka;

/// <summary>Turns .NET exceptions into Vaka <c>exceptions</c> entries (cause chain outermost first, frames crash site first).</summary>
internal static class ExceptionConverter
{
    private const int MaxChain = 10;
    private const int MaxFrames = 200;

    public static List<Dictionary<string, object?>> Convert(Exception ex, VakaOptions options)
    {
        var list = new List<Dictionary<string, object?>>();
        var seen = new HashSet<Exception>(ReferenceEqualityComparer.Instance);
        Exception? cur = ex;
        while (cur is not null && list.Count < MaxChain && seen.Add(cur))
        {
            list.Add(ConvertOne(cur, options));
            // AggregateException: follow the first inner exception (the usual single cause of a faulted task).
            cur = cur is AggregateException agg && agg.InnerExceptions.Count > 0 ? agg.InnerExceptions[0] : cur.InnerException;
        }
        return list;
    }

    private static Dictionary<string, object?> ConvertOne(Exception ex, VakaOptions options)
    {
        var type = ex.GetType();
        var e = new Dictionary<string, object?>
        {
            ["type"] = type.FullName ?? type.Name,
            ["value"] = ex.Message,
            ["module"] = type.Namespace,
            ["stacktrace"] = Frames(ex, options),
        };
        return e;
    }

    internal static List<Dictionary<string, object?>> Frames(Exception ex, VakaOptions options)
    {
        var frames = new List<Dictionary<string, object?>>();
        StackFrame[] raw;
        try { raw = new StackTrace(ex, fNeedFileInfo: true).GetFrames(); }
        catch { return frames; }
        foreach (var f in raw)
        {
            if (frames.Count >= MaxFrames) break;
            var frame = ConvertFrame(f, options);
            if (frame is not null) frames.Add(frame);
        }
        return frames;
    }

    internal static Dictionary<string, object?>? ConvertFrame(StackFrame f, VakaOptions options)
    {
        MethodBase? method;
        try { method = f.GetMethod(); } catch { method = null; }
        if (method is null) return null;
        var (module, function) = Names(method);
        var frame = new Dictionary<string, object?>
        {
            ["function"] = function,
            ["module"] = module,
            ["in_app"] = IsInApp(module, options),
        };
        var file = f.GetFileName();
        if (!string.IsNullOrEmpty(file))
        {
            frame["filename"] = Path.GetFileName(file);
            frame["abs_path"] = file;
        }
        var line = f.GetFileLineNumber();
        if (line > 0) frame["lineno"] = line;
        var col = f.GetFileColumnNumber();
        if (col > 0) frame["colno"] = col;
        if (method.Module.Assembly.GetName().Name is { } asm) frame["package"] = asm;
        return frame;
    }

    /// <summary>
    /// Readable module (type) and function names. Compiler-generated async state machines
    /// (<c>Outer+&lt;DoWork&gt;d__3.MoveNext</c>) and iterators become <c>Outer.DoWork</c>.
    /// </summary>
    internal static (string? Module, string Function) Names(MethodBase method)
    {
        var type = method.DeclaringType;
        var function = Clean(method.Name);
        // Compiler-generated nested types: async/iterator state machines (<DoWork>d__3), closures (<>c, <>c__DisplayClass0_0).
        while (type is not null && type.Name.StartsWith('<') && type.DeclaringType is not null)
        {
            if (!type.Name.StartsWith("<>")) function = Clean(type.Name);
            type = type.DeclaringType;
        }
        var module = type?.FullName?.Replace('+', '.');
        // Generic types: drop the assembly-qualified type arguments.
        if (module is not null)
        {
            var bracket = module.IndexOf('[');
            if (bracket > 0) module = module[..bracket];
        }
        return (module, function);
    }

    /// <summary>
    /// Readable name for compiler-generated members: <c>&lt;DoWork&gt;d__3</c> → <c>DoWork</c>,
    /// <c>&lt;Main&gt;b__0_0</c> → <c>Main.lambda</c>, <c>&lt;&lt;Main&gt;$&gt;b__0_1</c> (top-level statements) → <c>Main.lambda</c>,
    /// <c>&lt;Run&gt;g__Local|0_0</c> → <c>Run.Local</c>.
    /// </summary>
    internal static string Clean(string name)
    {
        if (!name.StartsWith('<')) return name;
        int depth = 0, close = -1;
        for (var i = 0; i < name.Length; i++)
        {
            if (name[i] == '<') depth++;
            else if (name[i] == '>' && --depth == 0) { close = i; break; }
        }
        if (close < 1) return name;
        var inner = Clean(name[1..close]);
        var rest = name[(close + 1)..];
        if (inner.Length == 0) return name;
        if (rest.StartsWith("b__")) return inner + ".lambda";
        if (rest.StartsWith("g__"))
        {
            var local = rest[3..];
            var bar = local.IndexOf('|');
            return inner + "." + (bar > 0 ? local[..bar] : local);
        }
        return inner;
    }

    internal static bool IsInApp(string? module, VakaOptions options)
    {
        if (string.IsNullOrEmpty(module)) return false;
        var m = module + ".";
        if (options.InAppInclude.Count > 0)
            return options.InAppInclude.Any(p => m.StartsWith(p, StringComparison.Ordinal));
        return !options.InAppExclude.Any(p => m.StartsWith(p, StringComparison.Ordinal));
    }
}
