using Microsoft.Extensions.Logging;

namespace Vaka;

/// <summary>Options for <see cref="VakaLoggerProvider"/>.</summary>
public sealed class VakaLoggingOptions
{
    /// <summary>Log entries at or above this level become breadcrumbs. Default <see cref="LogLevel.Information"/>.</summary>
    public LogLevel MinimumBreadcrumbLevel { get; set; } = LogLevel.Information;

    /// <summary>Log entries at or above this level are reported as Vaka errors. Default <see cref="LogLevel.Error"/>.
    /// Set to <see cref="LogLevel.None"/> to only record breadcrumbs.</summary>
    public LogLevel MinimumEventLevel { get; set; } = LogLevel.Error;
}

/// <summary>
/// <see cref="ILoggerProvider"/> that turns log entries into breadcrumbs, and errors into Vaka errors
/// (with the logged exception's stack trace). Register it with <c>logging.AddProvider(new VakaLoggerProvider())</c>,
/// or use <c>builder.AddVaka()</c> from <c>Vaka.AspNetCore</c>.
/// </summary>
public sealed class VakaLoggerProvider(VakaLoggingOptions? options = null) : ILoggerProvider
{
    private readonly VakaLoggingOptions _options = options ?? new VakaLoggingOptions();

    /// <inheritdoc />
    public ILogger CreateLogger(string categoryName) => new VakaLogger(categoryName, _options);

    /// <inheritdoc />
    public void Dispose() { }

    private sealed class VakaLogger(string category, VakaLoggingOptions options) : ILogger
    {
        public IDisposable? BeginScope<TState>(TState state) where TState : notnull => null;

        public bool IsEnabled(LogLevel logLevel) =>
            logLevel != LogLevel.None && VakaSdk.IsEnabled &&
            (logLevel >= options.MinimumBreadcrumbLevel || logLevel >= options.MinimumEventLevel);

        public void Log<TState>(LogLevel logLevel, EventId eventId, TState state, Exception? exception, Func<TState, Exception?, string> formatter)
        {
            if (!IsEnabled(logLevel)) return;
            string message;
            try { message = formatter(state, exception); } catch { message = state?.ToString() ?? ""; }
            var level = Map(logLevel);
            if (logLevel >= options.MinimumEventLevel && options.MinimumEventLevel != LogLevel.None)
            {
                void Configure(Scope s)
                {
                    s.SetTag("logger", category);
                    if (eventId.Id != 0) s.SetTag("log.event_id", eventId.Id.ToString());
                }
                if (exception is not null) VakaSdk.CaptureException(exception, level, handled: !ReportsUnhandled(category), Configure);
                else VakaSdk.CaptureMessage(message, level, Configure);
            }
            if (logLevel >= options.MinimumBreadcrumbLevel)
            {
                var data = exception is null ? null : new Dictionary<string, object?> { ["exception"] = exception.GetType().FullName };
                VakaSdk.AddBreadcrumb(message, category, level, data);
            }
        }

        // Framework loggers that log exceptions nobody in the app caught.
        private static bool ReportsUnhandled(string category) => category is
            "Microsoft.AspNetCore.Diagnostics.ExceptionHandlerMiddleware" or
            "Microsoft.AspNetCore.Diagnostics.DeveloperExceptionPageMiddleware" or
            "Microsoft.AspNetCore.Server.Kestrel" or
            "Microsoft.Extensions.Hosting.Internal.Host" or
            "Microsoft.Extensions.Hosting.Internal.BackgroundService";

        private static VakaLevel Map(LogLevel l) => l switch
        {
            LogLevel.Trace or LogLevel.Debug => VakaLevel.Debug,
            LogLevel.Information => VakaLevel.Info,
            LogLevel.Warning => VakaLevel.Warning,
            // Critical logs are severe but not a crash; only unhandled exceptions are reported as fatal.
            _ => VakaLevel.Error,
        };
    }
}
