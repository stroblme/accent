plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.compose.compiler)
}

// The repository root: the Rust workspace this app is a front end for.
val repo: File = rootProject.projectDir.parentFile
val abis = listOf("arm64-v8a", "x86_64")
// A plain path, not a provider: AGP 9's source-set API takes only the first.
val bindings = "build/generated/uniffi"

android {
    namespace = "io.github.stroblme.accent"
    // 37.2 because compose-foundation 1.13 asks for at least 37.1; the app itself targets 36
    // and runs from 31, which are the two numbers that decide what it may call and where it
    // installs. What it compiles against only has to be new enough for its dependencies.
    compileSdk = 37
    compileSdkMinor = 2

    defaultConfig {
        applicationId = "io.github.stroblme.accent"
        // 31 is where Material You reads the system colours, which is where the app takes its
        // accent from, the way the desktop takes GNOME's.
        minSdk = 31
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    buildFeatures { compose = true }

    // One APK per ABI instead of one carrying both: the second copy of the two libraries packed
    // below is 11.6 MB no device can use. An app bundle would defer the same split to Play, which
    // this app is not released through; a split APK installs as it is. No universal APK either —
    // the two ABIs answer that themselves: arm64-v8a is every phone, x86_64 is the emulator.
    //
    // This list is also the only one: AGP refuses `ndk.abiFilters` beside a split, and the split
    // does that job too — without it JNA's four ABIs would each get an APK of their own.
    splits {
        abi {
            isEnable = true
            reset()
            include(*abis.toTypedArray())
        }
    }

    buildTypes {
        release {
            // R8, with what uniffi and JNA reach by name kept whole (`proguard-rules.pro`): a
            // rename there breaks the core when its class initialises, not at build time.
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    packaging {
        // Uncompressed and page-aligned in the APK, which is what a 16 KB device needs in order
        // to map the libraries at all. See `.cargo/config.toml` for the other half.
        jniLibs { useLegacyPackaging = false }
    }

    sourceSets["main"].kotlin.directories.add(bindings)
}

// The Rust core, cross-compiled into `src/main/jniLibs/<abi>/`. Gradle does not know how to build
// Rust and does not try: it runs the same `make` targets a developer would.
val cargoNdk = tasks.register<Exec>("cargoNdk") {
    workingDir = repo
    commandLine("make", "android")
    inputs.dir(repo.resolve("crates"))
    inputs.dir(repo.resolve("android/ffi"))
    outputs.dir(repo.resolve("android/app/src/main/jniLibs"))
}

// The Kotlin the app calls the core through, read out of the library that was just built.
val uniffiBindgen = tasks.register<Exec>("uniffiBindgen") {
    dependsOn(cargoNdk)
    workingDir = repo
    commandLine("make", "bindings")
    // Not what it reads — that is a host build, since the shipped library is stripped of the
    // symbols uniffi keeps its metadata in — but it moves whenever the surface can have moved.
    // Without it Gradle calls the task up to date whenever the output directory exists.
    inputs.file(repo.resolve("android/app/src/main/jniLibs/arm64-v8a/libaccent_android.so"))
    outputs.dir(layout.projectDirectory.dir(bindings))
}

tasks.named("preBuild") { dependsOn(uniffiBindgen) }

dependencies {
    implementation(libs.compose.foundation)
    implementation(libs.compose.ui)
    implementation(libs.compose.material3)
    implementation(libs.activity.compose)
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.lifecycle.runtime.compose)
    // uniffi's generated bindings call the library through JNA's direct mapping.
    implementation(variantOf(libs.jna) { artifactType("aar") })
    testImplementation(libs.junit)
}
