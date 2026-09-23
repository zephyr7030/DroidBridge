import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.gradle.api.tasks.Copy
import org.gradle.api.tasks.Exec
import org.gradle.api.tasks.Sync
import org.gradle.api.tasks.bundling.Jar
import org.gradle.api.tasks.bundling.Zip
import org.gradle.api.tasks.compile.JavaCompile
import org.gradle.jvm.toolchain.JavaLanguageVersion
import org.gradle.jvm.toolchain.JavaToolchainService

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

val releaseConfig = Properties().apply {
    rootProject.file("release-config.properties").inputStream().use(::load)
}

/** The one product version; the APK, both module.prop files and the Rust workspace carry it. */
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
    namespace = "com.droidbridge.android"
    compileSdk = 37
    buildToolsVersion = "36.0.0"
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "com.droidbridge.android"
        minSdk = 33
        targetSdk = 37
        versionCode = productVersionCode
        versionName = productVersionName
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        buildConfigField("String", "GITHUB_OWNER", quoted(releaseConfig.getProperty("github_owner")))
        buildConfigField("String", "GITHUB_REPO", quoted(releaseConfig.getProperty("github_repo")))
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    buildTypes {
        debug {
            applicationIdSuffix = ".debug"
            ndk {
                abiFilters += "x86_64"
            }
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
    implementation(platform(libs.kotlin.bom))
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.foundation)
    implementation(libs.compose.material3)
    implementation(libs.compose.material3.adaptive.navigation.suite)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.adaptive)
    implementation(libs.adaptive.navigation3)
    implementation(libs.navigation3.runtime)
    implementation(libs.navigation3.ui)
    implementation(libs.activity.compose)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.lifecycle.viewmodel.navigation3)
    implementation(libs.window)
    implementation(libs.androidx.core)
    implementation(libs.datastore.preferences)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)
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
val sdkDirectory = androidComponents.sdkComponents.sdkDirectory
val ndkDirectory = androidComponents.sdkComponents.ndkDirectory
val sdkPath = sdkDirectory.get().asFile
val ndkPath = ndkDirectory.get().asFile.absolutePath
val buildRustAndroid by tasks.registering(Exec::class) {
    workingDir(rootProject.file("rust"))
    inputs.files(rootProject.fileTree("rust/crates") { include("**/*.rs", "**/Cargo.toml") })
    inputs.files(rootProject.file("rust/Cargo.toml"), rootProject.file("rust/Cargo.lock"))
    outputs.dir(rustJniOutput)
    commandLine(
        "cargo", "ndk", "-t", "arm64-v8a", "-t", "x86_64",
        "-o", rustJniOutput.get().asFile.absolutePath,
        "build", "--locked", "--release", "-p", "app_native"
    )
    environment("ANDROID_NDK_HOME", ndkPath)
    environment("ANDROID_NDK_ROOT", ndkPath)
}

val packageRustGuard by tasks.registering(Copy::class) {
    dependsOn(buildRustAndroid)
    from(rootProject.file("rust/target/aarch64-linux-android/release/droidbridge_exec_guard")) {
        into("arm64-v8a")
        rename { "libdroidbridge_exec_guard.so" }
    }
    from(rootProject.file("rust/target/x86_64-linux-android/release/droidbridge_exec_guard")) {
        into("x86_64")
        rename { "libdroidbridge_exec_guard.so" }
    }
    into(rustJniOutput)
}

tasks.named("preBuild").configure { dependsOn(packageRustGuard) }


// S-UI-017 Licenses and third-party notices ship from the same provenance the release checks use.
val copyProductInfoAssets by tasks.registering(Copy::class) {
    from(rootProject.file("tools/third-party-direct.tsv"))
    from(rootProject.file("THIRD_PARTY_NOTICES.txt"))
    into(layout.buildDirectory.dir("generated/productInfoAssets"))
}

tasks.named("preBuild").configure { dependsOn(copyProductInfoAssets) }

val helperCommonSources = rootProject.file("magisk/framework-src/common")
val javaToolchains = extensions.getByType(JavaToolchainService::class.java)
val java17Compiler = javaToolchains.compilerFor {
    languageVersion.set(JavaLanguageVersion.of(17))
}
val helperJars = (33..37).associateWith { api ->
    val classesDirectory = layout.buildDirectory.dir("generated/magiskFramework/api$api/classes")
    val compileHelper = tasks.register<JavaCompile>("compileMagiskFrameworkApi$api") {
        val stubs = rootProject.file("magisk/framework-stubs/api$api")
        source(
            fileTree(helperCommonSources) { include("**/*.java") },
            fileTree(rootProject.file("magisk/framework-src/api$api")) { include("**/*.java") },
        )
        inputs.dir(stubs)
        classpath = files(sdkPath.resolve("platforms/android-$api/android.jar"))
        options.sourcepath = files(stubs)
        options.compilerArgs.add("-implicit:none")
        destinationDirectory.set(classesDirectory)
        javaCompiler.set(java17Compiler)
        sourceCompatibility = "17"
        targetCompatibility = "17"
        options.encoding = "UTF-8"
    }
    val classesJarDirectory = layout.buildDirectory.dir("generated/magiskFramework/api$api")
    val classesJar = classesJarDirectory.map { it.file("classes.jar") }
    val jarHelper = tasks.register<Jar>("jarMagiskFrameworkApi$api") {
        dependsOn(compileHelper)
        from(classesDirectory)
        archiveFileName.set("classes.jar")
        destinationDirectory.set(classesJarDirectory)
    }
    val dexDirectory = layout.buildDirectory.dir("generated/magiskFramework/api$api/dex")
    val dexHelper = tasks.register<Exec>("dexMagiskFrameworkApi$api") {
        dependsOn(jarHelper)
        inputs.file(classesJar)
        outputs.dir(dexDirectory)
        commandLine(
            sdkPath.resolve("build-tools/36.0.0/d8.bat").absolutePath,
            "--min-api", "33",
            "--output", dexDirectory.get().asFile.absolutePath,
            classesJar.get().asFile.absolutePath,
        )
    }
    tasks.register<Zip>("packageMagiskFrameworkApi$api") {
        dependsOn(dexHelper)
        from(dexDirectory.map { it.file("classes.dex") })
        archiveFileName.set("droidbridge-framework-api$api.jar")
        destinationDirectory.set(layout.buildDirectory.dir("generated/magiskFramework/jars"))
    }
}

fun registerMagiskModule(variant: String, debugModule: Boolean) {
    val capitalized = variant.replaceFirstChar(Char::uppercase)
    val rustTarget = layout.buildDirectory.dir("generated/magiskRust/$variant")
    val rustTargetPath = rustTarget.get().asFile.absolutePath
    val buildRust = tasks.register<Exec>("build${capitalized}MagiskRust") {
        workingDir(rootProject.file("rust"))
        inputs.files(rootProject.fileTree("rust/crates") { include("**/*.rs", "**/Cargo.toml") })
        inputs.files(rootProject.file("rust/Cargo.toml"), rootProject.file("rust/Cargo.lock"))
        outputs.dir(rustTarget)
        val features = if (debugModule) listOf("--features", "daemon/debug-module") else emptyList()
        commandLine(
            listOf(
                "cargo", "ndk", "-t", "arm64-v8a", "--platform", "33", "build", "--locked", "--release",
                "-p", "daemon", "-p", "supervisor", "-p", "app_native", "--bins",
            ) + features,
        )
        environment("ANDROID_NDK_HOME", ndkPath)
        environment("ANDROID_NDK_ROOT", ndkPath)
        environment("CARGO_TARGET_DIR", rustTargetPath)
    }
    val staging = layout.buildDirectory.dir("generated/magiskModule/$variant")
    // The staged module carries the product version the APK carries.
    val stampedVersionName = productVersionName
    val stampedVersionCode = productVersionCode
    val stageModule = tasks.register<Sync>("stage${capitalized}MagiskModule") {
        dependsOn(buildRust)
        dependsOn(helperJars.values)
        from(rootProject.file("magisk")) {
            exclude("framework-src/**")
            exclude("framework-stubs/**")
            if (debugModule) exclude("module.prop")
        }
        from(rootProject.file("THIRD_PARTY_NOTICES.txt"))
        from(rustTarget.map { it.file("aarch64-linux-android/release/droidbridge-supervisor") }) {
            into("bin")
        }
        from(rustTarget.map { it.file("aarch64-linux-android/release/droidbridged") }) {
            into("bin")
        }
        from(rustTarget.map { it.file("aarch64-linux-android/release/droidbridge_exec_guard") }) {
            into("bin")
            rename { "droidbridge-exec-guard" }
        }
        helperJars.forEach { (api, task) ->
            from(task.flatMap { it.archiveFile }) {
                into("framework")
                rename { "droidbridge-framework-api$api.jar" }
            }
        }
        into(staging)
        val template = rootProject.file("magisk/module.prop")
        doLast {
            // The staged module.prop carries the one product version, written with LF endings
            // because the module ZIP is byte-checked and its scripts are LF-only.
            val lines = if (debugModule) {
                listOf(
                    "id=droidbridge_debug",
                    "name=DroidBridge Debug",
                    "version=",
                    "versionCode=",
                    "author=DroidBridge",
                    "description=DroidBridge debug privileged Android backend",
                )
            } else {
                template.readLines()
            }
            val stamped = lines.map { line ->
                when {
                    line.startsWith("version=") -> "version=$stampedVersionName"
                    line.startsWith("versionCode=") -> "versionCode=$stampedVersionCode"
                    else -> line
                }
            }
            staging.get().file("module.prop").asFile
                .writeText(stamped.joinToString("\n", postfix = "\n"))
        }
    }
    tasks.register<Zip>("assemble${capitalized}MagiskModule") {
        dependsOn(stageModule)
        from(staging)
        archiveFileName.set("droidbridge-$variant-magisk.zip")
        destinationDirectory.set(layout.buildDirectory.dir("outputs/magisk"))
    }
}

registerMagiskModule("stable", false)
registerMagiskModule("debug", true)
