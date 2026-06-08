import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.serialization")
    id("org.jetbrains.kotlin.plugin.compose")
}

val officeClimateBuildHash = (
    project.findProperty("officeClimateBuildHash") as String?
        ?: System.getenv("OFFICE_CLIMATE_BUILD_HASH")
        ?: ""
)

val releaseKeystoreFile = rootProject.file("../certs/android-release.jks")
val releaseKeystorePropertiesFile = rootProject.file("../certs/android-release.properties")
val releaseKeystoreProperties = Properties().apply {
    if (releaseKeystorePropertiesFile.exists()) {
        releaseKeystorePropertiesFile.inputStream().use { load(it) }
    }
}
val releaseSigningError = when {
    !releaseKeystoreFile.exists() ->
        "Missing release keystore at ${releaseKeystoreFile.absolutePath}."
    !releaseKeystorePropertiesFile.exists() ->
        "Missing release signing properties at ${releaseKeystorePropertiesFile.absolutePath}."
    releaseKeystoreProperties.getProperty("storePassword").isNullOrBlank() ->
        "Missing storePassword in ${releaseKeystorePropertiesFile.absolutePath}."
    releaseKeystoreProperties.getProperty("keyAlias").isNullOrBlank() ->
        "Missing keyAlias in ${releaseKeystorePropertiesFile.absolutePath}."
    else -> null
}
val releaseSigningConfigured = releaseSigningError == null

android {
    namespace = "com.rajesh.officeclimate"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.rajesh.officeclimate"
        minSdk = 26
        targetSdk = 35
        versionCode = 2
        versionName = "1.0.1"
        buildConfigField("String", "APK_HASH", "\"$officeClimateBuildHash\"")
    }

    signingConfigs {
        if (releaseSigningConfigured) {
            create("release") {
                val storePassword = releaseKeystoreProperties.getProperty("storePassword")
                val keyAlias = releaseKeystoreProperties.getProperty("keyAlias")
                val keyPassword = releaseKeystoreProperties.getProperty("keyPassword")
                    ?: storePassword

                storeFile = releaseKeystoreFile
                this.storePassword = storePassword
                this.keyAlias = keyAlias
                this.keyPassword = keyPassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (releaseSigningConfigured) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }
}

tasks.configureEach {
    if (name.contains("Release")) {
        doFirst {
            if (!releaseSigningConfigured) {
                throw GradleException(
                    "$releaseSigningError Create certs/android-release.jks and certs/android-release.properties before building release APKs."
                )
            }
        }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.12.01")
    implementation(composeBom)
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")

    implementation("androidx.navigation:navigation-compose:2.8.5")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
    implementation("androidx.activity:activity-compose:1.9.3")

    implementation("com.squareup.retrofit2:retrofit:2.11.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("com.squareup.okhttp3:logging-interceptor:4.12.0")
    implementation("org.bouncycastle:bcprov-jdk15to18:1.78.1")
    implementation("org.bouncycastle:bcpkix-jdk15to18:1.78.1")

    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.7.3")
    implementation("com.squareup.retrofit2:converter-kotlinx-serialization:2.11.0")

    implementation("androidx.datastore:datastore-preferences:1.1.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")

    implementation("androidx.browser:browser:1.8.0")
}
