plugins { id("com.android.library") }

android {
    namespace = "androidx.media3.decoder.ffmpeg"
    compileSdk = 36
    buildToolsVersion = "36.0.0"
    defaultConfig { minSdk = 29 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    sourceSets.getByName("main") {
        java.srcDir("../Vendor/media/libraries/decoder_ffmpeg/src/main/java")
        jniLibs.srcDir("../Vendor/jniLibs")
    }
}

dependencies {
    api("androidx.media3:media3-decoder:1.8.0")
    implementation("androidx.media3:media3-exoplayer:1.8.0")
    implementation("androidx.annotation:annotation:1.9.1")
    compileOnly("org.checkerframework:checker-qual:3.49.2")
}

// An APK advertised as supporting ALAC must never silently omit its decoder.
tasks.named("preBuild") {
    doFirst {
        listOf("armeabi-v7a", "arm64-v8a", "x86_64").forEach {
            check(file("../Vendor/jniLibs/$it/libffmpegJNI.so").isFile) {
                "Missing ALAC decoder for $it. Run android/scripts/build-decoder.sh first."
            }
        }
    }
}
