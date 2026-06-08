package com.rajesh.officeclimate.ui.settings

import android.app.Application
import android.content.Intent
import android.net.Uri
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.model.DeviceFlowStartResponse
import com.rajesh.officeclimate.data.repository.DeviceEnrollmentRepository
import com.rajesh.officeclimate.data.repository.OfficeAuthRepository
import com.rajesh.officeclimate.data.repository.SettingsRepository
import com.rajesh.officeclimate.util.Defaults
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

data class SettingsUiState(
    val serverUrl: String = Defaults.SERVER_URL,
    val isLoggedIn: Boolean = false,
    val hasDeviceCredential: Boolean = false,
    val userEmail: String = "",
    val deviceCertificateAlias: String = "",
    val pairingUrl: String = Defaults.DEVICE_PAIRING_URL,
    val pairingCode: String = "",
    val enrolling: Boolean = false,
    val signingIn: Boolean = false,
    val error: String? = null,
    val enrollmentStatus: String? = null,
    val signInStatus: String? = null,
)

class SettingsViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)
    private val deviceEnrollmentRepository = DeviceEnrollmentRepository(settingsRepo)
    private val officeAuthRepository = OfficeAuthRepository(settingsRepo)

    private val _uiState = MutableStateFlow(SettingsUiState())
    val uiState: StateFlow<SettingsUiState> = _uiState

    init {
        viewModelScope.launch {
            settingsRepo.clearLegacyAuthAndInvalidDeviceCredentialIfNeeded()
            val hasDeviceCredential = settingsRepo.hasDeviceCredential.first()
            val jwtToken = settingsRepo.jwtToken.first()
            _uiState.value = _uiState.value.copy(
                serverUrl = settingsRepo.serverUrl.first(),
                hasDeviceCredential = hasDeviceCredential,
                isLoggedIn = hasDeviceCredential && jwtToken.isNotBlank(),
                userEmail = settingsRepo.userEmail.first(),
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
                    hasDeviceCredential = true,
                    isLoggedIn = false,
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

    fun signInWithGoogle() {
        val state = _uiState.value
        if (!state.hasDeviceCredential) {
            _uiState.value = state.copy(error = "Enroll this device before signing in.")
            return
        }

        _uiState.value = state.copy(signingIn = true, error = null, signInStatus = null)
        viewModelScope.launch {
            try {
                settingsRepo.saveServerUrl(_uiState.value.serverUrl)
                val flow = officeAuthRepository.startDeviceFlow()
                _uiState.value = _uiState.value.copy(
                    signInStatus = "Complete Google sign-in with code ${flow.userCode}.",
                )
                openVerificationUrl(flow)
                pollDeviceFlow(flow)
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    signingIn = false,
                    error = "Sign-in failed: ${e.message}",
                )
            }
        }
    }

    private fun openVerificationUrl(flow: DeviceFlowStartResponse) {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(flow.verificationUrl)).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        getApplication<Application>().startActivity(intent)
    }

    private suspend fun pollDeviceFlow(flow: DeviceFlowStartResponse) {
        val expiresAt = System.currentTimeMillis() + flow.expiresIn * 1000L
        var intervalMillis = flow.interval.coerceAtLeast(1) * 1000L

        while (System.currentTimeMillis() < expiresAt) {
            delay(intervalMillis)
            val response = officeAuthRepository.pollDeviceFlow(flow.deviceCode)
            when (response.status) {
                "success" -> {
                    val token = response.accessToken
                        ?: throw IllegalStateException("OAuth response did not include a token")
                    val email = response.email
                        ?: throw IllegalStateException("OAuth response did not include an email")
                    settingsRepo.saveAuth(token, email)
                    _uiState.value = _uiState.value.copy(
                        signingIn = false,
                        isLoggedIn = true,
                        userEmail = email,
                        signInStatus = "Signed in as $email.",
                        error = null,
                    )
                    return
                }
                "pending" -> {
                    _uiState.value = _uiState.value.copy(
                        signInStatus = "Waiting for Google sign-in code ${flow.userCode}.",
                    )
                }
                "slow_down" -> {
                    intervalMillis += 5_000L
                    _uiState.value = _uiState.value.copy(
                        signInStatus = "Google asked us to poll more slowly.",
                    )
                }
                "expired" -> throw IllegalStateException("Google sign-in code expired")
                "forbidden" -> throw IllegalStateException(response.message ?: "Email not allowed")
                "invalid" -> throw IllegalStateException(response.message ?: "Unknown device code")
                else -> throw IllegalStateException(response.message ?: "Google sign-in failed")
            }
        }

        throw IllegalStateException("Google sign-in code expired")
    }
}
