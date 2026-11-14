package se.hellsoft.vaka.jvm;

import io.opentelemetry.api.OpenTelemetry;
import io.opentelemetry.api.trace.Span;
import io.opentelemetry.api.trace.SpanContext;
import io.opentelemetry.sdk.OpenTelemetrySdk;
import java.io.IOException;
import java.io.InputStream;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import se.hellsoft.vaka.jvm.internal.Exceptions;
import se.hellsoft.vaka.jvm.internal.HttpTransport;
import se.hellsoft.vaka.jvm.internal.Json;
import se.hellsoft.vaka.jvm.internal.Logger;
import se.hellsoft.vaka.jvm.internal.Transport;

/**
 * A running SDK instance, returned by {@link Vaka#init(VakaOptions)}. The static methods on
 * {@link Vaka} call the current instance; use this class directly only when you need more than one.
 * Closing it ends the session, flushes queued data and shuts down the OpenTelemetry SDK it created.
 */
public final class VakaClient implements AutoCloseable {
    public static final String SDK_NAME = "vaka-jvm";
    public static final String SDK_VERSION = readVersion();
    static final int BATCH_SIZE = 100;
    /** Span / log record attribute: the SDK already reported this error to Vaka. */
    static final io.opentelemetry.api.common.AttributeKey<Boolean> ERROR_CAPTURED =
        io.opentelemetry.api.common.AttributeKey.booleanKey("vaka.error_captured");
    private static final long MAX_BACKOFF_MS = 5 * 60_000;

    final VakaOptions options;
    private final Logger log;
    private final Transport transport;
    private final Exceptions exceptions;
    private final VakaScope global = new VakaScope();
    private final String anonId = UUID.randomUUID().toString();
    private final Map<String, Object> contexts;
    private final Map<String, Object> device;
    private final String hostName;
    private final ScheduledExecutorService executor;
    private final OpenTelemetrySdk otel;

    // Queue of serialized items; guarded by `queue`.
    private final ArrayDeque<String> queue = new ArrayDeque<>();
    private int dropped;
    // Touched only on the executor thread.
    private long backoffMs;
    private long nextAttempt;
    private volatile boolean paused;
    private final AtomicBoolean drainScheduled = new AtomicBoolean();
    private final AtomicBoolean closed = new AtomicBoolean();

    // Session (one per process run).
    private final String sessionId;
    private final long sessionStarted = System.currentTimeMillis();
    private final AtomicInteger sessionErrors = new AtomicInteger();
    private volatile boolean sessionEnded;

    private final FlagCache flagCache;
    private Thread.UncaughtExceptionHandler previousHandler;
    private Thread.UncaughtExceptionHandler installedHandler;
    private Thread shutdownHook;

    VakaClient(VakaOptions options) {
        this.options = options;
        this.log = new Logger(options.debug);
        this.transport = options.transport != null ? options.transport
            : new HttpTransport(options.endpoint, options.key, SDK_NAME + "/" + SDK_VERSION, log);
        this.exceptions = new Exceptions(options.inAppPackages);
        for (Map.Entry<String, String> t : options.tags.entrySet()) global.setTag(t.getKey(), t.getValue());
        if (options.serviceName != null) global.setTag("service", options.serviceName);
        this.hostName = hostName();
        this.contexts = contexts(hostName);
        this.device = new LinkedHashMap<>();
        device.put("os_name", System.getProperty("os.name"));
        device.put("os_version", System.getProperty("os.version"));
        if (options.release != null) device.put("app_version", options.release);
        this.flagCache = new FlagCache(options.flagsCacheTtl.toMillis());
        this.executor = Executors.newSingleThreadScheduledExecutor(r -> {
            Thread t = new Thread(r, "vaka-sender");
            t.setDaemon(true);
            return t;
        });
        long interval = Math.max(10, options.flushInterval.toMillis());
        executor.scheduleWithFixedDelay(() -> drain(false), interval, interval, TimeUnit.MILLISECONDS);
        this.otel = options.openTelemetry ? OpenTelemetrySetup.create(options, this, log) : null;
        this.sessionId = options.sessions ? UUID.randomUUID().toString() : null;
        if (sessionId != null) enqueue(sessionItem("ok"));
        log.debug("initialised " + SDK_NAME + "/" + SDK_VERSION + " for " + options.endpoint
            + " release=" + options.release + " environment=" + options.environment);
    }

    void installHooks() {
        if (options.captureUncaughtExceptions) {
            previousHandler = Thread.getDefaultUncaughtExceptionHandler();
            installedHandler = this::onUncaught;
            Thread.setDefaultUncaughtExceptionHandler(installedHandler);
        }
        if (options.shutdownHook) {
            shutdownHook = new Thread(() -> close(Duration.ofSeconds(3)), "vaka-shutdown");
            Runtime.getRuntime().addShutdownHook(shutdownHook);
        }
    }

    // ------------------------------------------------------------------ scope

    /** The global scope, shared by all threads. */
    public VakaScope globalScope() { return global; }

    /** The current {@link VakaScope} on the OpenTelemetry context, or the global scope. */
    public VakaScope currentScope() {
        VakaScope s = VakaScope.current();
        return s != null ? s : global;
    }

    /** The OpenTelemetry SDK this client created, or {@link OpenTelemetry#noop()}. */
    public OpenTelemetry openTelemetry() { return otel != null ? otel : OpenTelemetry.noop(); }

    /** Identify the user on the current scope (opt-in). Also sets {@code enduser.id} on the current span. */
    public void setUser(String userId) {
        currentScope().setUser(userId);
        Span span = Span.current();
        if (userId != null && span.isRecording()) span.setAttribute("enduser.id", userId);
    }

    public void setTag(String key, String value) { currentScope().setTag(key, value); }

    public void addBreadcrumb(String category, String message, Level level, Map<String, ?> data) {
        currentScope().addBreadcrumb(category, message, level, data);
    }

    // ------------------------------------------------------------------ capture

    public String captureException(Throwable t, Level level) {
        Map<String, Object> item = errorItem(level, false, true);
        item.put("exceptions", exceptions.exceptions(t));
        if (level.isError()) sessionErrors.incrementAndGet();
        enqueue(item);
        currentScope().addBreadcrumb("error", t.getClass().getName() + ": " + (t.getMessage() == null ? "" : t.getMessage()), level, null);
        return (String) item.get("event_id");
    }

    public String captureMessage(String message, Level level) {
        Map<String, Object> item = errorItem(level, false, true);
        item.put("message", message);
        if (level.isError()) sessionErrors.incrementAndGet();
        enqueue(item);
        return (String) item.get("event_id");
    }

    /** Tracks a custom analytics event. Props: flat string, number or boolean values. */
    public void track(String name, Map<String, ?> props) {
        Map<String, Object> item = common("event");
        item.put("name", name);
        Map<String, Object> p = new LinkedHashMap<>();
        if (props != null) {
            for (Map.Entry<String, ?> e : props.entrySet()) {
                Object v = e.getValue();
                if (p.size() >= 64) break;
                p.put(e.getKey(), v instanceof Number || v instanceof Boolean || v == null ? v : String.valueOf(v));
            }
        }
        item.put("props", p);
        enqueue(item);
    }

    void onUncaught(Thread thread, Throwable t) {
        try {
            captureCrash(thread, t);
        } catch (Throwable e) {
            log.warn("failed to report uncaught exception", e);
        }
        Thread.UncaughtExceptionHandler prev = previousHandler;
        if (prev != null) {
            prev.uncaughtException(thread, t);
        } else {
            // What ThreadGroup.uncaughtException would print (it can't be called: it would call us again).
            System.err.print("Exception in thread \"" + thread.getName() + "\" ");
            t.printStackTrace(System.err);
        }
    }

    /**
     * Reports an uncaught exception and waits (up to 2 s) until it is sent. On the main thread
     * the process is usually about to exit, so the session ends as {@code crashed}.
     */
    void captureCrash(Thread thread, Throwable t) {
        Map<String, Object> item = errorItem(Level.FATAL, true, false);
        item.put("exceptions", exceptions.exceptions(t));
        @SuppressWarnings("unchecked")
        Map<String, Object> tags = (Map<String, Object>) item.get("tags");
        tags.put("mechanism", "uncaught");
        tags.put("thread", thread.getName());
        sessionErrors.incrementAndGet();
        enqueue(item);
        if ("main".equals(thread.getName())) endSession("crashed");
        flush(Duration.ofSeconds(2));
    }

    // ------------------------------------------------------------------ feedback

    /**
     * Sends user feedback right away (blocking) with the release, environment and the current
     * scope's user, tags and flags.
     *
     * @throws FeedbackException when the feedback is invalid or the server refused it
     */
    public FeedbackResult captureFeedback(Feedback f) {
        String message = f.message.trim();
        if (message.isEmpty()) throw new FeedbackException("feedback needs a message", 0);
        if (f.attachments.size() > Feedback.MAX_ATTACHMENTS) {
            throw new FeedbackException("at most " + Feedback.MAX_ATTACHMENTS + " attachments per feedback", 0);
        }
        if (message.codePointCount(0, message.length()) > 5000) message = message.substring(0, message.offsetByCodePoints(0, 5000));
        VakaScope.Snapshot s = snapshot();
        Map<String, Object> item = new LinkedHashMap<>();
        item.put("id", UUID.randomUUID().toString());
        item.put("timestamp", System.currentTimeMillis());
        item.put("platform", "jvm");
        item.put("source", "api");
        item.put("message", message);
        if (options.release != null) item.put("release", options.release);
        item.put("environment", options.environment);
        putIfSet(item, "email", f.email == null ? null : f.email.trim());
        putIfSet(item, "name", f.name == null ? null : f.name.trim());
        putIfSet(item, "event_id", f.eventId);
        putIfSet(item, "issue", f.issue);
        putIfSet(item, "user_id", f.userId != null ? f.userId : s.userId);
        putIfSet(item, "session_id", f.sessionId);
        putIfSet(item, "url", f.url);
        item.put("anon_id", anonId);
        Map<String, String> tags = new LinkedHashMap<>(s.tags);
        tags.putAll(f.tags);
        item.put("tags", tags);
        if (!s.flags.isEmpty()) item.put("flags", s.flags);
        String json = Json.write(item);
        Map<String, String> headers = new LinkedHashMap<>();
        if (f.ip != null && !f.ip.isEmpty()) headers.put("X-Forwarded-For", f.ip);
        Transport.Response res;
        if (f.attachments.isEmpty()) {
            res = transport.post("/ingest/v1/feedback", "application/json", json.getBytes(StandardCharsets.UTF_8), headers, Duration.ofSeconds(30));
        } else {
            String boundary = "vaka-" + UUID.randomUUID().toString().replace("-", "");
            res = transport.post("/ingest/v1/feedback", "multipart/form-data; boundary=" + boundary, multipart(boundary, json, f.attachments), headers, Duration.ofSeconds(30));
        }
        Map<?, ?> body = Collections.emptyMap();
        try {
            Object parsed = res.body.isEmpty() ? null : Json.parse(res.body);
            if (parsed instanceof Map) body = (Map<?, ?>) parsed;
        } catch (IllegalArgumentException ignored) {
            // not JSON
        }
        if (res.status < 200 || res.status >= 300) {
            Object err = body.get("error");
            String detail = res.status == 0 ? ": " + res.body : err != null ? ": " + err : "";
            throw new FeedbackException("vaka feedback: " + (res.status == 0 ? "request failed" : "HTTP " + res.status) + detail, res.status);
        }
        currentScope().addBreadcrumb("feedback", "user feedback sent");
        Object id = body.get("id");
        Object shortId = body.get("short_id");
        return new FeedbackResult(id != null ? id.toString() : (String) item.get("id"), shortId != null ? shortId.toString() : "");
    }

    private static void putIfSet(Map<String, Object> m, String key, String value) {
        if (value != null && !value.isEmpty()) m.put(key, value);
    }

    static byte[] multipart(String boundary, String json, java.util.List<Feedback.Attachment> files) {
        java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
        java.util.function.Consumer<String> w = str -> {
            byte[] b = str.getBytes(StandardCharsets.UTF_8);
            out.write(b, 0, b.length);
        };
        w.accept("--" + boundary + "\r\nContent-Disposition: form-data; name=\"feedback\"\r\nContent-Type: application/json\r\n\r\n");
        w.accept(json);
        w.accept("\r\n");
        for (Feedback.Attachment a : files) {
            String name = a.filename.replaceAll("[\"\r\n]", "");
            w.accept("--" + boundary + "\r\nContent-Disposition: form-data; name=\"attachment\"; filename=\"" + name + "\"\r\nContent-Type: " + a.contentType + "\r\n\r\n");
            out.write(a.data, 0, a.data.length);
            w.accept("\r\n");
        }
        w.accept("--" + boundary + "--\r\n");
        return out.toByteArray();
    }

    // ------------------------------------------------------------------ flags

    /** Evaluates feature flags for {@code request}; never throws (returns empty flags on failure). */
    public Flags flags(FlagRequest request) {
        String cacheKey = request.cacheKey(options.environment);
        Map<String, Object> cached = flagCache.get(cacheKey);
        if (cached != null) return new Flags(cached, true, this);
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("distinct_id", request.distinctId);
        if (request.userId != null) body.put("user_id", request.userId);
        body.put("environment", request.environment != null ? request.environment : options.environment);
        if (!request.properties.isEmpty()) body.put("properties", request.properties);
        Transport.Response res = transport.post("/ingest/v1/flags", Json.write(body), Duration.ofSeconds(5));
        if (res.status != 200) {
            log.warn("flag evaluation failed (HTTP " + res.status + "): " + truncate(res.body));
            return new Flags(Collections.emptyMap(), false, this);
        }
        try {
            Object parsed = Json.parse(res.body);
            Object flags = parsed instanceof Map ? ((Map<?, ?>) parsed).get("flags") : null;
            Map<String, Object> values = new LinkedHashMap<>();
            if (flags instanceof Map) {
                for (Map.Entry<?, ?> e : ((Map<?, ?>) flags).entrySet()) values.put(String.valueOf(e.getKey()), e.getValue());
            }
            flagCache.put(cacheKey, values);
            return new Flags(values, true, this);
        } catch (IllegalArgumentException e) {
            log.warn("could not parse flags response: " + e.getMessage());
            return new Flags(Collections.emptyMap(), false, this);
        }
    }

    // ------------------------------------------------------------------ items

    private Map<String, Object> common(String type) {
        VakaScope.Snapshot s = snapshot();
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("type", type);
        m.put("timestamp", System.currentTimeMillis());
        m.put("platform", "jvm");
        if (options.release != null) m.put("release", options.release);
        m.put("environment", options.environment);
        if (s.userId != null) m.put("user_id", s.userId);
        if (sessionId != null) m.put("session_id", sessionId);
        m.put("anon_id", anonId);
        if (!s.flags.isEmpty()) m.put("flags", s.flags);
        return m;
    }

    private Map<String, Object> errorItem(Level level, boolean fatal, boolean handled) {
        VakaScope.Snapshot s = snapshot();
        Map<String, Object> m = common("error");
        m.put("event_id", UUID.randomUUID().toString());
        m.put("level", level.wire);
        m.put("fatal", fatal);
        m.put("handled", handled);
        if (s.userId != null) m.put("user", Collections.singletonMap("id", s.userId));
        m.put("tags", new LinkedHashMap<String, Object>(s.tags));
        m.put("breadcrumbs", s.breadcrumbsJson());
        m.put("contexts", contexts);
        m.put("device", device);
        SpanContext span = Span.current().getSpanContext();
        if (span.isValid()) {
            // Tells the server not to derive a second error from this span's exception events.
            Span.current().setAttribute(ERROR_CAPTURED, true);
            m.put("trace_id", span.getTraceId());
            m.put("span_id", span.getSpanId());
        }
        return m;
    }

    private Map<String, Object> sessionItem(String status) {
        Map<String, Object> m = common("session");
        m.remove("user_id");
        m.put("session_id", sessionId);
        m.put("status", status);
        m.put("started", sessionStarted);
        long now = System.currentTimeMillis();
        m.put("timestamp", now);
        m.put("duration_ms", now - sessionStarted);
        m.put("errors", sessionErrors.get());
        return m;
    }

    private VakaScope.Snapshot snapshot() { return new VakaScope.Snapshot(global, VakaScope.current()); }

    String sessionId() { return sessionId; }

    String anonId() { return anonId; }

    private void endSession(String status) {
        if (sessionId == null || sessionEnded) return;
        sessionEnded = true;
        if ("exited".equals(status) && sessionErrors.get() > 0) status = "errored";
        enqueue(sessionItem(status));
    }

    // ------------------------------------------------------------------ queue and sending

    private void enqueue(Map<String, Object> item) {
        if (closed.get() && !"session".equals(item.get("type"))) {
            log.debug("dropping " + item.get("type") + " after close");
            return;
        }
        String json = Json.write(item);
        int size;
        synchronized (queue) {
            if (paused) return;
            queue.addLast(json);
            while (queue.size() > options.maxQueueSize) {
                queue.removeFirst();
                dropped++;
            }
            size = queue.size();
        }
        log.debug("queued " + item.get("type") + " (" + size + " pending)");
        if (size >= BATCH_SIZE) scheduleDrain();
    }

    private void scheduleDrain() {
        if (drainScheduled.compareAndSet(false, true)) {
            try {
                executor.execute(() -> {
                    drainScheduled.set(false);
                    drain(false);
                });
            } catch (java.util.concurrent.RejectedExecutionException e) {
                drainScheduled.set(false);
            }
        }
    }

    /** Sends queued items. Runs on the executor thread only. */
    private void drain(boolean force) {
        if (paused) return;
        if (!force && System.currentTimeMillis() < nextAttempt) return;
        int batchSize = BATCH_SIZE;
        while (true) {
            List<String> batch = new ArrayList<>(batchSize);
            synchronized (queue) {
                if (dropped > 0) {
                    log.warn("queue full, dropped " + dropped + " items");
                    dropped = 0;
                }
                for (String s : queue) {
                    if (batch.size() >= batchSize) break;
                    batch.add(s);
                }
            }
            if (batch.isEmpty()) return;
            StringBuilder body = new StringBuilder(256 + batch.size() * 512);
            body.append("{\"sdk\":{\"name\":\"").append(SDK_NAME).append("\",\"version\":\"").append(SDK_VERSION).append("\"},\"items\":[");
            for (int i = 0; i < batch.size(); i++) {
                if (i > 0) body.append(',');
                body.append(batch.get(i));
            }
            body.append("]}");
            Transport.Response res = transport.post("/ingest/v1/batch", body.toString(), Duration.ofSeconds(30));
            int code = res.status;
            if ((code >= 200 && code < 300) || code == 400) {
                if (code == 400) log.warn("server rejected batch: " + truncate(res.body));
                else if (res.body.contains("\"rejected\":[{")) log.warn("server rejected some items: " + truncate(res.body));
                else log.debug("sent " + batch.size() + " items: HTTP " + code);
                remove(batch);
                backoffMs = 0;
                batchSize = BATCH_SIZE;
            } else if (code == 401 || code == 403) {
                log.warn("ingest key rejected (HTTP " + code + "); sending is paused until restart: " + truncate(res.body));
                paused = true;
                synchronized (queue) { queue.clear(); }
                return;
            } else if (code == 413 && batch.size() > 1) {
                batchSize = Math.max(1, batch.size() / 2);
            } else if (code == 413) {
                log.warn("dropping an item the server says is too large");
                remove(batch);
            } else {
                backoffMs = backoffMs == 0 ? 1000 : Math.min(MAX_BACKOFF_MS, backoffMs * 2);
                long wait = res.retryAfterMillis > 0 ? Math.min(MAX_BACKOFF_MS, res.retryAfterMillis) : backoffMs;
                nextAttempt = System.currentTimeMillis() + wait;
                log.debug("send failed (HTTP " + code + "), retrying in " + wait + " ms");
                return;
            }
        }
    }

    private void remove(List<String> batch) {
        synchronized (queue) {
            // Only this thread removes from the head; items dropped for overflow meanwhile were
            // the oldest, i.e. ours, so remove by identity.
            for (String s : batch) {
                if (!queue.isEmpty() && queue.peekFirst() == s) queue.removeFirst();
                else queue.removeFirstOccurrence(s);
            }
        }
    }

    int pending() {
        synchronized (queue) { return queue.size(); }
    }

    /** Sends everything queued; returns false if that did not finish within {@code timeout}. */
    public boolean flush(Duration timeout) {
        Future<?> f;
        try {
            f = executor.submit(() -> drain(true));
        } catch (java.util.concurrent.RejectedExecutionException e) {
            return pending() == 0;
        }
        try {
            f.get(timeout.toMillis(), TimeUnit.MILLISECONDS);
            if (otel != null) otel.getSdkTracerProvider().forceFlush().join(timeout.toMillis(), TimeUnit.MILLISECONDS);
            return pending() == 0;
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return false;
        } catch (ExecutionException | TimeoutException e) {
            return false;
        }
    }

    /** Ends the session, flushes (up to 3 s) and shuts down. */
    @Override
    public void close() { close(Duration.ofSeconds(3)); }

    public void close(Duration timeout) {
        if (!closed.compareAndSet(false, true)) return;
        endSession("exited");
        flush(timeout);
        executor.shutdownNow();
        if (otel != null) otel.shutdown().join(timeout.toMillis(), TimeUnit.MILLISECONDS);
        // Restore the previous handler unless someone replaced ours meanwhile.
        if (installedHandler != null && Thread.getDefaultUncaughtExceptionHandler() == installedHandler) {
            Thread.setDefaultUncaughtExceptionHandler(previousHandler);
        }
        if (shutdownHook != null && Thread.currentThread() != shutdownHook) {
            try {
                Runtime.getRuntime().removeShutdownHook(shutdownHook);
            } catch (IllegalStateException ignored) {
                // already shutting down
            }
        }
        Vaka.clearIfCurrent(this);
        log.debug("closed");
    }

    public boolean isClosed() { return closed.get(); }

    // ------------------------------------------------------------------ helpers

    private static String truncate(String s) { return s.length() > 500 ? s.substring(0, 500) : s; }

    private static String hostName() {
        String h = System.getenv("HOSTNAME");
        if (h != null && !h.isEmpty()) return h;
        try {
            return InetAddress.getLocalHost().getHostName();
        } catch (IOException | RuntimeException e) {
            return null;
        }
    }

    private static Map<String, Object> contexts(String hostName) {
        Map<String, Object> runtime = new LinkedHashMap<>();
        runtime.put("name", System.getProperty("java.vm.name", "Java"));
        runtime.put("version", System.getProperty("java.version"));
        runtime.put("vendor", System.getProperty("java.vendor"));
        Map<String, Object> os = new LinkedHashMap<>();
        os.put("name", System.getProperty("os.name"));
        os.put("version", System.getProperty("os.version"));
        os.put("arch", System.getProperty("os.arch"));
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("runtime", runtime);
        m.put("os", os);
        if (hostName != null) m.put("host", Collections.singletonMap("name", hostName));
        return m;
    }

    private static String readVersion() {
        try (InputStream in = VakaClient.class.getResourceAsStream("version.txt")) {
            if (in == null) return "0.0.0";
            return new String(in.readAllBytes(), StandardCharsets.UTF_8).trim();
        } catch (IOException e) {
            return "0.0.0";
        }
    }

    /** Small LRU cache of evaluated flags. */
    private static final class FlagCache {
        private final long ttlMs;
        private final Map<String, Object[]> entries = new LinkedHashMap<String, Object[]>(64, 0.75f, true) {
            private static final long serialVersionUID = 1L;

            @Override
            protected boolean removeEldestEntry(Map.Entry<String, Object[]> eldest) { return size() > 10_000; }
        };

        FlagCache(long ttlMs) { this.ttlMs = ttlMs; }

        synchronized Map<String, Object> get(String key) {
            if (ttlMs <= 0) return null;
            Object[] e = entries.get(key);
            if (e == null) return null;
            if ((long) e[0] < System.currentTimeMillis()) {
                entries.remove(key);
                return null;
            }
            @SuppressWarnings("unchecked")
            Map<String, Object> v = (Map<String, Object>) e[1];
            return v;
        }

        synchronized void put(String key, Map<String, Object> values) {
            if (ttlMs > 0) entries.put(key, new Object[] {System.currentTimeMillis() + ttlMs, values});
        }
    }
}
