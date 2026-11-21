plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
    `java-test-fixtures`
}

kotlin {
    jvmToolchain(21)
}

dependencies {
    implementation(libs.kotlinx.serialization.json)

    // Opening sealed pushes (HPKE and the key store) serves the debug door and
    // the tests only: a shipping build carries just what a push says.
    testFixturesImplementation(libs.kotlinx.serialization.json)

    testImplementation(libs.junit)
}
