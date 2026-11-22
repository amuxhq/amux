import org.gradle.api.GradleException

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.roborazzi)
}

if (file("google-services.json").isFile) {
    apply(plugin = libs.plugins.google.services.get().pluginId)
}

val releaseVersionCodeOverride = providers.gradleProperty("amux.versionCode").orNull?.toInt()
val releaseVersionNameOverride = providers.gradleProperty("amux.versionName").orNull
val releaseStoreFile = providers.gradleProperty("amux.signing.storeFile").orNull
val releaseSmoke = providers.gradleProperty("amux.releaseSmoke").isPresent

android {
    namespace = "sh.amux.android"
    compileSdk = 36

    defaultConfig {
        applicationId = "sh.amux.android"
        minSdk = 34
        targetSdk = 36
        versionCode = releaseVersionCodeOverride ?: 1
        versionName = releaseVersionNameOverride ?: "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    // The instrumented tests are the release launch smoke test, which proves
    // the R8 rules on the minified build (`just android release-smoke`).
    testBuildType = "release"

    if (releaseStoreFile != null) {
        val releaseKeyAlias = providers.gradleProperty("amux.signing.keyAlias").orNull
            ?: throw GradleException("amux.signing.storeFile is set without amux.signing.keyAlias")
        val releaseStorePassword = System.getenv("AMUX_ANDROID_STORE_PASSWORD")
            ?: throw GradleException("amux.signing.storeFile is set without AMUX_ANDROID_STORE_PASSWORD")
        val releaseKeyPassword = System.getenv("AMUX_ANDROID_KEY_PASSWORD")
            ?: throw GradleException("amux.signing.storeFile is set without AMUX_ANDROID_KEY_PASSWORD")

        signingConfigs {
            create("release") {
                storeFile = file(releaseStoreFile)
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            // Signed with the upload key when one is given. Only the launch
            // smoke test's run (`just android release-smoke`, which sets
            // amux.releaseSmoke) signs with the debug key, so the APK installs
            // on an emulator; any other release build without the upload key
            // stays unsigned, so a missing key shows here and not at Play.
            signingConfig = when {
                releaseStoreFile != null -> signingConfigs.getByName("release")
                releaseSmoke -> signingConfigs.getByName("debug")
                else -> null
            }
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            if (releaseSmoke) {
                proguardFile("release-smoke-rules.pro")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            all { test ->
                for (prop in listOf(
                    "amux.storybook.capture",
                    "amux.storybook.filter",
                    "amux.storybook.out",
                    "amux.accessibility.filter",
                )) {
                    System.getProperty(prop)?.let { value ->
                        test.systemProperty(prop, value)
                    }
                }
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
    implementation(project(":core:runtime"))
    implementation(project(":design"))
    implementation(project(":features"))
    implementation(project(":shell"))

    debugImplementation(project(":test-support"))
    testImplementation(project(":test-support"))

    implementation(platform(libs.firebase.bom))
    implementation(libs.firebase.messaging)
    // Play services, under Firebase, still ask for Fragment 1.1, whose
    // activity results lint refuses in a release build; raise it to one
    // that works with this Activity version.
    implementation(libs.androidx.fragment)

    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material.icons.extended)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)

    debugImplementation(libs.androidx.compose.ui.test.manifest)

    testImplementation(libs.junit)
    testImplementation(platform(libs.androidx.compose.bom))
    testImplementation(libs.androidx.compose.ui.test.junit4)
    testImplementation(libs.robolectric)
    testImplementation(libs.roborazzi)
    testImplementation(libs.roborazzi.compose)
    testImplementation(libs.roborazzi.junit.rule)

    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.junit)
    androidTestImplementation(libs.kotlinx.coroutines.android)
}
