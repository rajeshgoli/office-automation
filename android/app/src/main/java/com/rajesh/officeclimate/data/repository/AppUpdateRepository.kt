package com.rajesh.officeclimate.data.repository

import android.content.Context
import android.content.Intent
import androidx.core.content.FileProvider
import com.rajesh.officeclimate.BuildConfig
import com.rajesh.officeclimate.data.model.AppArtifactMetadata
import com.rajesh.officeclimate.data.remote.ApiService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import retrofit2.HttpException
import retrofit2.Retrofit
import retrofit2.converter.kotlinx.serialization.asConverterFactory
import java.io.File
import java.io.IOException
import java.util.concurrent.TimeUnit

data class AvailableAppUpdate(
    val versionCode: Long,
    val versionName: String,
    val uploadedAt: String?,
)

class AppUpdateRepository(
    private val context: Context,
    private val settingsRepository: SettingsRepository,
) {
    private val json = Json { ignoreUnknownKeys = true; coerceInputValues = true }

    suspend fun getAvailableUpdate(): AvailableAppUpdate? {
        val serverUrl = settingsRepository.serverUrl.first().trimEnd('/')
        val metadata = fetchMetadata(serverUrl) ?: return null
        val serverVersionCode = metadata.versionCode ?: return null
        if (serverVersionCode <= BuildConfig.VERSION_CODE.toLong()) {
            return null
        }

        if (settingsRepository.dismissedUpdateVersionCode.first() == serverVersionCode) {
            return null
        }

        return AvailableAppUpdate(
            versionCode = serverVersionCode,
            versionName = metadata.versionName ?: serverVersionCode.toString(),
            uploadedAt = metadata.uploadedAt,
        )
    }

    suspend fun dismissUpdate(versionCode: Long) {
        settingsRepository.saveDismissedUpdateVersionCode(versionCode)
    }

    suspend fun downloadUpdate(update: AvailableAppUpdate): File = withContext(Dispatchers.IO) {
        val serverUrl = settingsRepository.serverUrl.first().trimEnd('/')
        val request = Request.Builder()
            .url("$serverUrl/apps/office-climate/latest.apk")
            .build()

        val updatesDir = File(context.cacheDir, "updates").apply { mkdirs() }
        val apkFile = File(updatesDir, "office-climate-${update.versionCode}.apk")

        okHttpClient().newCall(request).execute().use { response ->
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
            if (e.code() == 404) null else throw e
        }
    }

    private fun apiService(serverUrl: String): ApiService {
        val retrofit = Retrofit.Builder()
            .baseUrl("$serverUrl/")
            .client(okHttpClient())
            .addConverterFactory(json.asConverterFactory("application/json".toMediaType()))
            .build()
        return retrofit.create(ApiService::class.java)
    }

    private fun okHttpClient(): OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .build()
}
