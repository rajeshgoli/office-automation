package com.rajesh.officeclimate.ui.productivity

import android.app.Application
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.model.OrchestrationResponse
import com.rajesh.officeclimate.data.model.ProjectFocusResponse
import com.rajesh.officeclimate.data.model.SessionsResponse
import com.rajesh.officeclimate.data.repository.ClimateRepository
import com.rajesh.officeclimate.data.repository.SettingsRepository
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

class ProductivityViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)
    private val climateRepo = ClimateRepository(settingsRepo, viewModelScope)

    private val _sessions = MutableStateFlow<SessionsResponse?>(null)
    val sessions: StateFlow<SessionsResponse?> = _sessions

    private val _orchestration = MutableStateFlow<OrchestrationResponse?>(null)
    val orchestration: StateFlow<OrchestrationResponse?> = _orchestration

    private val _projectFocus = MutableStateFlow<ProjectFocusResponse?>(null)
    val projectFocus: StateFlow<ProjectFocusResponse?> = _projectFocus

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    init {
        loadData()
    }

    fun loadData() {
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null

            val sessionsDeferred = async { climateRepo.getSessions(7) }
            val orchestrationDeferred = async { climateRepo.getOrchestration(7) }
            val projectFocusDeferred = async { climateRepo.getProjectFocus(7) }

            sessionsDeferred.await()
                .onSuccess { _sessions.value = it }
                .onFailure { Log.e(TAG, "Sessions fetch failed", it) }

            orchestrationDeferred.await()
                .onSuccess { _orchestration.value = it }
                .onFailure { e ->
                    Log.e(TAG, "Orchestration fetch failed", e)
                    _error.value = e.message
                }

            projectFocusDeferred.await()
                .onSuccess { _projectFocus.value = it }
                .onFailure { Log.e(TAG, "Project focus fetch failed", it) }

            _isLoading.value = false
        }
    }

    companion object {
        private const val TAG = "ProductivityVM"
    }
}
