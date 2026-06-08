package com.rajesh.officeclimate.data.repository

import com.rajesh.officeclimate.data.model.BrowserLoginStartResponse
import com.rajesh.officeclimate.data.model.DeviceFlowPollRequest
import com.rajesh.officeclimate.data.model.DeviceFlowPollResponse
import com.rajesh.officeclimate.data.model.DeviceFlowStartResponse
import com.rajesh.officeclimate.data.remote.ApiService
import com.rajesh.officeclimate.data.remote.HttpClientFactory
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import retrofit2.Retrofit
import retrofit2.converter.kotlinx.serialization.asConverterFactory

class OfficeAuthRepository(
    private val settingsRepository: SettingsRepository,
) {
    private val json = Json { ignoreUnknownKeys = true; coerceInputValues = true }
    private val httpClientFactory = HttpClientFactory(settingsRepository)

    suspend fun startBrowserLogin(): BrowserLoginStartResponse =
        apiService().startBrowserLogin()

    suspend fun startDeviceFlow(): DeviceFlowStartResponse =
        apiService().startDeviceFlow()

    suspend fun pollDeviceFlow(deviceCode: String): DeviceFlowPollResponse =
        apiService().pollDeviceFlow(DeviceFlowPollRequest(deviceCode))

    private suspend fun apiService(): ApiService {
        val serverUrl = settingsRepository.serverUrl.first().trimEnd('/')
        val client = httpClientFactory.create(
            includeLogging = true,
            connectTimeoutSeconds = 10,
            readTimeoutSeconds = 30,
        )
        return Retrofit.Builder()
            .baseUrl("$serverUrl/")
            .client(client)
            .addConverterFactory(json.asConverterFactory("application/json".toMediaType()))
            .build()
            .create(ApiService::class.java)
    }
}
