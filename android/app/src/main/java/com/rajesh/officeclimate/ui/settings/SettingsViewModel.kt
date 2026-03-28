package com.rajesh.officeclimate.ui.settings

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.repository.SettingsRepository
import com.rajesh.officeclimate.util.Defaults
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.OkHttpClient
import okhttp3.Request
import java.util.concurrent.TimeUnit

data class SettingsUiState(
    val serverUrl: String = Defaults.SERVER_URL,
    val userEmail: String = "",
    val isLoggedIn: Boolean = false,
    val loading: Boolean = false,
    val error: String? = null,
)

class SettingsViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)

    private val _uiState = MutableStateFlow(SettingsUiState())
    val uiState: StateFlow<SettingsUiState> = _uiState

    init {
        viewModelScope.launch {
            _uiState.value = _uiState.value.copy(
                serverUrl = settingsRepo.serverUrl.first(),
                userEmail = settingsRepo.userEmail.first(),
                isLoggedIn = settingsRepo.jwtToken.first().isNotBlank(),
            )
        }
    }

    fun updateServerUrl(url: String) {
        _uiState.value = _uiState.value.copy(serverUrl = url, error = null)
    }

    fun saveServerUrl() {
        viewModelScope.launch {
            settingsRepo.saveServerUrl(_uiState.value.serverUrl)
        }
    }

    fun getOAuthUrl(callback: (String?) -> Unit) {
        val state = _uiState.value
        _uiState.value = state.copy(loading = true, error = null)

        viewModelScope.launch(kotlinx.coroutines.Dispatchers.IO) {
            try {
                settingsRepo.saveServerUrl(state.serverUrl)
                val url = "${state.serverUrl.trimEnd('/')}/auth/login?platform=android"
                val client = OkHttpClient.Builder()
                    .connectTimeout(10, TimeUnit.SECONDS)
                    .followRedirects(false)
                    .build()
                val request = Request.Builder().url(url).build()
                val response = client.newCall(request).execute()
                val body = response.body?.string() ?: ""

                val json = Json { ignoreUnknownKeys = true }
                val payload = runCatching { json.parseToJsonElement(body).jsonObject }.getOrNull()

                if (!response.isSuccessful) {
                    val errorMessage = payload
                        ?.get("error")
                        ?.jsonPrimitive
                        ?.contentOrNull
                        ?: body.ifBlank { "HTTP ${response.code}" }
                    throw IllegalStateException(errorMessage)
                }

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
                isLoggedIn = false,
            )
        }
    }
}
