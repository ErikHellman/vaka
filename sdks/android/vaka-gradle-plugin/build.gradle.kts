plugins {
    `kotlin-dsl`
    `maven-publish`
}

group = "se.hellsoft.vaka"
version = "0.4.0"

dependencies {
    compileOnly("com.android.tools.build:gradle-api:8.7.3")
    testImplementation(gradleTestKit())
    testImplementation("junit:junit:4.13.2")
}

java {
    sourceCompatibility = JavaVersion.VERSION_11
    targetCompatibility = JavaVersion.VERSION_11
}

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_11) }
}

gradlePlugin {
    plugins {
        create("vaka") {
            id = "se.hellsoft.vaka"
            implementationClass = "se.hellsoft.vaka.gradle.VakaPlugin"
            displayName = "Vaka R8 mapping upload"
            description = "Uploads R8/ProGuard mapping files to a Vaka server after minification."
        }
    }
}
