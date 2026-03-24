package com.rajesh.officeclimate.ui.dashboard

import android.app.Application
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.data.repository.ClimateRepository
import com.rajesh.officeclimate.data.repository.SettingsRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

class DashboardViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)
    val climateRepo = ClimateRepository(settingsRepo, viewModelScope)

    val status: StateFlow<ApiStatus?> = climateRepo.status
    val apiConnected: StateFlow<Boolean> = climateRepo.apiConnected
    val wsConnected: StateFlow<Boolean> = climateRepo.wsConnected
    val error: StateFlow<String?> = climateRepo.error

    private val _controlLoading = MutableStateFlow<String?>(null)
    val controlLoading: StateFlow<String?> = _controlLoading

    init {
        climateRepo.start()
    }

    private val _controlError = MutableStateFlow<String?>(null)
    val controlError: StateFlow<String?> = _controlError

    fun clearControlError() { _controlError.value = null }

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

    override fun onCleared() {
        super.onCleared()
        climateRepo.stop()
    }
}
