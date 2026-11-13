package se.hellsoft.vaka.jvm;

import java.util.LinkedHashMap;
import java.util.Map;
import java.util.logging.Handler;
import java.util.logging.LogRecord;

/**
 * A {@code java.util.logging} handler: records at {@code INFO} and above become breadcrumbs, and
 * {@code SEVERE} records become Vaka errors (with the attached exception, if any).
 *
 * <pre>{@code
 * java.util.logging.Logger.getLogger("").addHandler(new VakaLogHandler());
 * }</pre>
 *
 * For Logback or Log4j, use the OpenTelemetry log appenders with {@link Vaka#openTelemetry()}:
 * ERROR records become Vaka errors server-side and every record is searchable as a log.
 */
public final class VakaLogHandler extends Handler {
    private final java.util.logging.Level breadcrumbLevel;
    private final java.util.logging.Level errorLevel;

    public VakaLogHandler() { this(java.util.logging.Level.INFO, java.util.logging.Level.SEVERE); }

    public VakaLogHandler(java.util.logging.Level breadcrumbLevel, java.util.logging.Level errorLevel) {
        this.breadcrumbLevel = breadcrumbLevel;
        this.errorLevel = errorLevel;
    }

    @Override
    public void publish(LogRecord record) {
        if (record == null || !isLoggable(record)) return;
        VakaClient c = Vaka.client();
        if (c == null) return;
        String logger = record.getLoggerName() == null ? "" : record.getLoggerName();
        // Don't report the SDK's own (or OpenTelemetry's export) problems back to Vaka.
        if (logger.startsWith("se.hellsoft.vaka") || logger.startsWith("io.opentelemetry")) return;
        int value = record.getLevel().intValue();
        String message = message(record);
        if (value >= errorLevel.intValue()) {
            if (record.getThrown() != null) {
                c.captureException(record.getThrown(), Level.ERROR);
            } else {
                c.captureMessage(message, Level.ERROR);
            }
        } else if (value >= breadcrumbLevel.intValue()) {
            Map<String, Object> data = new LinkedHashMap<>();
            if (!logger.isEmpty()) data.put("logger", logger);
            c.addBreadcrumb("log", message, level(value), data);
        }
    }

    private String message(LogRecord r) {
        try {
            java.util.logging.Formatter f = getFormatter();
            if (f != null) return f.formatMessage(r);
            String msg = r.getMessage();
            Object[] params = r.getParameters();
            if (msg != null && params != null && params.length > 0) return java.text.MessageFormat.format(msg, params);
            return msg == null ? "" : msg;
        } catch (RuntimeException e) {
            return r.getMessage() == null ? "" : r.getMessage();
        }
    }

    private static Level level(int v) {
        if (v >= java.util.logging.Level.SEVERE.intValue()) return Level.ERROR;
        if (v >= java.util.logging.Level.WARNING.intValue()) return Level.WARNING;
        if (v >= java.util.logging.Level.INFO.intValue()) return Level.INFO;
        return Level.DEBUG;
    }

    @Override
    public void flush() {}

    @Override
    public void close() {}
}
