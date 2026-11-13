package se.hellsoft.vaka.jvm.internal;

/** SDK diagnostics on stderr: warnings always, debug output only with {@code debug(true)}. */
public final class Logger {
    private final boolean debug;

    public Logger(boolean debug) { this.debug = debug; }

    public boolean isDebug() { return debug; }

    public void debug(String msg) {
        if (debug) System.err.println("[vaka] " + msg);
    }

    public void warn(String msg) {
        System.err.println("[vaka] warning: " + msg);
    }

    public void warn(String msg, Throwable t) {
        System.err.println("[vaka] warning: " + msg + ": " + t);
        if (debug) t.printStackTrace(System.err);
    }
}
