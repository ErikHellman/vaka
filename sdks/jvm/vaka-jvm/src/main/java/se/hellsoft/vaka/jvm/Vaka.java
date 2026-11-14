package se.hellsoft.vaka.jvm;

import io.opentelemetry.api.OpenTelemetry;
import java.time.Duration;
import java.util.Collections;
import java.util.Map;

/**
 * Entry point of the Vaka JVM SDK. Call {@link #init(VakaOptions)} once at startup; every other
 * method is a no-op until then (and when the SDK is disabled by an empty key or endpoint).
 *
 * <pre>{@code
 * Vaka.init(VakaOptions.builder()
 *     .key("vk_...").endpoint("https://vaka.example.com")
 *     .release("2.4.0").serviceName("checkout").inAppPackages("com.example")
 *     .build());
 *
 * try { work(); } catch (Exception e) { Vaka.captureException(e); }
 * Tracer tracer = Vaka.openTelemetry().getTracer("checkout");
 * }</pre>
 */
public final class Vaka {
    private static volatile VakaClient current;

    private Vaka() {}

    /**
     * Starts the SDK: a release-health session, the uncaught exception handler, a shutdown hook
     * and (unless turned off) an OpenTelemetry SDK exporting traces and logs to Vaka. Calling it
     * again closes the previous instance first. Returns null when the SDK is disabled.
     */
    public static synchronized VakaClient init(VakaOptions options) {
        VakaClient old = current;
        if (old != null) old.close();
        if (!options.isEnabled()) {
            if (options.debug) System.err.println("[vaka] disabled: no key or endpoint configured");
            current = null;
            return null;
        }
        VakaClient c = new VakaClient(options);
        current = c;
        c.installHooks();
        return c;
    }

    /** {@link #init(VakaOptions)} with {@link VakaOptions#fromEnvironment()}. */
    public static VakaClient init() { return init(VakaOptions.fromEnvironment()); }

    /** The running instance, or null. */
    public static VakaClient client() { return current; }

    public static boolean isEnabled() { return current != null; }

    static synchronized void clearIfCurrent(VakaClient c) {
        if (current == c) current = null;
    }

    /** The OpenTelemetry SDK created by {@link #init}, or {@link OpenTelemetry#noop()}. */
    public static OpenTelemetry openTelemetry() {
        VakaClient c = current;
        return c != null ? c.openTelemetry() : OpenTelemetry.noop();
    }

    /** Reports a handled exception (level error) with its cause chain. Returns the event id, or null. */
    public static String captureException(Throwable t) { return captureException(t, Level.ERROR); }

    public static String captureException(Throwable t, Level level) {
        VakaClient c = current;
        return c != null ? c.captureException(t, level) : null;
    }

    /** Reports a message without an exception. Returns the event id, or null. */
    public static String captureMessage(String message, Level level) {
        VakaClient c = current;
        return c != null ? c.captureMessage(message, level) : null;
    }

    /**
     * Forwards feedback a user wrote (a support form, in-app chat) to Vaka. Blocks until the
     * server answers.
     *
     * @throws FeedbackException when the SDK is not initialised, the feedback is invalid or the
     *     server refused it ({@link FeedbackException#status()} 429 means a rate limit was hit)
     */
    public static FeedbackResult captureFeedback(Feedback feedback) {
        VakaClient c = current;
        if (c == null) throw new FeedbackException("vaka is not initialized", 0);
        return c.captureFeedback(feedback);
    }

    /** Adds a breadcrumb to the current scope; the last 100 are sent with the next error. */
    public static void addBreadcrumb(String category, String message) { addBreadcrumb(category, message, Level.INFO, null); }

    public static void addBreadcrumb(String category, String message, Level level, Map<String, ?> data) {
        VakaClient c = current;
        if (c != null) c.addBreadcrumb(category, message, level, data);
    }

    /** Sets or (with null) removes a tag on the current scope. */
    public static void setTag(String key, String value) {
        VakaClient c = current;
        if (c != null) c.setTag(key, value);
    }

    /** Identifies the user on the current scope (opt-in). Null clears it. */
    public static void setUser(String userId) {
        VakaClient c = current;
        if (c != null) c.setUser(userId);
    }

    /** The current {@link VakaScope} (request scope or global), or a detached scope when not initialised. */
    public static VakaScope currentScope() {
        VakaClient c = current;
        return c != null ? c.currentScope() : VakaScope.create();
    }

    /** Tracks a custom analytics event. */
    public static void track(String name) { track(name, Collections.emptyMap()); }

    public static void track(String name, Map<String, ?> props) {
        VakaClient c = current;
        if (c != null) c.track(name, props);
    }

    /** Evaluates feature flags for a distinct id (blocking HTTP call, cached briefly). */
    public static Flags flags(String distinctId) { return flags(FlagRequest.forDistinctId(distinctId)); }

    public static Flags flags(FlagRequest request) {
        VakaClient c = current;
        return c != null ? c.flags(request) : Flags.empty();
    }

    /** Sends queued data; returns false if that did not finish within {@code timeout}. */
    public static boolean flush(Duration timeout) {
        VakaClient c = current;
        return c == null || c.flush(timeout);
    }

    /** Ends the session, flushes and shuts down. Also done by the shutdown hook. */
    public static void close() {
        VakaClient c = current;
        if (c != null) c.close();
    }
}
