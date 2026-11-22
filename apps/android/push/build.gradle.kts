plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

kotlin {
    jvmToolchain(21)
}

// The pushes the daemon's tests seal (crates/node/src/seal.rs) and the iPhone's
// open: reading the same file holds all three to one format.
sourceSets.named("test") {
    resources.srcDir(rootDir.resolve("../apple/Packages/AmuxPush/Tests/AmuxPushTests/Fixtures"))
}

dependencies {
    implementation(libs.kotlinx.serialization.json)

    testImplementation(libs.junit)
}
