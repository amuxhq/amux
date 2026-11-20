import org.gradle.internal.os.OperatingSystem

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "sh.amux.core.runtime"
    compileSdk = 36

    defaultConfig {
        minSdk = 34
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }

    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            all { test ->
                val os = OperatingSystem.current()
                val libName = when {
                    os.isMacOsX -> "libamux_app.dylib"
                    os.isWindows -> "amux_app.dll"
                    else -> "libamux_app.so"
                }
                val repoRoot = rootProject.projectDir.parentFile.parentFile
                val hostLib = repoRoot.resolve("target/android/host-jni/$libName")
                test.systemProperty("amux.jni.lib", hostLib.absolutePath)
            }
        }
    }
}

kotlin {
    jvmToolchain(21)
}

dependencies {
    implementation(project(":core:values"))
    implementation(project(":core:words"))
    implementation(project(":push"))

    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.okhttp)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.robolectric)
}
