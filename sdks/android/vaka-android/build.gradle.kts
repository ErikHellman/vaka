plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
    `maven-publish`
}

val sdkVersion = providers.gradleProperty("VERSION_NAME").get()
group = "se.hellsoft.vaka"
version = sdkVersion

android {
    namespace = "se.hellsoft.vaka"
    compileSdk = 35

    defaultConfig {
        minSdk = 21
        consumerProguardFiles("consumer-rules.pro")
        buildConfigField("String", "SDK_VERSION", "\"$sdkVersion\"")
    }

    buildFeatures { buildConfig = true }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    testOptions {
        unitTests.all { test ->
            // Integration test against a running server: VAKA_TEST_ENDPOINT and VAKA_TEST_KEY.
            listOf("VAKA_TEST_ENDPOINT", "VAKA_TEST_KEY").forEach { name ->
                System.getenv(name)?.let { test.environment(name, it) }
            }
            test.testLogging { events("passed", "skipped", "failed"); showStandardStreams = true }
        }
    }

    publishing {
        singleVariant("release") { withSourcesJar() }
    }
}

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_1_8) }
    explicitApi()
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}

publishing {
    publications {
        register<MavenPublication>("release") {
            groupId = "se.hellsoft.vaka"
            artifactId = "vaka-android"
            version = sdkVersion
            afterEvaluate { from(components["release"]) }
            pom {
                name.set("Vaka Android SDK")
                description.set("Crash reporting, release health and privacy-friendly analytics for Android.")
                url.set("https://github.com/ErikHellman/vaka")
                licenses {
                    license { name.set("MIT"); url.set("https://opensource.org/licenses/MIT") }
                    license { name.set("Apache-2.0"); url.set("https://www.apache.org/licenses/LICENSE-2.0") }
                }
            }
        }
    }
}
