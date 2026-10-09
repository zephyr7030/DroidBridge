import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.gradle.api.tasks.Copy
import org.gradle.api.tasks.Exec

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

val releaseConfig = Properties().apply {
    rootProject.file("release-config.properties").inputStream().use(::load)
}

/** The App edition's version; the APK and its native Runtime carry it. */
val productVersionName = providers.gradleProperty("droidbridgeVersionName").get()
val productVersionCode = providers.gradleProperty("droidbridgeVersionCode").get().toInt()
val rustWorkspaceVersion = Regex("""(?m)^version = "([^"]+)"""")
    .find(rootProject.file("rust/Cargo.toml").readText())
    ?.groupValues
    ?.get(1)
check(rustWorkspaceVersion == productVersionName) {
    "rust/Cargo.toml version $rustWorkspaceVersion does not carry the product version $productVersionName"
}

fun quoted(value: String): String = "\"" + value.replace("\\", "\\\\").replace("\"", "\\\"") + "\""

android {
    namespace = "com.droidbridge.standalone"
    compileSdk = 37
    buildToolsVersion = "36.0.0"
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "com.droidbridge.standalone"
        minSdk = 33
        targetSdk = 37
        versionCode = productVersionCode
        versionName = productVersionName
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        buildConfigField("String", "GITHUB_OWNER", quoted(releaseConfig.getProperty("github_owner")))
        buildConfigField("String", "GITHUB_REPO", quoted(releaseConfig.getProperty("github_repo")))
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildTypes {
        debug {
            applicationIdSuffix = ".debug"
            buildConfigField("String", "RELEASE_MANIFEST_URL", quoted("UNCONFIGURED"))
            buildConfigField("String", "RELEASE_KEY_ID", quoted("UNCONFIGURED"))
            buildConfigField("String", "RELEASE_PUBLIC_KEY_BASE64", quoted("UNCONFIGURED"))
            buildConfigField("String", "APK_SIGNER_SHA256", quoted("UNCONFIGURED"))
        }
        release {
            isMinifyEnabled = false
            isShrinkResources = false
            buildConfigField("String", "RELEASE_MANIFEST_URL", quoted(releaseConfig.getProperty("manifest_url")))
            buildConfigField("String", "RELEASE_KEY_ID", quoted(releaseConfig.getProperty("release_key_id")))
            buildConfigField("String", "RELEASE_PUBLIC_KEY_BASE64", quoted(releaseConfig.getProperty("release_public_key_base64")))
            buildConfigField("String", "APK_SIGNER_SHA256", quoted(releaseConfig.getProperty("apk_signer_sha256")))
        }
    }

    buildFeatures {
        aidl = true
        buildConfig = true
        compose = true
    }

    androidResources {
        generateLocaleConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }

    sourceSets.named("main") {
        jniLibs.srcDir(layout.buildDirectory.dir("generated/rustJniLibs").get().asFile)
        assets.srcDir(layout.buildDirectory.dir("generated/productInfoAssets").get().asFile)
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    implementation(project(":ui-common"))
    implementation(libs.adaptive)
    implementation(libs.adaptive.navigation3)
    implementation(libs.navigation3.runtime)
    implementation(libs.navigation3.ui)
    implementation(libs.lifecycle.viewmodel.navigation3)
    implementation(libs.window)
    implementation(libs.shizuku.api)
    implementation(libs.shizuku.provider)

    debugImplementation(libs.compose.ui.tooling)
    debugImplementation(libs.compose.ui.test.manifest)

    testImplementation(libs.junit4)

    androidTestImplementation(platform(libs.compose.bom))
    androidTestImplementation(libs.compose.ui.test.junit4)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.core)
    androidTestImplementation(libs.androidx.test.rules)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.espresso.core)
}

val rustJniOutput = layout.buildDirectory.dir("generated/rustJniLibs")
val ndkPath = androidComponents.sdkComponents.ndkDirectory.get().asFile.absolutePath
val rustTarget = layout.buildDirectory.dir("generated/rust")
val buildRustAndroid by tasks.registering(Exec::class) {
    workingDir(rootProject.file("rust"))
    inputs.files(rootProject.fileTree("rust/crates") { include("**/*.rs", "**/Cargo.toml") })
    inputs.files(rootProject.file("rust/Cargo.toml"), rootProject.file("rust/Cargo.lock"))
    outputs.dir(rustJniOutput)
    outputs.dir(rustTarget)
    commandLine(
        "cargo", "ndk", "-t", "arm64-v8a", "-t", "x86_64",
        "-o", rustJniOutput.get().asFile.absolutePath,
        "build", "--locked", "--release", "-p", "app_native",
    )
    environment("ANDROID_NDK_HOME", ndkPath)
    environment("ANDROID_NDK_ROOT", ndkPath)
    environment("CARGO_TARGET_DIR", rustTarget.get().asFile.absolutePath)
}

val packageRustGuard by tasks.registering(Copy::class) {
    dependsOn(buildRustAndroid)
    from(rustTarget.map { it.file("aarch64-linux-android/release/droidbridge_exec_guard") }) {
        into("arm64-v8a")
        rename { "libdroidbridge_exec_guard.so" }
    }
    from(rustTarget.map { it.file("x86_64-linux-android/release/droidbridge_exec_guard") }) {
        into("x86_64")
        rename { "libdroidbridge_exec_guard.so" }
    }
    into(rustJniOutput)
}

tasks.named("preBuild").configure { dependsOn(packageRustGuard) }

// Licenses and third-party notices ship from the same provenance the release checks use.
val copyProductInfoAssets by tasks.registering(Copy::class) {
    from(rootProject.file("tools/third-party-direct.tsv"))
    from(rootProject.file("THIRD_PARTY_NOTICES.txt"))
    into(layout.buildDirectory.dir("generated/productInfoAssets"))
}

tasks.named("preBuild").configure { dependsOn(copyProductInfoAssets) }
