package com.rajesh.officeclimate.ui.dashboard

import android.app.Application
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.model.AppNotification
import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.data.model.TemperatureBands
import com.rajesh.officeclimate.data.repository.AppUpdateRepository
import com.rajesh.officeclimate.data.repository.AvailableAppUpdate
import com.rajesh.officeclimate.data.repository.ClimateRepository
import com.rajesh.officeclimate.data.repository.SettingsRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch

data class UpdateBannerUiState(
    val update: AvailableAppUpdate? = null,
    val installing: Boolean = false,
    val error: String? = null,
)

data class AppNotificationBannerUiState(
    val notification: AppNotification? = null,
)

class DashboardViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)
    private val updateRepo = AppUpdateRepository(application, settingsRepo)

    val climateRepo = ClimateRepository(settingsRepo, viewModelScope)

    val status: StateFlow<ApiStatus?> = climateRepo.status
    val apiConnected: StateFlow<Boolean> = climateRepo.apiConnected
    val wsConnected: StateFlow<Boolean> = climateRepo.wsConnected
    val error: StateFlow<String?> = climateRepo.error
    val authExpired: StateFlow<Boolean> = climateRepo.authExpired

    private val _controlLoading = MutableStateFlow<String?>(null)
    val controlLoading: StateFlow<String?> = _controlLoading

    private val _bandUpdateInFlight = MutableStateFlow(false)
    val bandUpdateInFlight: StateFlow<Boolean> = _bandUpdateInFlight

    private val _controlError = MutableStateFlow<String?>(null)
    val controlError: StateFlow<String?> = _controlError

    private val _updateBannerState = MutableStateFlow(UpdateBannerUiState())
    val updateBannerState: StateFlow<UpdateBannerUiState> = _updateBannerState

    private val _appNotificationBannerState = MutableStateFlow(AppNotificationBannerUiState())
    val appNotificationBannerState: StateFlow<AppNotificationBannerUiState> = _appNotificationBannerState

    init {
        climateRepo.start()
        refreshUpdateBanner()
        observeAppNotifications()
    }

    fun clearControlError() {
        _controlError.value = null
    }

    fun setErvSpeed(speed: String) {
        _controlLoading.value = "erv_$speed"
        viewModelScope.launch {
            climateRepo.setErvSpeed(speed)
                .onFailure { e ->
                    Log.e("DashboardVM", "ERV control failed", e)
                    _controlError.value = "ERV command failed: ${e.message}"
                }
            _controlLoading.value = null
        }
    }

    fun setHvacMode(mode: String, setpointF: Int? = null) {
        _controlLoading.value = "hvac_$mode"
        viewModelScope.launch {
            climateRepo.setHvacMode(mode, setpointF)
                .onFailure { e ->
                    Log.e("DashboardVM", "HVAC control failed", e)
                    _controlError.value = "HVAC command failed: ${e.message}"
                }
            _controlLoading.value = null
        }
    }

    fun setPresence(state: String) {
        _controlLoading.value = "presence"
        viewModelScope.launch {
            climateRepo.setPresence(state)
                .onFailure { e ->
                    Log.e("DashboardVM", "Presence control failed", e)
                    _controlError.value = "Presence command failed: ${e.message}"
                }
            _controlLoading.value = null
        }
    }

    fun updateTemperatureBand(band: String, action: String, delta: Int) {
        if (_bandUpdateInFlight.value) return

        val currentBands = status.value?.hvac?.temperatureBands ?: return
        val nextBands = adjustTemperatureBands(currentBands, band, action, delta)
        _bandUpdateInFlight.value = true
        viewModelScope.launch {
            climateRepo.setTemperatureBands(nextBands)
                .onFailure { e ->
                    Log.e("DashboardVM", "Temperature band update failed", e)
                    _controlError.value = "Band update failed: ${e.message}"
                }
            _bandUpdateInFlight.value = false
        }
    }

    fun resetTemperatureBands() {
        if (_bandUpdateInFlight.value) return

        val defaults = status.value?.hvac?.temperatureBandDefaults ?: return
        _bandUpdateInFlight.value = true
        viewModelScope.launch {
            climateRepo.setTemperatureBands(defaults)
                .onFailure { e ->
                    Log.e("DashboardVM", "Temperature band reset failed", e)
                    _controlError.value = "Band reset failed: ${e.message}"
                }
            _bandUpdateInFlight.value = false
        }
    }

    fun dismissUpdateBanner() {
        val update = _updateBannerState.value.update ?: return
        viewModelScope.launch {
            updateRepo.dismissUpdate(update.artifactHash)
            _updateBannerState.value = _updateBannerState.value.copy(update = null)
        }
    }

    fun installUpdate() {
        val update = _updateBannerState.value.update ?: return
        if (_updateBannerState.value.installing) return

        _updateBannerState.value = _updateBannerState.value.copy(installing = true, error = null)
        viewModelScope.launch {
            runCatching {
                val apkFile = updateRepo.downloadUpdate(update)
                updateRepo.launchInstaller(apkFile)
            }.onFailure { e ->
                Log.e("DashboardVM", "Update install failed", e)
                _updateBannerState.value = _updateBannerState.value.copy(
                    installing = false,
                    error = "Update failed: ${e.message}",
                )
                return@launch
            }

            _updateBannerState.value = _updateBannerState.value.copy(installing = false)
        }
    }

    fun clearUpdateError() {
        _updateBannerState.value = _updateBannerState.value.copy(error = null)
    }

    fun dismissAppNotificationBanner() {
        val notification = _appNotificationBannerState.value.notification ?: return
        viewModelScope.launch {
            settingsRepo.dismissAppNotification(notification.id)
            _appNotificationBannerState.value = AppNotificationBannerUiState()
        }
    }

    private fun refreshUpdateBanner() {
        viewModelScope.launch {
            runCatching { updateRepo.getAvailableUpdate() }
                .onSuccess { update ->
                    _updateBannerState.value = UpdateBannerUiState(update = update)
                }
                .onFailure { e ->
                    Log.w("DashboardVM", "Update check failed", e)
                    _updateBannerState.value = UpdateBannerUiState()
                }
        }
    }

    private fun observeAppNotifications() {
        viewModelScope.launch {
            combine(status, settingsRepo.dismissedAppNotificationIds) { currentStatus, dismissedIds ->
                currentStatus
                    ?.notifications
                    ?.firstOrNull { notification ->
                        notification.active && notification.id !in dismissedIds
                    }
            }.collect { notification ->
                _appNotificationBannerState.value = AppNotificationBannerUiState(notification = notification)
            }
        }
    }

    override fun onCleared() {
        super.onCleared()
        climateRepo.stop()
    }

    private fun adjustTemperatureBands(
        bands: TemperatureBands,
        band: String,
        action: String,
        delta: Int,
    ): TemperatureBands {
        var heatOn = bands.heatOnTempF
        var heatOff = bands.heatOffTempF
        var coolOff = bands.coolOffTempF
        var coolOn = bands.coolOnTempF

        if (band == "heat") {
            if (action == "shift") {
                heatOn += delta
                heatOff += delta
            } else {
                heatOn -= delta
                heatOff += delta
            }
            heatOn = heatOn.coerceIn(45, 85)
            heatOff = heatOff.coerceIn(46, 90)
            if (heatOff <= heatOn) {
                heatOff = (heatOn + 1).coerceIn(46, 90)
                heatOn = (heatOff - 1).coerceIn(45, 85)
            }
        } else {
            if (action == "shift") {
                coolOff += delta
                coolOn += delta
            } else {
                coolOff -= delta
                coolOn += delta
            }
            coolOff = coolOff.coerceIn(55, 95)
            coolOn = coolOn.coerceIn(56, 100)
            if (coolOn <= coolOff) {
                coolOn = (coolOff + 1).coerceIn(56, 100)
                coolOff = (coolOn - 1).coerceIn(55, 95)
            }
        }

        return TemperatureBands(
            heatOnTempF = heatOn,
            heatOffTempF = heatOff,
            coolOffTempF = coolOff,
            coolOnTempF = coolOn,
        )
    }
}
