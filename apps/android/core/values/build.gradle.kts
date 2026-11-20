plugins {
    `java-library`
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

kotlin {
    jvmToolchain(21)
}

dependencies {
    api(project(":push"))
    implementation(libs.kotlinx.serialization.json)

    testImplementation(libs.junit)
}
