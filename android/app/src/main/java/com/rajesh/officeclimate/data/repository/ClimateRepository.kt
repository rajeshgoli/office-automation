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
import com.rajesh.officeclimate.data.model.TemperatureBands
import com.rajesh.officeclimate.data.model.TemperatureResponse
import com.rajesh.officeclimate.data.remote.ApiService
import com.rajesh.officeclimate.data.remote.HttpClientFactory
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
import retrofit2.HttpException
import retrofit2.Retrofit
import retrofit2.converter.kotlinx.serialization.asConverterFactory

class ClimateRepository(
    private val settingsRepository: SettingsRepository,
    private val scope: CoroutineScope,
) {
    private val json = Json { ignoreUnknownKeys = true; coerceInputValues = true }

    private var currentUrl = ""

    private lateinit var apiService: ApiService
    private lateinit var wsManager: WebSocketManager
    private var okHttpClient: OkHttpClient? = null
    private val httpClientFactory = HttpClientFactory(settingsRepository)

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
        rebuild(url)
    }

    fun stop() {
        pollJob?.cancel()
        wsCollectJob?.cancel()
        if (::wsManager.isInitialized) wsManager.disconnect()
    }

    private suspend fun rebuild(url: String) {
        currentUrl = url
        val authToken = settingsRepository.jwtToken.first().trim()

        val client = httpClientFactory.create(
            includeLogging = true,
            connectTimeoutSeconds = 10,
            readTimeoutSeconds = 10,
        )
        okHttpClient = client

        val retrofit = Retrofit.Builder()
            .baseUrl("$url/")
            .client(client)
            .addConverterFactory(json.asConverterFactory("application/json".toMediaType()))
            .build()

        apiService = retrofit.create(ApiService::class.java)
        wsManager = WebSocketManager(client, json, authToken)
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
                    if (e.code() == 302 || e.code() == 401 || e.code() == 403) {
                        Log.w(TAG, "Cloudflare Access blocked request (${e.code()})")
                        _apiConnected.value = false
                        _error.value = "Cloudflare Access blocked this device. Enroll it with oa register-device."
                        delay(Defaults.POLL_INTERVAL_MS)
                        continue
                    }
                    Log.w(TAG, "Poll failed: ${e.message}")
                    _apiConnected.value = false
                    if (_status.value == null) {
                        _error.value = "${e::class.java.simpleName}: ${e.message ?: "Connection failed"}"
                    }
                } catch (e: Exception) {
                    Log.w(TAG, "Poll failed: ${e.message}")
                    _apiConnected.value = false
                    if (_status.value == null) {
                        _error.value = "${e::class.java.simpleName}: ${e.message ?: "Connection failed"}"
                    }
                }
                delay(Defaults.POLL_INTERVAL_MS)
            }
        }
    }

    private fun startWebSocket() {
        if (!::wsManager.isInitialized) return

        wsManager.connect(currentUrl)

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

    suspend fun setPresence(state: String): Result<Unit> = runCatching {
        apiService.setPresence(mapOf("state" to state))
    }

    suspend fun setTemperatureBands(bands: TemperatureBands): Result<Unit> = runCatching {
        val body = buildJsonObject {
            put("temperature_bands", buildJsonObject {
                put("heat_on_temp_f", bands.heatOnTempF)
                put("heat_off_temp_f", bands.heatOffTempF)
                put("cool_off_temp_f", bands.coolOffTempF)
                put("cool_on_temp_f", bands.coolOnTempF)
            })
        }
        val response = apiService.setTemperatureBands(body)
        val savedBands = response.temperatureBands ?: bands
        _status.value = _status.value?.let { currentStatus ->
            currentStatus.copy(
                hvac = currentStatus.hvac.copy(temperatureBands = savedBands),
            )
        }
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

    suspend fun testConnection(url: String): Result<ApiStatus> = runCatching {
        val client = httpClientFactory.create(
            connectTimeoutSeconds = 5,
            readTimeoutSeconds = 5,
        )

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
            rebuild(url)
            startPolling()
            startWebSocket()
        }
    }

    companion object {
        private const val TAG = "ClimateRepository"
    }
}
