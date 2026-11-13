package se.hellsoft.vaka.jvm;

import io.opentelemetry.context.Context;
import io.opentelemetry.context.ContextKey;
import io.opentelemetry.context.Scope;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * User, tags, breadcrumbs and feature flag values attached to errors and events.
 *
 * <p>There is one global scope. For servers, create a scope per request and make it current; it
 * lives on the OpenTelemetry {@link Context}, so it follows the request wherever the context is
 * propagated (instrumented executors, coroutines with the OpenTelemetry context element, ...):
 *
 * <pre>{@code
 * try (Scope ignored = VakaScope.create().setUser(userId).makeCurrent()) {
 *     handle(request); // Vaka.addBreadcrumb(...), Vaka.captureException(...) use this scope
 * }
 * }</pre>
 *
 * Errors carry the global scope merged with the current one: the current scope's user and
 * values win, breadcrumbs from both are combined in time order. All methods are thread-safe.
 */
public final class VakaScope {
    static final int MAX_BREADCRUMBS = 100;
    static final int MAX_FLAGS = 32;
    private static final ContextKey<VakaScope> KEY = ContextKey.named("vaka-scope");

    private String userId;
    private final Map<String, String> tags = new LinkedHashMap<>();
    private final Map<String, String> flags = new LinkedHashMap<>();
    private final ArrayDeque<Breadcrumb> breadcrumbs = new ArrayDeque<>();

    VakaScope() {}

    /** A new, empty scope. Call {@link #makeCurrent()} to use it. */
    public static VakaScope create() { return new VakaScope(); }

    /** The scope on the current OpenTelemetry context, or null. */
    public static VakaScope current() { return fromContext(Context.current()); }

    public static VakaScope fromContext(Context context) { return context.get(KEY); }

    /** Makes this scope current until the returned {@link Scope} is closed. */
    public Scope makeCurrent() { return storeIn(Context.current()).makeCurrent(); }

    /** A copy of {@code context} that carries this scope. */
    public Context storeIn(Context context) { return context.with(KEY, this); }

    /** Identify the user (opt-in: only if your privacy policy allows it). Null clears it. */
    public synchronized VakaScope setUser(String id) {
        userId = id == null || id.trim().isEmpty() ? null : id;
        return this;
    }

    public synchronized String user() { return userId; }

    /** Sets or (with null) removes a tag. */
    public synchronized VakaScope setTag(String key, String value) {
        if (value == null) tags.remove(key);
        else tags.put(key, value);
        return this;
    }

    /** Records the value of a feature flag used while handling this scope (exposure). */
    public synchronized VakaScope setFlag(String key, String value) {
        if (value == null) flags.remove(key);
        else if (flags.containsKey(key) || flags.size() < MAX_FLAGS) flags.put(key, value);
        return this;
    }

    public VakaScope addBreadcrumb(String category, String message) {
        return addBreadcrumb(category, message, Level.INFO, null);
    }

    public VakaScope addBreadcrumb(String category, String message, Level level, Map<String, ?> data) {
        Breadcrumb b = new Breadcrumb(System.currentTimeMillis(), category, message, level, data);
        synchronized (this) {
            breadcrumbs.addLast(b);
            while (breadcrumbs.size() > MAX_BREADCRUMBS) breadcrumbs.removeFirst();
        }
        return this;
    }

    /** Removes everything from this scope. */
    public synchronized void clear() {
        userId = null;
        tags.clear();
        flags.clear();
        breadcrumbs.clear();
    }

    synchronized Map<String, String> tags() { return new LinkedHashMap<>(tags); }

    synchronized Map<String, String> flags() { return new LinkedHashMap<>(flags); }

    synchronized List<Breadcrumb> breadcrumbs() { return new ArrayList<>(breadcrumbs); }

    /** Global scope merged with an optional current scope (current wins). */
    static final class Snapshot {
        final String userId;
        final Map<String, String> tags;
        final Map<String, String> flags;
        final List<Breadcrumb> breadcrumbs;

        Snapshot(VakaScope global, VakaScope current) {
            String user = global.user();
            Map<String, String> t = global.tags();
            Map<String, String> f = global.flags();
            List<Breadcrumb> b = global.breadcrumbs();
            if (current != null && current != global) {
                if (current.user() != null) user = current.user();
                t.putAll(current.tags());
                f.putAll(current.flags());
                b.addAll(current.breadcrumbs());
                b.sort((x, y) -> Long.compare(x.timestamp, y.timestamp));
                if (b.size() > MAX_BREADCRUMBS) b = new ArrayList<>(b.subList(b.size() - MAX_BREADCRUMBS, b.size()));
            }
            userId = user;
            tags = t;
            flags = f;
            breadcrumbs = b;
        }

        List<Map<String, Object>> breadcrumbsJson() {
            List<Map<String, Object>> out = new ArrayList<>(breadcrumbs.size());
            for (Breadcrumb b : breadcrumbs) out.add(b.toJson());
            return out;
        }
    }

    static final class Breadcrumb {
        final long timestamp;
        final String category;
        final String message;
        final Level level;
        final Map<String, Object> data;

        Breadcrumb(long timestamp, String category, String message, Level level, Map<String, ?> data) {
            this.timestamp = timestamp;
            this.category = category == null ? "default" : category;
            this.message = message == null ? "" : message;
            this.level = level == null ? Level.INFO : level;
            this.data = data == null || data.isEmpty() ? Collections.emptyMap() : new LinkedHashMap<>(data);
        }

        Map<String, Object> toJson() {
            Map<String, Object> m = new LinkedHashMap<>();
            m.put("timestamp", timestamp);
            m.put("category", category);
            m.put("message", message);
            m.put("level", level.wire);
            if (!data.isEmpty()) m.put("data", data);
            return m;
        }
    }
}
