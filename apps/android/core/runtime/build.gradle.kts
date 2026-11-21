import org.gradle.api.GradleException
import org.gradle.api.tasks.PathSensitivity
import org.gradle.internal.os.OperatingSystem

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.serialization)
}

val repoRoot = rootProject.projectDir.parentFile.parentFile
val debugJniLibsDir = repoRoot.resolve("target/android/debug-jniLibs")
val releaseJniLibsDir = repoRoot.resolve("target/android/release-jniLibs")

android {
    namespace = "sh.amux.core.runtime"
    compileSdk = 36

    defaultConfig {
        minSdk = 34
    }

    sourceSets {
        getByName("main") {
            jniLibs.setSrcDirs(emptyList<Any>())
        }
        getByName("debug") {
            jniLibs.setSrcDirs(listOf(debugJniLibsDir))
        }
        getByName("release") {
            jniLibs.setSrcDirs(listOf(releaseJniLibsDir))
        }
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
                val hostLib = repoRoot.resolve("target/android/host-jni/$libName")
                test.systemProperty("amux.jni.lib", hostLib.absolutePath)
                test.inputs.file(hostLib)
                    .withPropertyName("amuxHostJniLib")
                    .withPathSensitivity(PathSensitivity.NONE)
                    .optional()
            }
        }
    }
}

val verifyReleaseJniLibs = tasks.register("verifyReleaseJniLibs") {
    description = "Verify shipping libamux_app.so is staged under target/android/release-jniLibs before release packaging."
    outputs.upToDateWhen { false }
    doLast {
        val libs = if (releaseJniLibsDir.isDirectory) {
            releaseJniLibsDir.listFiles()
                ?.filter { it.isDirectory }
                ?.map { it.resolve("libamux_app.so") }
                ?.filter { it.isFile }
                .orEmpty()
        } else {
            emptyList()
        }
        if (libs.isEmpty()) {
            throw GradleException(
                "Missing shipping JNI library in ${releaseJniLibsDir.absolutePath} " +
                    "(expected <abi>/libamux_app.so). Run `just android rust --release` before assembling a release build.",
            )
        }
    }
}

tasks.matching {
    it.name == "mergeReleaseJniLibFolders"
}.configureEach {
    dependsOn(verifyReleaseJniLibs)
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
