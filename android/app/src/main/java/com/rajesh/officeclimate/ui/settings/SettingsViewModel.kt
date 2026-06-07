package com.rajesh.officeclimate.ui.settings

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.repository.DeviceEnrollmentRepository
import com.rajesh.officeclimate.data.repository.SettingsRepository
import com.rajesh.officeclimate.data.remote.HttpClientFactory
import com.rajesh.officeclimate.util.Defaults
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.Request

data class SettingsUiState(
    val serverUrl: String = Defaults.SERVER_URL,
    val userEmail: String = "",
    val isLoggedIn: Boolean = false,
    val deviceCertificateAlias: String = "",
    val pairingUrl: String = Defaults.DEVICE_PAIRING_URL,
    val pairingCode: String = "",
    val loading: Boolean = false,
    val enrolling: Boolean = false,
    val error: String? = null,
    val enrollmentStatus: String? = null,
)

class SettingsViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)
    private val httpClientFactory = HttpClientFactory(settingsRepo)
    private val deviceEnrollmentRepository = DeviceEnrollmentRepository(settingsRepo)

    private val _uiState = MutableStateFlow(SettingsUiState())
    val uiState: StateFlow<SettingsUiState> = _uiState

    init {
        viewModelScope.launch {
            settingsRepo.clearInvalidDeviceCredentialIfNeeded()
            _uiState.value = _uiState.value.copy(
                serverUrl = settingsRepo.serverUrl.first(),
                userEmail = settingsRepo.userEmail.first(),
                isLoggedIn = settingsRepo.isAuthenticated.first(),
                deviceCertificateAlias = settingsRepo.deviceCertificateAlias.first(),
                pairingUrl = Defaults.DEVICE_PAIRING_URL,
            )
        }
    }

    fun updateServerUrl(url: String) {
        _uiState.value = _uiState.value.copy(serverUrl = url, error = null)
    }

    fun updatePairingUrl(url: String) {
        _uiState.value = _uiState.value.copy(pairingUrl = url, error = null, enrollmentStatus = null)
    }

    fun updatePairingCode(code: String) {
        _uiState.value = _uiState.value.copy(pairingCode = code.uppercase(), error = null, enrollmentStatus = null)
    }

    fun saveServerUrl() {
        viewModelScope.launch {
            try {
                settingsRepo.saveServerUrl(_uiState.value.serverUrl)
                _uiState.value = _uiState.value.copy(error = null)
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(error = e.message)
            }
        }
    }

    fun enrollDevice() {
        val state = _uiState.value
        if (state.pairingUrl.isBlank() || state.pairingCode.isBlank()) {
            _uiState.value = state.copy(error = "Enter a pairing URL and code first.")
            return
        }

        _uiState.value = state.copy(enrolling = true, error = null, enrollmentStatus = null)
        viewModelScope.launch {
            try {
                val result = deviceEnrollmentRepository.enrollDevice(
                    pairingUrl = state.pairingUrl,
                    pairingCode = state.pairingCode,
                    deviceName = "Android ${android.os.Build.MODEL}".trim(),
                )
                _uiState.value = _uiState.value.copy(
                    enrolling = false,
                    deviceCertificateAlias = result.alias,
                    isLoggedIn = true,
                    enrollmentStatus = "Device enrolled as ${result.deviceName}.",
                    error = null,
                )
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    enrolling = false,
                    error = "Device enrollment failed: ${e.message}",
                )
            }
        }
    }

    fun getOAuthUrl(callback: (String?) -> Unit) {
        val state = _uiState.value
        _uiState.value = state.copy(loading = true, error = null)

        viewModelScope.launch(kotlinx.coroutines.Dispatchers.IO) {
            try {
                settingsRepo.saveServerUrl(state.serverUrl)
                val url = "${state.serverUrl.trimEnd('/')}/auth/login?platform=android"
                val client = httpClientFactory.create(connectTimeoutSeconds = 10, readTimeoutSeconds = 10)
                val request = Request.Builder().url(url).build()
                val response = client.newCall(request).execute()
                val body = response.body?.string() ?: ""

                if (!response.isSuccessful) {
                    val location = response.header("Location").orEmpty()
                    if (response.code in 300..399 || location.contains("/cdn-cgi/access/login/")) {
                        throw IllegalStateException(
                            "Cloudflare Access blocked Android bootstrap. Enroll the device with oa register-device, then retry.",
                        )
                    }
                    val json = Json { ignoreUnknownKeys = true }
                    val payload = runCatching { json.parseToJsonElement(body).jsonObject }.getOrNull()
                    val errorMessage = payload
                        ?.get("error")
                        ?.jsonPrimitive
                        ?.contentOrNull
                        ?: body.ifBlank { "HTTP ${response.code}" }
                    throw IllegalStateException(errorMessage)
                }

                val json = Json { ignoreUnknownKeys = true }
                val payload = runCatching { json.parseToJsonElement(body).jsonObject }.getOrNull()
                val authUrl = payload
                    ?.get("authorization_url")
                    ?.jsonPrimitive
                    ?.contentOrNull
                    ?: throw IllegalStateException("OAuth response missing authorization_url")

                _uiState.value = _uiState.value.copy(loading = false)
                callback(authUrl)
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    loading = false,
                    error = "Failed to start OAuth: ${e.message}",
                )
                callback(null)
            }
        }
    }

    fun onAuthCallback(token: String, email: String, onSuccess: () -> Unit) {
        viewModelScope.launch {
            settingsRepo.saveAuth(token, email)
            _uiState.value = _uiState.value.copy(
                userEmail = email,
                isLoggedIn = true,
                error = null,
            )
            onSuccess()
        }
    }

    fun logout() {
        viewModelScope.launch {
            settingsRepo.clearAuth()
            _uiState.value = _uiState.value.copy(
                userEmail = "",
                isLoggedIn = settingsRepo.isAuthenticated.first(),
            )
        }
    }
}
