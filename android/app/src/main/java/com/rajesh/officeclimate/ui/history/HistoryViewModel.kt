package com.rajesh.officeclimate.ui.history

import android.app.Application
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.model.DailyStatsResponse
import com.rajesh.officeclimate.data.model.OHLCResponse
import com.rajesh.officeclimate.data.model.SessionsResponse
import com.rajesh.officeclimate.data.model.TemperatureResponse
import com.rajesh.officeclimate.data.repository.ClimateRepository
import com.rajesh.officeclimate.data.repository.SettingsRepository
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

enum class OHLCRange(val hours: Int, val label: String) {
    ONE_HOUR(1, "1h"),
    SIX_HOURS(6, "6h"),
    ONE_DAY(24, "1d"),
    ONE_WEEK(168, "1w"),
}

class HistoryViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)
    val climateRepo = ClimateRepository(settingsRepo, viewModelScope)

    private val _sessions = MutableStateFlow<SessionsResponse?>(null)
    val sessions: StateFlow<SessionsResponse?> = _sessions

    private val _ohlcData = MutableStateFlow<OHLCResponse?>(null)
    val ohlcData: StateFlow<OHLCResponse?> = _ohlcData

    private val _dailyStats = MutableStateFlow<DailyStatsResponse?>(null)
    val dailyStats: StateFlow<DailyStatsResponse?> = _dailyStats

    private val _temperature = MutableStateFlow<TemperatureResponse?>(null)
    val temperature: StateFlow<TemperatureResponse?> = _temperature

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    private val _selectedRange = MutableStateFlow(OHLCRange.ONE_DAY)
    val selectedRange: StateFlow<OHLCRange> = _selectedRange

    init {
        loadData()
    }

    fun loadData() {
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null

            val sessionsDeferred = async { climateRepo.getSessions(7) }
            val ohlcDeferred = async { climateRepo.getCO2OHLC(_selectedRange.value.hours) }
            val statsDeferred = async { climateRepo.getDailyStats(7) }
            val tempDeferred = async { climateRepo.getTemperature(_selectedRange.value.hours) }

            sessionsDeferred.await()
                .onSuccess { _sessions.value = it }
                .onFailure { Log.e(TAG, "Sessions fetch failed", it) }

            ohlcDeferred.await()
                .onSuccess { _ohlcData.value = it }
                .onFailure { Log.e(TAG, "OHLC fetch failed", it) }

            statsDeferred.await()
                .onSuccess { _dailyStats.value = it }
                .onFailure { e ->
                    Log.e(TAG, "Stats fetch failed", e)
                    _error.value = e.message
                }

            tempDeferred.await()
                .onSuccess { _temperature.value = it }
                .onFailure { Log.e(TAG, "Temperature fetch failed", it) }

            _isLoading.value = false
        }
    }

    fun selectOHLCRange(range: OHLCRange) {
        _selectedRange.value = range
        viewModelScope.launch {
            val ohlcDeferred = async { climateRepo.getCO2OHLC(range.hours) }
            val tempDeferred = async { climateRepo.getTemperature(range.hours) }

            ohlcDeferred.await()
                .onSuccess { _ohlcData.value = it }
                .onFailure { Log.e(TAG, "OHLC fetch failed for ${range.label}", it) }

            tempDeferred.await()
                .onSuccess { _temperature.value = it }
                .onFailure { Log.e(TAG, "Temp fetch failed for ${range.label}", it) }
        }
    }

    companion object {
        private const val TAG = "HistoryVM"
    }
}
