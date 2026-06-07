package com.rajesh.officeclimate.data.repository

import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.FileProvider
import com.rajesh.officeclimate.BuildConfig
import com.rajesh.officeclimate.data.model.AppArtifactMetadata
import com.rajesh.officeclimate.data.remote.ApiService
import com.rajesh.officeclimate.data.remote.HttpClientFactory
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.Request
import retrofit2.HttpException
import retrofit2.Retrofit
import retrofit2.converter.kotlinx.serialization.asConverterFactory
import java.io.File
import java.io.IOException
import java.security.MessageDigest

data class AvailableAppUpdate(
    val artifactHash: String,
    val sha256: String,
    val versionName: String,
    val uploadedAt: String?,
    val versionCode: Long?,
    val sizeBytes: Long?,
)

class AppUpdateRepository(
    private val context: Context,
    private val settingsRepository: SettingsRepository,
) {
    private val json = Json { ignoreUnknownKeys = true; coerceInputValues = true }
    private val httpClientFactory = HttpClientFactory(settingsRepository)
    @Volatile private var cachedCurrentBuildSha256: String? = null

    suspend fun getAvailableUpdate(): AvailableAppUpdate? {
        val serverUrl = settingsRepository.serverUrl.first().trimEnd('/')
        val metadata = fetchMetadata(serverUrl) ?: return null
        val serverArtifactHash = metadata.artifactHash?.trim()?.lowercase() ?: return null
        if (!isValidArtifactHash(serverArtifactHash)) {
            return null
        }
        val serverSha256 = metadata.sha256?.trim()?.lowercase() ?: return null
        if (!isValidSha256(serverSha256) || !serverSha256.startsWith(serverArtifactHash)) {
            return null
        }

        val currentBuildSha256 = currentBuildSha256()
        if (serverSha256 == currentBuildSha256) return null

        if (settingsRepository.dismissedUpdateArtifactHash.first() == serverArtifactHash) {
            return null
        }

        return AvailableAppUpdate(
            artifactHash = serverArtifactHash,
            sha256 = serverSha256,
            versionName = metadata.versionName ?: serverArtifactHash,
            uploadedAt = metadata.uploadedAt,
            versionCode = metadata.versionCode,
            sizeBytes = metadata.sizeBytes,
        )
    }

    suspend fun dismissUpdate(artifactHash: String) {
        settingsRepository.saveDismissedUpdateArtifactHash(artifactHash)
    }

    suspend fun downloadUpdate(update: AvailableAppUpdate): File = withContext(Dispatchers.IO) {
        val serverUrl = settingsRepository.serverUrl.first().trimEnd('/')
        val request = Request.Builder()
            .url("$serverUrl/apps/office-climate/${update.artifactHash}.apk")
            .build()

        val updatesDir = File(context.cacheDir, "updates").apply { mkdirs() }
        val apkFile = File(updatesDir, "office-climate-${update.artifactHash}.apk")

        httpClientFactory.create(connectTimeoutSeconds = 10, readTimeoutSeconds = 30)
            .newCall(request)
            .execute()
            .use { response ->
                if (!response.isSuccessful) {
                    throw IOException("Update download failed: HTTP ${response.code}")
                }

                val responseBody = response.body ?: throw IOException("Update download returned no body")
                responseBody.byteStream().use { input ->
                    apkFile.outputStream().use { output ->
                        input.copyTo(output)
                    }
                }
            }

        verifyDownloadedUpdate(update, apkFile)
        apkFile
    }

    fun launchInstaller(apkFile: File) {
        val uri = FileProvider.getUriForFile(
            context,
            "${context.packageName}.fileprovider",
            apkFile,
        )
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        context.startActivity(intent)
    }

    private suspend fun fetchMetadata(serverUrl: String): AppArtifactMetadata? = withContext(Dispatchers.IO) {
        try {
            apiService(serverUrl).getAppArtifactMetadata()
        } catch (e: HttpException) {
            if (e.code() == 404) return@withContext null
            if (e.code() == 302 || e.code() == 401 || e.code() == 403) {
                return@withContext null
            }
            throw e
        }
    }

    private suspend fun currentBuildSha256(): String {
        cachedCurrentBuildSha256?.let { return it }

        val configuredHash = BuildConfig.APK_HASH.trim().lowercase()
        if (isValidSha256(configuredHash)) {
            cachedCurrentBuildSha256 = configuredHash
            return configuredHash
        }

        return withContext(Dispatchers.IO) {
            val computedHash = sha256(File(context.packageCodePath))
            cachedCurrentBuildSha256 = computedHash
            computedHash
        }
    }

    private fun verifyDownloadedUpdate(update: AvailableAppUpdate, apkFile: File) {
        if (update.sizeBytes != null && apkFile.length() != update.sizeBytes) {
            throw IOException("Update APK size did not match metadata")
        }

        val actualSha256 = sha256(apkFile)
        if (actualSha256 != update.sha256) {
            apkFile.delete()
            throw IOException("Update APK digest did not match metadata")
        }

        val archiveInfo = packageInfoForArchive(apkFile)
            ?: throw IOException("Downloaded update is not a valid APK")
        if (archiveInfo.packageName != context.packageName) {
            throw IOException("Downloaded update package did not match installed app")
        }

        if (update.versionCode != null && packageVersionCode(archiveInfo) != update.versionCode) {
            throw IOException("Downloaded update version did not match metadata")
        }
        if (packageVersionCode(archiveInfo) < installedVersionCode()) {
            throw IOException("Downloaded update is older than the installed app")
        }

        val installedCertDigests = signingCertificateDigests(installedPackageInfo())
        val updateCertDigests = signingCertificateDigests(archiveInfo)
        if (installedCertDigests.isEmpty() || updateCertDigests.isEmpty()) {
            throw IOException("Unable to verify update signing certificate")
        }
        if (installedCertDigests != updateCertDigests) {
            throw IOException("Downloaded update signing certificate did not match installed app")
        }
    }

    private fun packageInfoForArchive(apkFile: File): PackageInfo? {
        val flags = signingPackageManagerFlags()
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.packageManager.getPackageArchiveInfo(
                apkFile.absolutePath,
                PackageManager.PackageInfoFlags.of(flags.toLong()),
            )
        } else {
            @Suppress("DEPRECATION")
            context.packageManager.getPackageArchiveInfo(apkFile.absolutePath, flags)
        }
    }

    private fun installedPackageInfo(): PackageInfo {
        val flags = signingPackageManagerFlags()
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.packageManager.getPackageInfo(
                context.packageName,
                PackageManager.PackageInfoFlags.of(flags.toLong()),
            )
        } else {
            @Suppress("DEPRECATION")
            context.packageManager.getPackageInfo(context.packageName, flags)
        }
    }

    private fun signingPackageManagerFlags(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            PackageManager.GET_SIGNING_CERTIFICATES
        } else {
            @Suppress("DEPRECATION")
            PackageManager.GET_SIGNATURES
        }

    private fun signingCertificateDigests(packageInfo: PackageInfo): Set<String> {
        val signatures = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            packageInfo.signingInfo?.apkContentsSigners ?: return emptySet()
        } else {
            @Suppress("DEPRECATION")
            packageInfo.signatures
        } ?: return emptySet()

        return signatures.map { signature ->
            MessageDigest.getInstance("SHA-256")
                .digest(signature.toByteArray())
                .joinToString(separator = "") { byte -> "%02x".format(byte) }
        }.toSet()
    }

    private fun installedVersionCode(): Long = packageVersionCode(installedPackageInfo())

    private fun packageVersionCode(packageInfo: PackageInfo): Long =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            packageInfo.longVersionCode
        } else {
            @Suppress("DEPRECATION")
            packageInfo.versionCode.toLong()
        }

    private fun sha256(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val bytesRead = input.read(buffer)
                if (bytesRead <= 0) break
                digest.update(buffer, 0, bytesRead)
            }
        }
        return digest.digest()
            .joinToString(separator = "") { byte -> "%02x".format(byte) }
    }

    private fun isValidArtifactHash(value: String): Boolean =
        value.length == 8 && value.all { it in '0'..'9' || it in 'a'..'f' }

    private fun isValidSha256(value: String): Boolean =
        value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }

    private suspend fun apiService(serverUrl: String): ApiService {
        val retrofit = Retrofit.Builder()
            .baseUrl("$serverUrl/")
            .client(httpClientFactory.create(connectTimeoutSeconds = 10, readTimeoutSeconds = 30))
            .addConverterFactory(json.asConverterFactory("application/json".toMediaType()))
            .build()
        return retrofit.create(ApiService::class.java)
    }
}
