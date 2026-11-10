// Sample app: shows the integration and exercises R8 + the mapping upload plugin.
//   VAKA_URL=http://localhost:8080 VAKA_PROJECT=android-app VAKA_TOKEN=vt_... ./gradlew :sample:assembleRelease
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("se.hellsoft.vaka")
}

android {
    namespace = "se.hellsoft.vaka.sample"
    compileSdk = 35

    defaultConfig {
        applicationId = "se.hellsoft.vaka.sample"
        minSdk = 21
        targetSdk = 35
        versionCode = 1
        versionName = "1.0.0"
        buildConfigField("String", "VAKA_ENDPOINT", "\"${providers.gradleProperty("vaka.sample.endpoint").getOrElse("http://10.0.2.2:8080")}\"")
        buildConfigField("String", "VAKA_KEY", "\"${providers.gradleProperty("vaka.sample.key").getOrElse("vk_replace_me")}\"")
    }

    buildFeatures { buildConfig = true }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
}

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_1_8) }
}

vaka {
    failOnError.set(providers.environmentVariable("VAKA_TOKEN").isPresent)
}

dependencies {
    implementation(project(":vaka-android"))
}
