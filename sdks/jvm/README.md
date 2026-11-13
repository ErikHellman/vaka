# Vaka JVM SDK

`se.hellsoft.vaka:vaka-jvm`: crash reporting, breadcrumbs, release health, feature flags and
OpenTelemetry trace/log export to [Vaka](../../README.md) for Java, Kotlin and Scala services (Java 11+).

- Reference: [docs/sdk-jvm.md](../../docs/sdk-jvm.md)
- Install guide (agent-followable): [docs/install/jvm.md](../../docs/install/jvm.md)
- Releasing to Maven Central: [docs/releasing/jvm.md](../../docs/releasing/jvm.md)

```sh
./gradlew :vaka-jvm:test                    # unit tests (no network)
VAKA_TEST_ENDPOINT=http://localhost:8080 VAKA_TEST_KEY=vk_... ./gradlew :vaka-jvm:test   # plus a live server test
VAKA_ENDPOINT=http://localhost:8080 VAKA_KEY=vk_... ./gradlew :sample:run                # demo app
./gradlew publishToMavenLocal               # use an unreleased build from ~/.m2
```

Layout: `vaka-jvm/src/main/java/se/hellsoft/vaka/jvm` (public API: `Vaka`, `VakaOptions`,
`VakaScope`, `Flags`, `VakaOpenTelemetry`, `VakaLogHandler`; `internal/` is not API) and `sample/`.
The Android SDK is a separate library because it needs Android-specific capture (ANRs, NDK
crashes, lifecycle, offline disk queue); the JSON and stack-frame conventions are the same.
