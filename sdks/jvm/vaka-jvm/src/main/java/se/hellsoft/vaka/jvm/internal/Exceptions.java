package se.hellsoft.vaka.jvm.internal;

import java.util.ArrayList;
import java.util.Collections;
import java.util.IdentityHashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** Turns a Throwable into the ingest {@code exceptions} array (outermost first, crash site first). */
public final class Exceptions {
    public static final int MAX_CAUSES = 10;
    public static final int MAX_FRAMES = 200;

    /** Frames from these packages are library or runtime code ({@code in_app: false}). */
    static final String[] SYSTEM_PREFIXES = {
        "java.", "javax.", "jdk.", "sun.", "com.sun.", "jakarta.", "kotlin.", "kotlinx.", "scala.",
        "groovy.", "org.codehaus.groovy.", "clojure.", "org.springframework.", "io.micronaut.", "io.quarkus.",
        "io.vertx.", "io.netty.", "io.undertow.", "org.eclipse.jetty.", "org.apache.", "org.glassfish.",
        "io.ktor.", "io.javalin.", "io.dropwizard.", "reactor.", "io.reactivex.", "rx.", "okhttp3.", "okio.",
        "retrofit2.", "com.fasterxml.", "com.google.common.", "com.google.protobuf.", "com.google.gson.",
        "com.google.inject.", "io.grpc.", "org.hibernate.", "com.zaxxer.", "org.postgresql.", "com.mysql.",
        "org.jooq.", "org.slf4j.", "ch.qos.logback.", "org.junit.", "org.gradle.", "io.opentelemetry.",
        "net.bytebuddy.", "org.aspectj.", "se.hellsoft.vaka.",
    };

    private final List<String> inApp;

    public Exceptions(List<String> inAppPackages) {
        List<String> l = new ArrayList<>();
        for (String p : inAppPackages) {
            if (p == null || p.trim().isEmpty()) continue;
            String t = p.trim();
            while (t.endsWith(".")) t = t.substring(0, t.length() - 1);
            l.add(t + ".");
        }
        this.inApp = Collections.unmodifiableList(l);
    }

    public List<Map<String, Object>> exceptions(Throwable t) {
        Set<Throwable> seen = Collections.newSetFromMap(new IdentityHashMap<>());
        List<Map<String, Object>> out = new ArrayList<>();
        Throwable cur = t;
        while (cur != null && seen.add(cur) && out.size() < MAX_CAUSES) {
            out.add(exception(cur));
            cur = cur.getCause();
        }
        return out;
    }

    private Map<String, Object> exception(Throwable t) {
        String cls = t.getClass().getName();
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("type", cls);
        m.put("value", t.getMessage() == null ? "" : t.getMessage());
        int dot = cls.lastIndexOf('.');
        m.put("module", dot < 0 ? "" : cls.substring(0, dot));
        m.put("stacktrace", frames(t.getStackTrace()));
        return m;
    }

    public List<Map<String, Object>> frames(StackTraceElement[] stack) {
        List<Map<String, Object>> frames = new ArrayList<>(Math.min(stack.length, MAX_FRAMES));
        for (int i = 0; i < stack.length && i < MAX_FRAMES; i++) frames.add(frame(stack[i]));
        return frames;
    }

    public Map<String, Object> frame(StackTraceElement e) {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("function", e.isNativeMethod() ? e.getMethodName() + " (native)" : e.getMethodName());
        m.put("module", e.getClassName());
        if (e.getFileName() != null) m.put("filename", e.getFileName());
        if (e.getLineNumber() > 0) m.put("lineno", e.getLineNumber());
        m.put("in_app", isInApp(e.getClassName()));
        return m;
    }

    /**
     * With {@code inAppPackages} configured only those packages are app code; otherwise everything
     * except well-known runtime, framework and library packages is.
     */
    public boolean isInApp(String className) {
        if (!inApp.isEmpty()) {
            for (String p : inApp) if (className.startsWith(p)) return true;
            return false;
        }
        for (String p : SYSTEM_PREFIXES) if (className.startsWith(p)) return false;
        return true;
    }
}
