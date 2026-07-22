import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Ветка on-device сборки (Termux): её скрипт линкует против /system/lib64 и
// работает только на самом Android-устройстве, поэтому одного ARM64 (os.arch
// совпадает с Apple Silicon) недостаточно — требуем ещё Linux-хост.
val armHost = System.getProperty("os.name").lowercase().contains("linux") &&
    System.getProperty("os.arch").lowercase() in setOf("aarch64", "arm64")
val signingProperties = Properties().apply {
    val propertiesFile = rootProject.file("keystore.properties")
    if (propertiesFile.isFile) propertiesFile.inputStream().use { load(it) }
}

android {
    namespace = "com.safecopy.android"
    compileSdk = 35

    if (!armHost) {
        ndkVersion = "27.2.12479018"
        externalNativeBuild {
            ndkBuild {
                path = file("src/main/jni/Android.mk")
            }
        }
    } else {
        sourceSets.getByName("main").jniLibs.srcDir(
            layout.buildDirectory.dir("generated/safecopy-jni"),
        )
    }

    defaultConfig {
        applicationId = "com.safecopy.android"
        minSdk = 29
        targetSdk = 35
        versionCode = 4
        versionName = "1.2.0"

        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    signingConfigs {
        if (signingProperties.isNotEmpty()) {
            create("release") {
                storeFile = rootProject.file(signingProperties.getProperty("storeFile"))
                storePassword = signingProperties.getProperty("storePassword")
                keyAlias = signingProperties.getProperty("keyAlias")
                keyPassword = signingProperties.getProperty("keyPassword")
                enableV1Signing = false
                enableV2Signing = true
                enableV3Signing = true
                enableV4Signing = true
            }
        }
    }

    buildTypes {
        release {
            isDebuggable = false
            isMinifyEnabled = false
            if (signingProperties.isNotEmpty()) {
                signingConfig = signingConfigs.getByName("release")
            }
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}

if (armHost) {
    val nativeSource = layout.projectDirectory.file("src/main/jni/native_io.cpp")
    val nativeOutput = layout.buildDirectory.file(
        "generated/safecopy-jni/arm64-v8a/libsafecopy_io.so",
    )
    val buildSafeCopyNative by tasks.registering(Exec::class) {
        inputs.file(nativeSource)
        inputs.file(layout.projectDirectory.file("../scripts/build-native-arm64.sh"))
        outputs.file(nativeOutput)
        commandLine(
            "/bin/bash",
            layout.projectDirectory.file("../scripts/build-native-arm64.sh").asFile,
            nativeSource.asFile,
            nativeOutput.get().asFile,
            System.getProperty("java.home"),
        )
    }
    tasks.matching { it.name == "preBuild" }.configureEach {
        dependsOn(buildSafeCopyNative)
    }
}
