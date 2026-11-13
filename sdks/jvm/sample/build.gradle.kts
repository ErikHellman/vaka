plugins {
    application
}

dependencies {
    implementation(project(":vaka-jvm"))
}

application {
    mainClass.set("se.hellsoft.vaka.sample.Demo")
}

tasks.withType<JavaCompile>().configureEach { options.release.set(11) }
