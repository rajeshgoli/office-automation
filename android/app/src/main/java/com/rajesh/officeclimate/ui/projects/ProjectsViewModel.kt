package com.rajesh.officeclimate.ui.projects

import android.app.Application
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.rajesh.officeclimate.data.model.ProjectLeverageResponse
import com.rajesh.officeclimate.data.repository.ClimateRepository
import com.rajesh.officeclimate.data.repository.SettingsRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

class ProjectsViewModel(application: Application) : AndroidViewModel(application) {
    private val settingsRepo = SettingsRepository(application)
    private val climateRepo = ClimateRepository(settingsRepo, viewModelScope)

    private val _projectLeverage = MutableStateFlow<ProjectLeverageResponse?>(null)
    val projectLeverage: StateFlow<ProjectLeverageResponse?> = _projectLeverage

    private val _projectLeverageComparison = MutableStateFlow<ProjectLeverageResponse?>(null)
    val projectLeverageComparison: StateFlow<ProjectLeverageResponse?> = _projectLeverageComparison

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
            _projectLeverageComparison.value = null

            climateRepo.getProjectLeverage(7)
                .onSuccess { _projectLeverage.value = it }
                .onFailure { error ->
                    Log.e(TAG, "Project leverage fetch failed", error)
                    _error.value = error.message
                }

            climateRepo.getProjectLeverage(14)
                .onSuccess { _projectLeverageComparison.value = it }
                .onFailure { error ->
                    Log.w(TAG, "Project leverage comparison fetch failed", error)
                }

            _isLoading.value = false
        }
    }

    companion object {
        private const val TAG = "ProjectsVM"
    }
}
