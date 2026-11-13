package se.hellsoft.vaka.jvm;

/** Severity of an error, message or breadcrumb. */
public enum Level {
    DEBUG("debug"), INFO("info"), WARNING("warning"), ERROR("error"), FATAL("fatal");

    final String wire;

    Level(String wire) { this.wire = wire; }

    /** The value sent to Vaka ({@code "error"}, {@code "warning"}, ...). */
    public String wire() { return wire; }

    /** Errors and fatals count against the session (release health). */
    boolean isError() { return this == ERROR || this == FATAL; }
}
