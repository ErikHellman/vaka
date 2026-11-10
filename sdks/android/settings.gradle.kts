pluginManagement {
    includeBuild("vaka-gradle-plugin")
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "vaka-android-sdk"
include(":vaka-android")
include(":sample")
