package com.rajesh.officeclimate.data.repository

import android.util.Log
import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.data.model.DailyStatsResponse
import com.rajesh.officeclimate.data.model.LeverageResponse
import com.rajesh.officeclimate.data.model.OHLCResponse
import com.rajesh.officeclimate.data.model.OpeningsResponse
import com.rajesh.officeclimate.data.model.OrchestrationResponse
import com.rajesh.officeclimate.data.model.ProjectLeverageResponse
import com.rajesh.officeclimate.data.model.ProjectFocusResponse
import com.rajesh.officeclimate.data.model.SessionsResponse
import com.rajesh.officeclimate.data.model.TemperatureResponse
import com.rajesh.officeclimate.data.remote.ApiService
import com.rajesh.officeclimate.data.remote.AuthInterceptor
import com.rajesh.officeclimate.data.remote.WebSocketManager
import com.rajesh.officeclimate.util.Defaults
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.logging.HttpLoggingInterceptor
import retrofit2.HttpException
import retrofit2.Retrofit
import retrofit2.converter.kotlinx.serialization.asConverterFactory
import java.util.concurrent.TimeUnit

class ClimateRepository(
    private val settingsRepository: SettingsRepository,
    private val scope: CoroutineScope,
) {
    private val json = Json { ignoreUnknownKeys = true; coerceInputValues = true }

    private var currentUrl = ""
    private var currentToken = ""

    private lateinit var apiService: ApiService
    private lateinit var wsManager: WebSocketManager
    private var okHttpClient: OkHttpClient? = null

    private val _status = MutableStateFlow<ApiStatus?>(null)
    val status: StateFlow<ApiStatus?> = _status

    private val _apiConnected = MutableStateFlow(false)
    val apiConnected: StateFlow<Boolean> = _apiConnected

    private val _wsConnected = MutableStateFlow(false)
    val wsConnected: StateFlow<Boolean> = _wsConnected

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    private val _authExpired = MutableStateFlow(false)
    val authExpired: StateFlow<Boolean> = _authExpired

    private var pollJob: Job? = null
    private var wsCollectJob: Job? = null

    fun start() {
        scope.launch {
            ensureInitialized()
            startPolling()
            startWebSocket()
        }
    }

    private suspend fun ensureInitialized() {
        if (::apiService.isInitialized) return
        val url = settingsRepository.serverUrl.first()
        val token = settingsRepository.jwtToken.first()
        rebuild(url, token)
    }

    fun stop() {
        pollJob?.cancel()
        wsCollectJob?.cancel()
        if (::wsManager.isInitialized) wsManager.disconnect()
    }

    private fun rebuild(url: String, token: String) {
        currentUrl = url
        currentToken = token

        val client = OkHttpClient.Builder()
            .addInterceptor(AuthInterceptor { currentToken })
            .addInterceptor(HttpLoggingInterceptor().apply {
                level = HttpLoggingInterceptor.Level.BASIC
            })
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(10, TimeUnit.SECONDS)
            .build()
        okHttpClient = client

        val retrofit = Retrofit.Builder()
            .baseUrl("$url/")
            .client(client)
            .addConverterFactory(json.asConverterFactory("application/json".toMediaType()))
            .build()

        apiService = retrofit.create(ApiService::class.java)
        wsManager = WebSocketManager(client, json)
    }

    private fun startPolling() {
        pollJob?.cancel()
        pollJob = scope.launch(Dispatchers.IO) {
            while (true) {
                try {
                    val status = apiService.getStatus()
                    _status.value = status
                    _apiConnected.value = true
                    _error.value = null
                } catch (e: HttpException) {
                    if (e.code() == 401) {
                        Log.w(TAG, "Auth expired (401), clearing token")
                        settingsRepository.clearAuth()
                        _authExpired.value = true
                        stop()
                        return@launch
                    }
                    Log.w(TAG, "Poll failed: ${e.message}")
                    _apiConnected.value = false
                    if (_status.value == null) {
                        _error.value = e.message ?: "Connection failed"
                    }
                } catch (e: Exception) {
                    Log.w(TAG, "Poll failed: ${e.message}")
                    _apiConnected.value = false
                    if (_status.value == null) {
                        _error.value = e.message ?: "Connection failed"
                    }
                }
                delay(Defaults.POLL_INTERVAL_MS)
            }
        }
    }

    private fun startWebSocket() {
        if (!::wsManager.isInitialized) return

        wsManager.connect(currentUrl, currentToken)

        wsCollectJob?.cancel()
        wsCollectJob = scope.launch {
            launch {
                wsManager.statusFlow.collect { status ->
                    _status.value = status
                }
            }
            launch {
                wsManager.connected.collect { connected ->
                    _wsConnected.value = connected
                }
            }
        }
    }

    suspend fun setErvSpeed(speed: String): Result<Unit> = runCatching {
        apiService.setErvSpeed(mapOf("speed" to speed))
    }

    suspend fun setHvacMode(mode: String, setpointF: Int? = null): Result<Unit> = runCatching {
        val body = buildJsonObject {
            put("mode", mode)
            if (setpointF != null) put("setpoint_f", setpointF)
        }
        apiService.setHvacMode(body)
    }

    suspend fun getSessions(days: Int = 7): Result<SessionsResponse> = runCatching {
        ensureInitialized()
        apiService.getSessions(days)
    }

    suspend fun getCO2OHLC(hours: Int = 24): Result<OHLCResponse> = runCatching {
        ensureInitialized()
        apiService.getCO2OHLC(hours)
    }

    suspend fun getDailyStats(days: Int = 7): Result<DailyStatsResponse> = runCatching {
        ensureInitialized()
        apiService.getDailyStats(days)
    }

    suspend fun getTemperature(hours: Int = 24): Result<TemperatureResponse> = runCatching {
        ensureInitialized()
        apiService.getTemperature(hours)
    }

    suspend fun getOpenings(days: Int = 7): Result<OpeningsResponse> = runCatching {
        ensureInitialized()
        apiService.getOpenings(days)
    }

    suspend fun getOrchestration(days: Int = 7): Result<OrchestrationResponse> = runCatching {
        ensureInitialized()
        apiService.getOrchestration(days)
    }

    suspend fun getProjectFocus(days: Int = 7): Result<ProjectFocusResponse> = runCatching {
        ensureInitialized()
        apiService.getProjectFocus(days)
    }

    suspend fun getLeverage(days: Int = 7): Result<LeverageResponse> = runCatching {
        ensureInitialized()
        apiService.getLeverage(days)
    }

    suspend fun getProjectLeverage(days: Int = 7): Result<ProjectLeverageResponse> = runCatching {
        ensureInitialized()
        apiService.getProjectLeverage(days)
    }

    suspend fun testConnection(url: String, token: String): Result<ApiStatus> = runCatching {
        val client = OkHttpClient.Builder()
            .addInterceptor(AuthInterceptor { token })
            .connectTimeout(5, TimeUnit.SECONDS)
            .readTimeout(5, TimeUnit.SECONDS)
            .build()

        val retrofit = Retrofit.Builder()
            .baseUrl("${url.trimEnd('/')}/")
            .client(client)
            .addConverterFactory(json.asConverterFactory("application/json".toMediaType()))
            .build()

        retrofit.create(ApiService::class.java).getStatus()
    }

    fun reconnect() {
        scope.launch {
            stop()
            val url = settingsRepository.serverUrl.first()
            val token = settingsRepository.jwtToken.first()
            rebuild(url, token)
            startPolling()
            startWebSocket()
        }
    }

    companion object {
        private const val TAG = "ClimateRepository"
    }
}
