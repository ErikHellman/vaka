package se.hellsoft.vaka.jvm;

import java.util.Collections;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;

/** Who to evaluate feature flags for: a distinct id, an optional user id and targeting properties. */
public final class FlagRequest {
    final String distinctId;
    final String userId;
    final String environment;
    final Map<String, String> properties;

    private FlagRequest(String distinctId, String userId, String environment, Map<String, String> properties) {
        this.distinctId = distinctId;
        this.userId = userId;
        this.environment = environment;
        this.properties = Collections.unmodifiableMap(properties);
    }

    /** Evaluate for a stable id (user id, account id, device id ...). The same id always gets the same answer. */
    public static FlagRequest forDistinctId(String distinctId) {
        return new FlagRequest(Objects.requireNonNull(distinctId), null, null, new TreeMap<>());
    }

    /** Evaluate for an identified user; rules with {@code user_ids} match it. */
    public static FlagRequest forUser(String userId) {
        return new FlagRequest(Objects.requireNonNull(userId), userId, null, new TreeMap<>());
    }

    public FlagRequest userId(String userId) {
        return new FlagRequest(distinctId, userId, environment, new TreeMap<>(properties));
    }

    /** Overrides the SDK's environment for this evaluation. */
    public FlagRequest environment(String environment) {
        return new FlagRequest(distinctId, userId, environment, new TreeMap<>(properties));
    }

    /** A targeting property, matched by rules such as {@code {"properties": {"plan": "pro"}}}. */
    public FlagRequest property(String key, String value) {
        Map<String, String> p = new TreeMap<>(properties);
        p.put(Objects.requireNonNull(key), Objects.requireNonNull(value));
        return new FlagRequest(distinctId, userId, environment, p);
    }

    String cacheKey(String defaultEnvironment) {
        return distinctId + "\u0000" + (userId == null ? "" : userId) + "\u0000"
            + (environment == null ? defaultEnvironment : environment) + "\u0000" + properties;
    }
}
