# Install Vaka in a JVM service (Java, Kotlin, Scala)

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.
Requirements: Java 11+. For Android apps use the Android guide instead.

## 1. Add the dependency

Gradle (Kotlin DSL):

```kotlin
dependencies {
    implementation("se.hellsoft.vaka:vaka-jvm:0.4.0")
}
```

Maven:

```xml
<dependency>
  <groupId>se.hellsoft.vaka</groupId>
  <artifactId>vaka-jvm</artifactId>
  <version>0.4.0</version>
</dependency>
```

The artifact is on Maven Central. If it cannot be resolved (the version is not published yet), build
it from source: clone `https://github.com/ErikHellman/vaka`, run `./gradlew publishToMavenLocal` in
`sdks/jvm`, and add `mavenLocal()` to the repositories (Gradle) or rely on `~/.m2` (Maven). With
Gradle you can instead add `includeBuild("path/to/vaka/sdks/jvm")` to `settings.gradle.kts`.

The SDK brings the OpenTelemetry API and SDK (it exports traces and logs to Vaka). If the app
already uses the OpenTelemetry **Java agent**, see step 4 before continuing.

## 2. Initialise at the start of `main`

```java
import se.hellsoft.vaka.jvm.Vaka;
import se.hellsoft.vaka.jvm.VakaOptions;

public static void main(String[] args) {
    Vaka.init(VakaOptions.builder()
        .key("{{KEY}}")
        .endpoint("{{ENDPOINT}}")
        .release(Main.class.getPackage().getImplementationVersion()) // or your version string
        .serviceName("my-service")
        .inAppPackages("com.example")       // your base package
        .build());
    // ... start the app
}
```

Every option falls back to the environment (`VAKA_KEY`, `VAKA_ENDPOINT`, `VAKA_RELEASE`,
`VAKA_ENVIRONMENT`, `VAKA_SERVICE`), so the key can come from deployment config instead of source:
`Vaka.init(VakaOptions.fromEnvironment())`. With an empty key the SDK does nothing.

Kotlin: `Vaka.init(VakaOptions.builder().key("{{KEY}}").endpoint("{{ENDPOINT}}").release(BuildConfig.VERSION).build())`.

Spring Boot: call `Vaka.init(...)` in `main` before `SpringApplication.run`, and see step 4 if the
app configures OpenTelemetry itself (Micrometer tracing, the starter or the agent).

## 3. What you get automatically

- Uncaught exceptions on any thread, with cause chain and stack, sent before the thread dies.
- A session per process run (`exited`, `errored` or `crashed`), for crash-free rates per release.
- Traces: any span started with `Vaka.openTelemetry()` or `GlobalOpenTelemetry` is exported to Vaka.

## 4. If the app already uses OpenTelemetry

- **Java agent**: set `.openTelemetry(false)` and configure the agent with
  `OTEL_EXPORTER_OTLP_ENDPOINT={{ENDPOINT}}/ingest/otlp`, `OTEL_EXPORTER_OTLP_HEADERS=x-vaka-key={{KEY}}`,
  `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`, `OTEL_METRICS_EXPORTER=none`,
  `OTEL_SERVICE_NAME=...`, `OTEL_RESOURCE_ATTRIBUTES=service.version=<release>`.
- **Own SDK**: set `.openTelemetry(false)` and add `VakaOpenTelemetry.spanExporter(options)`,
  `VakaOpenTelemetry.spanProcessor()` and `VakaOpenTelemetry.resource(options)` to it
  (details in the [JVM SDK reference]({{ENDPOINT}}/docs/sdk-jvm.md)).

## 5. Report errors, context and events

```java
try {
    checkout(cart);
} catch (Exception e) {
    Vaka.captureException(e);
}
Vaka.addBreadcrumb("db", "connected");
Vaka.setTag("region", "eu-north-1");
Vaka.track("order_placed", Map.of("items", 3));
```

In a server, give each request a scope so users and breadcrumbs don't mix between requests. Put
this in a servlet filter, interceptor or middleware:

```java
try (Scope ignored = VakaScope.create().setUser(currentUserId).makeCurrent()) {
    chain.doFilter(request, response);
}
```

Identifying users is opt-in; only do it if the project's privacy policy allows it.

For `java.util.logging`, add `Logger.getLogger("").addHandler(new VakaLogHandler())`. For Logback or
Log4j 2, install the OpenTelemetry log appender with `Vaka.openTelemetry()`.

## 6. Feature flags (optional)

```java
Flags flags = Vaka.flags(FlagRequest.forUser(userId));
if (flags.isEnabled("new_checkout")) { ... }
```

## 7. Verify

Temporarily add, after `init`:

```java
Vaka.captureException(new IllegalStateException("Vaka test error"));
Vaka.flush(java.time.Duration.ofSeconds(5));
```

Run the app, then check that the error arrived:

```sh
curl -s "{{ENDPOINT}}/api/v1/projects/{{PROJECT}}/issues?since=1h" -H "Authorization: Bearer $VAKA_TOKEN"
```

or ask your agent to call the MCP tool `list_issues` for project `{{PROJECT}}`. An issue titled
`java.lang.IllegalStateException: Vaka test error` should be listed. Remove the test lines afterwards.
