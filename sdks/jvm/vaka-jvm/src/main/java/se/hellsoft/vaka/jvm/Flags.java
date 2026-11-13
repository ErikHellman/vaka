package se.hellsoft.vaka.jvm;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * Feature flag values for one identity, from {@link Vaka#flags(FlagRequest)}.
 *
 * <p>Reading a flag records the value on the current {@link VakaScope} (or the global scope when
 * none is current), so errors and events report which variant they ran with. Flag health and
 * experiment results in Vaka are computed from these exposures.
 */
public final class Flags {
    private static final Flags EMPTY = new Flags(Collections.emptyMap(), false, null);

    private final Map<String, Object> values;
    private final boolean loaded;
    private final VakaClient client;

    Flags(Map<String, Object> values, boolean loaded, VakaClient client) {
        this.values = Collections.unmodifiableMap(new LinkedHashMap<>(values));
        this.loaded = loaded;
        this.client = client;
    }

    /** No flags: every flag is off and every variant is the fallback. */
    public static Flags empty() { return EMPTY; }

    /** False when evaluation failed (server unreachable, SDK disabled) and only fallbacks apply. */
    public boolean isLoaded() { return loaded; }

    /** True when the flag is on: {@code true}, or any variant. Unknown flags are off. */
    public boolean isEnabled(String key) {
        Object v = values.get(key);
        boolean on = v instanceof Boolean ? (Boolean) v : v instanceof String && !((String) v).isEmpty() && !"false".equals(v);
        expose(key, v instanceof String ? (String) v : String.valueOf(on));
        return on;
    }

    /** The flag's variant, or {@code fallback} when the flag is off, unknown or has no variants. */
    public String variant(String key, String fallback) {
        Object v = values.get(key);
        if (v instanceof String && !((String) v).isEmpty()) {
            expose(key, (String) v);
            return (String) v;
        }
        expose(key, v == null ? "false" : String.valueOf(v));
        return fallback;
    }

    /** The raw value ({@code Boolean} or variant {@code String}), or null; does not record an exposure. */
    public Object value(String key) { return values.get(key); }

    /** All values; does not record exposures. */
    public Map<String, Object> asMap() { return values; }

    private void expose(String key, String value) {
        if (client != null) client.currentScope().setFlag(key, value);
    }

    @Override
    public String toString() { return "Flags" + values; }
}
