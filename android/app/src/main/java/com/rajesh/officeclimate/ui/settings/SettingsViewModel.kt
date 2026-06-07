package com.rajesh.officeclimate.ui.settings

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.repository.DeviceEnrollmentRepository
import com.rajesh.officeclimate.data.repository.SettingsRepository
import com.rajesh.officeclimate.util.Defaults
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

data class SettingsUiState(
    val serverUrl: String = Defaults.SERVER_URL,
    val isLoggedIn: Boolean = false,
    val deviceCertificateAlias: String = "",
    val pairingUrl: String = Defaults.DEVICE_PAIRING_URL,
    val pairingCode: String = "",
    val enrolling: Boolean = false,
    val error: String? = null,
    val enrollmentStatus: String? = null,
)

class SettingsViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)
    private val deviceEnrollmentRepository = DeviceEnrollmentRepository(settingsRepo)

    private val _uiState = MutableStateFlow(SettingsUiState())
    val uiState: StateFlow<SettingsUiState> = _uiState

    init {
        viewModelScope.launch {
            settingsRepo.clearLegacyAuthAndInvalidDeviceCredentialIfNeeded()
            _uiState.value = _uiState.value.copy(
                serverUrl = settingsRepo.serverUrl.first(),
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
                settingsRepo.saveServerUrl(state.serverUrl)
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

}
