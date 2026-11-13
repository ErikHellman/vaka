plugins {
    `java-library`
    id("com.vanniktech.maven.publish")
}

val sdkVersion = providers.gradleProperty("VERSION_NAME").get()
group = providers.gradleProperty("GROUP").get()
version = sdkVersion

tasks.withType<JavaCompile>().configureEach { options.release.set(11) }
tasks.compileJava { options.compilerArgs.addAll(listOf("-Xlint:all,-serial,-options,-try", "-Werror")) }

val otelVersion = "1.68.0"

dependencies {
    // Part of the public API: Vaka.openTelemetry() and VakaScope live on the OpenTelemetry Context.
    api("io.opentelemetry:opentelemetry-api:$otelVersion")
    api("io.opentelemetry:opentelemetry-sdk:$otelVersion")
    implementation("io.opentelemetry:opentelemetry-exporter-otlp:$otelVersion") {
        // Use the JDK HttpClient instead of pulling OkHttp and Kotlin into server apps.
        exclude(group = "io.opentelemetry", module = "opentelemetry-exporter-sender-okhttp")
    }
    implementation("io.opentelemetry:opentelemetry-exporter-sender-jdk:$otelVersion")

    testImplementation(platform("org.junit:junit-bom:5.13.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

// Version constant compiled into the jar (read by Vaka.SDK_VERSION).
val generateVersion by tasks.registering {
    val out = layout.buildDirectory.dir("generated/resources/vaka")
    inputs.property("version", sdkVersion)
    outputs.dir(out)
    doLast {
        val f = out.get().file("se/hellsoft/vaka/jvm/version.txt").asFile
        f.parentFile.mkdirs()
        f.writeText(sdkVersion)
    }
}
sourceSets.main { resources.srcDir(generateVersion) }

tasks.test {
    useJUnitPlatform()
    // Integration test against a running server: VAKA_TEST_ENDPOINT and VAKA_TEST_KEY.
    listOf("VAKA_TEST_ENDPOINT", "VAKA_TEST_KEY").forEach { name ->
        System.getenv(name)?.let { environment(name, it) }
    }
    testLogging { events("passed", "skipped", "failed"); exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL }
}

tasks.withType<Javadoc>().configureEach {
    (options as StandardJavadocDocletOptions).addStringOption("Xdoclint:none", "-quiet")
}

mavenPublishing {
    // publishAndReleaseToMavenCentral releases right away; publishToMavenCentral only uploads.
    publishToMavenCentral()
    // Signing is configured from signingInMemoryKey / signingInMemoryKeyPassword; it is only
    // required when publishing to Maven Central (see docs/releasing/jvm.md).
    if (providers.gradleProperty("signingInMemoryKey").isPresent) signAllPublications()
    coordinates(group.toString(), "vaka-jvm", sdkVersion)
    pom {
        name.set("Vaka JVM SDK")
        description.set("Crash reporting, breadcrumbs, feature flags and OpenTelemetry export to Vaka for Java and Kotlin services.")
        inceptionYear.set("2026")
        url.set("https://github.com/ErikHellman/vaka")
        licenses {
            license { name.set("MIT"); url.set("https://opensource.org/licenses/MIT"); distribution.set("repo") }
            license { name.set("Apache-2.0"); url.set("https://www.apache.org/licenses/LICENSE-2.0"); distribution.set("repo") }
        }
        developers {
            developer { id.set("ErikHellman"); name.set("Erik Hellman"); url.set("https://github.com/ErikHellman") }
        }
        scm {
            url.set("https://github.com/ErikHellman/vaka")
            connection.set("scm:git:https://github.com/ErikHellman/vaka.git")
            developerConnection.set("scm:git:ssh://git@github.com/ErikHellman/vaka.git")
        }
    }
}
