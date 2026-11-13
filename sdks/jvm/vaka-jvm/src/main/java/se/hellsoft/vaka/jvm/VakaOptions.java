package se.hellsoft.vaka.jvm;

import java.time.Duration;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import se.hellsoft.vaka.jvm.internal.Transport;

/**
 * Configuration for {@link Vaka#init(VakaOptions)}. Unset values fall back to the environment:
 * {@code VAKA_KEY}, {@code VAKA_ENDPOINT}, {@code VAKA_RELEASE}, {@code VAKA_ENVIRONMENT} and
 * {@code VAKA_SERVICE} (or {@code OTEL_SERVICE_NAME}), or the matching system properties
 * {@code vaka.key}, {@code vaka.endpoint}, ... An empty key or endpoint disables the SDK.
 */
public final class VakaOptions {
    final String key;
    final String endpoint;
    final String release;
    final String environment;
    final String serviceName;
    final List<String> inAppPackages;
    final Map<String, String> tags;
    final boolean captureUncaughtExceptions;
    final boolean sessions;
    final boolean openTelemetry;
    final boolean registerGlobalOpenTelemetry;
    final double tracesSampleRate;
    final Duration flushInterval;
    final int maxQueueSize;
    final Duration flagsCacheTtl;
    final boolean shutdownHook;
    final boolean debug;
    final Transport transport;

    private VakaOptions(Builder b) {
        key = orEnv(b.key, "VAKA_KEY", "vaka.key", "");
        endpoint = orEnv(b.endpoint, "VAKA_ENDPOINT", "vaka.endpoint", "").replaceAll("/+$", "");
        release = emptyToNull(orEnv(b.release, "VAKA_RELEASE", "vaka.release", ""));
        environment = orEnv(b.environment, "VAKA_ENVIRONMENT", "vaka.environment", "production");
        String service = orEnv(b.serviceName, "VAKA_SERVICE", "vaka.service", "");
        if (service.isEmpty()) service = orEnv(null, "OTEL_SERVICE_NAME", "otel.service.name", "");
        serviceName = emptyToNull(service);
        inAppPackages = Collections.unmodifiableList(new ArrayList<>(b.inAppPackages));
        tags = Collections.unmodifiableMap(new LinkedHashMap<>(b.tags));
        captureUncaughtExceptions = b.captureUncaughtExceptions;
        sessions = b.sessions;
        openTelemetry = b.openTelemetry;
        registerGlobalOpenTelemetry = b.registerGlobalOpenTelemetry;
        tracesSampleRate = b.tracesSampleRate;
        flushInterval = b.flushInterval;
        maxQueueSize = b.maxQueueSize;
        flagsCacheTtl = b.flagsCacheTtl;
        shutdownHook = b.shutdownHook;
        debug = b.debug;
        transport = b.transport;
    }

    public static Builder builder() { return new Builder(); }

    /** Options taken entirely from the environment. */
    public static VakaOptions fromEnvironment() { return builder().build(); }

    public String key() { return key; }
    public String endpoint() { return endpoint; }
    /** May be null. */
    public String release() { return release; }
    public String environment() { return environment; }
    /** May be null. */
    public String serviceName() { return serviceName; }
    public List<String> inAppPackages() { return inAppPackages; }
    public Map<String, String> tags() { return tags; }
    public boolean isEnabled() { return !key.isEmpty() && !endpoint.isEmpty(); }

    private static String orEnv(String value, String env, String prop, String fallback) {
        if (value != null) return value.trim();
        String v = System.getProperty(prop);
        if (v == null || v.trim().isEmpty()) v = System.getenv(env);
        return v == null || v.trim().isEmpty() ? fallback : v.trim();
    }

    private static String emptyToNull(String s) { return s == null || s.isEmpty() ? null : s; }

    public static final class Builder {
        private String key;
        private String endpoint;
        private String release;
        private String environment;
        private String serviceName;
        private final List<String> inAppPackages = new ArrayList<>();
        private final Map<String, String> tags = new LinkedHashMap<>();
        private boolean captureUncaughtExceptions = true;
        private boolean sessions = true;
        private boolean openTelemetry = true;
        private boolean registerGlobalOpenTelemetry = true;
        private double tracesSampleRate = 1.0;
        private Duration flushInterval = Duration.ofSeconds(5);
        private int maxQueueSize = 1000;
        private Duration flagsCacheTtl = Duration.ofSeconds(30);
        private boolean shutdownHook = true;
        private boolean debug;
        private Transport transport;

        private Builder() {}

        /** The project's ingest key ({@code vk_...}). */
        public Builder key(String key) { this.key = key; return this; }

        /** The Vaka server URL, such as {@code https://vaka.example.com}. */
        public Builder endpoint(String endpoint) { this.endpoint = endpoint; return this; }

        /** The version you deploy, such as {@code 2.4.0}; release health and regressions are per release. */
        public Builder release(String release) { this.release = release; return this; }

        /** Defaults to {@code production}. */
        public Builder environment(String environment) { this.environment = environment; return this; }

        /** OpenTelemetry {@code service.name}; also sent as the {@code service} tag on errors. */
        public Builder serviceName(String serviceName) { this.serviceName = serviceName; return this; }

        /**
         * Package prefixes that are your code, such as {@code com.example}. Frames in them are
         * {@code in_app} and drive grouping. Without any, everything except well-known runtime,
         * framework and library packages counts as your code.
         */
        public Builder inAppPackages(String... packages) { inAppPackages.addAll(Arrays.asList(packages)); return this; }

        /** A tag sent with every error. */
        public Builder tag(String key, String value) { tags.put(Objects.requireNonNull(key), Objects.requireNonNull(value)); return this; }

        /** Report uncaught exceptions (default true). The previous handler still runs. */
        public Builder captureUncaughtExceptions(boolean enabled) { captureUncaughtExceptions = enabled; return this; }

        /** One release-health session per process run (default true). */
        public Builder sessions(boolean enabled) { sessions = enabled; return this; }

        /**
         * Create an OpenTelemetry SDK that exports traces and logs to Vaka (default true). Turn it
         * off when the app already configures OpenTelemetry (for example with the Java agent);
         * see {@link VakaOpenTelemetry} for adding Vaka to an existing SDK.
         */
        public Builder openTelemetry(boolean enabled) { openTelemetry = enabled; return this; }

        /** Register the created SDK as {@code GlobalOpenTelemetry} (default true). */
        public Builder registerGlobalOpenTelemetry(boolean enabled) { registerGlobalOpenTelemetry = enabled; return this; }

        /** Share of new traces to record, 0.0 to 1.0 (default 1.0). Child spans follow their parent. */
        public Builder tracesSampleRate(double rate) {
            if (rate < 0 || rate > 1) throw new IllegalArgumentException("tracesSampleRate must be in [0, 1]");
            tracesSampleRate = rate;
            return this;
        }

        /** How often queued errors, events and sessions are sent (default 5 s). */
        public Builder flushInterval(Duration interval) { flushInterval = Objects.requireNonNull(interval); return this; }

        /** Items kept in memory while the server is unreachable; the oldest are dropped (default 1000). */
        public Builder maxQueueSize(int size) { maxQueueSize = Math.max(1, size); return this; }

        /** How long evaluated feature flags are reused per identity (default 30 s; zero disables caching). */
        public Builder flagsCacheTtl(Duration ttl) { flagsCacheTtl = Objects.requireNonNull(ttl); return this; }

        /** Close the SDK (end the session, flush) in a JVM shutdown hook (default true). */
        public Builder shutdownHook(boolean enabled) { shutdownHook = enabled; return this; }

        /** Log SDK activity to stderr. */
        public Builder debug(boolean enabled) { debug = enabled; return this; }

        /** For tests: replaces the HTTP transport. */
        Builder transport(Transport transport) { this.transport = transport; return this; }

        public VakaOptions build() { return new VakaOptions(this); }
    }
}
