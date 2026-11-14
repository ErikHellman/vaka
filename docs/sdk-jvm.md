# JVM SDK (Java, Kotlin, Scala)

Module: `sdks/jvm` (library `se.hellsoft.vaka:vaka-jvm`, Java 11+). Step-by-step setup:
[install/jvm.md](install/jvm.md). For Android apps use the [Android SDK](sdk-android.md) instead.

The SDK is a thin layer on the standard [OpenTelemetry Java SDK](https://opentelemetry.io/docs/languages/java/):
traces and logs go to Vaka's [OTLP endpoint](opentelemetry.md), and the SDK adds what OpenTelemetry
has no model for: uncaught exception capture, breadcrumbs, release-health sessions, user and tag
context, and feature flags. It depends only on OpenTelemetry (with the JDK `HttpClient` sender, so
no OkHttp or Kotlin runtime).

## API

```java
Vaka.init(VakaOptions.builder()
    .key("vk_...")                          // or $VAKA_KEY
    .endpoint("https://vaka.example.com")   // or $VAKA_ENDPOINT
    .release("2.4.0")                       // or $VAKA_RELEASE
    .environment("production")              // or $VAKA_ENVIRONMENT
    .serviceName("checkout")                // or $VAKA_SERVICE / $OTEL_SERVICE_NAME
    .inAppPackages("com.example")
    .build());

Vaka.captureException(e);                         // handled exception with cause chain; returns the event id
Vaka.captureMessage("cache rebuilt", Level.INFO);
Vaka.addBreadcrumb("db", "connected");            // or (category, message, level, data)
Vaka.setTag("region", "eu-north-1");              // null removes it
Vaka.setUser("user-123");                         // opt-in; also sets enduser.id on the current span
Vaka.track("job_finished", Map.of("kind", "import"));
Flags flags = Vaka.flags(FlagRequest.forUser("user-123").property("plan", "pro"));
Tracer tracer = Vaka.openTelemetry().getTracer("checkout");
Vaka.flush(Duration.ofSeconds(2));
Vaka.close();                                     // also done by a shutdown hook
```

All methods are no-ops before `init`, or when the key or endpoint is empty, and safe from any thread.
Kotlin uses the same calls (`Vaka.track("signup", mapOf("plan" to "pro"))`).

### VakaOptions

| Builder method | Default | |
| --- | --- | --- |
| `key`, `endpoint` | `$VAKA_KEY`, `$VAKA_ENDPOINT` | empty disables the SDK; system properties `vaka.key`, `vaka.endpoint` work too |
| `release` | `$VAKA_RELEASE` | becomes `service.version`; release health and regressions are per release |
| `environment` | `$VAKA_ENVIRONMENT` or `production` | becomes `deployment.environment.name` |
| `serviceName` | `$VAKA_SERVICE` or `$OTEL_SERVICE_NAME` | `service.name`, and the `service` tag on errors |
| `inAppPackages` | none | your packages; without them everything except JDK, Kotlin, Spring, Netty, Jetty, Apache, Jackson, OpenTelemetry and similar library frames is `in_app` |
| `tag(k, v)` | none | tags on every error |
| `captureUncaughtExceptions` | `true` | default uncaught exception handler, chained to the previous one |
| `sessions` | `true` | one session per process run, for crash-free rates |
| `openTelemetry` | `true` | create an OpenTelemetry SDK exporting to Vaka (see below) |
| `registerGlobalOpenTelemetry` | `true` | register it as `GlobalOpenTelemetry` |
| `tracesSampleRate` | `1.0` | share of new traces recorded (parent-based) |
| `flushInterval` | 5 s | how often errors, events and sessions are sent |
| `maxQueueSize` | 1000 | items kept while the server is unreachable; oldest dropped |
| `flagsCacheTtl` | 30 s | evaluated flags are reused per identity for this long |
| `shutdownHook` | `true` | close (end session, flush) on JVM shutdown |
| `debug` | `false` | log SDK activity to stderr |

## User feedback

Forward what users wrote in your own forms (support, contact, in-app chat). The call blocks until
the server answers and carries the current scope's user, tags and flags.

```java
try {
    FeedbackResult r = Vaka.captureFeedback(Feedback.of(form.message())
        .email(form.email())            // optional contact
        .issue("API-12")                // or .eventId(Vaka.captureException(e))
        .ip(request.getRemoteAddr())
        .attachment("screenshot.png", png));
    return r.shortId;                   // "API-F12"
} catch (FeedbackException e) {
    if (e.status() == 429) { /* rate limited */ }
}
```

Builder methods: `email`, `name`, `eventId`, `issue`, `userId`, `sessionId`, `url`, `ip`, `tag`,
`attachment` (up to 3 PNG, JPEG or WebP images of 5 MB).

Link it to a crash with the event id from a capture call, or to an issue by its short id (`API-12`). Pass the end user's IP address: feedback is rate limited per address (10 an hour by default), so without it all feedback from your server shares one limit. Rejected feedback raises an error with the HTTP status; 429 means a limit was hit. See [user feedback](feedback.md).

## Scopes: users, tags and breadcrumbs per request

There is one global scope. A server should give each request its own `VakaScope`; it lives on the
OpenTelemetry `Context`, so it follows the request wherever the context is propagated (instrumented
executors, `Context.taskWrapping`, Kotlin coroutines with `asContextElement()`):

```java
try (Scope ignored = VakaScope.create().setUser(userId).setTag("route", "/carts").makeCurrent()) {
    Vaka.addBreadcrumb("http", "GET /carts/42");
    handle(request);                 // errors here carry this user, tag and breadcrumb
}
```

Errors carry the global scope merged with the current one (the current scope's values win, and
breadcrumbs from both are combined in time order, at most 100).

## OpenTelemetry

By default `init` builds an `OpenTelemetrySdk` with:

- OTLP/HTTP (protobuf, gzip) span and log exporters to `{endpoint}/ingest/otlp/v1/traces` and
  `/v1/logs`, authenticated with the ingest key;
- a resource with `service.name`, `service.version` and `deployment.environment.name`;
- W3C trace context propagation, and a parent-based ratio sampler;
- a span processor that adds `enduser.id` (from the current scope) and `session.id`.

Use it through `Vaka.openTelemetry()` or `GlobalOpenTelemetry`, and add any OpenTelemetry
instrumentation library on top of it. Errors captured while a span is active carry its `trace_id`,
so they show up on the trace, and the span gets `vaka.error_captured: true` so the server does not
make a second error out of the span's `exception` event.

**Already using OpenTelemetry?** With the Java agent, set `.openTelemetry(false)` and point the
agent at Vaka with the environment variables in [opentelemetry.md](opentelemetry.md). With your own
SDK (Spring Boot, Quarkus, manual), set `.openTelemetry(false)` and add Vaka's pieces:

```java
SdkTracerProvider.builder()
    .setResource(Resource.getDefault().merge(VakaOpenTelemetry.resource(options)))
    .addSpanProcessor(VakaOpenTelemetry.spanProcessor())
    .addSpanProcessor(BatchSpanProcessor.builder(VakaOpenTelemetry.spanExporter(options)).build());
// and VakaOpenTelemetry.logRecordExporter(options) for logs
```

## Logging

- `java.util.logging`: `Logger.getLogger("").addHandler(new VakaLogHandler())`. `INFO` and above become
  breadcrumbs, `SEVERE` becomes an error.
- Logback / Log4j 2: install the OpenTelemetry appender (`opentelemetry-logback-appender-1.0` or
  `opentelemetry-log4j-appender-2.17`) with `OpenTelemetryAppender.install(Vaka.openTelemetry())`.
  Every record becomes a searchable log on its trace, and ERROR records become Vaka errors server-side.

## Feature flags

```java
Flags flags = Vaka.flags(FlagRequest.forUser(userId).property("plan", account.plan()));
if (flags.isEnabled("dark_mode")) { ... }
String variant = flags.variant("new_checkout", "control");
```

`flags` calls `POST /ingest/v1/flags` (blocking, 5 s timeout) and caches the result per identity for
`flagsCacheTtl`. It never throws: on failure `isLoaded()` is false and every flag is off. Reading a
flag records the value on the current scope, so errors and events in the same request carry `flags`
and [flag health and experiments](flags.md) can split crash rates and conversions by variant.

## Behaviour

- **Uncaught exceptions**: reported as `fatal: true, handled: false` with tags `mechanism: uncaught`
  and `thread`, then passed to the previous handler (or printed like the JVM default). The SDK
  waits up to 2 s for the send. On the `main` thread the session also ends as `crashed`; on other
  threads the process usually keeps running, so the error counts against the session instead.
- **Sessions**: one per process. `ok` at start; at shutdown `exited`, or `errored` if errors were
  captured. Events and errors carry the session id; spans carry `session.id`.
- **Sending**: items are batched (up to 100 per request, gzip) on a daemon thread. 429/5xx and
  network errors back off exponentially up to 5 minutes (honouring `Retry-After`); 413 splits the
  batch; 401 stops sending until restart.
- **Frames** are sent as the JVM reports them (`module` = class, `function` = method, `filename`,
  `lineno`), crash site first, at most 200 per exception and 10 causes.

See `sdks/jvm/sample` for a runnable example (`VAKA_KEY=... VAKA_ENDPOINT=... ./gradlew :sample:run`).
