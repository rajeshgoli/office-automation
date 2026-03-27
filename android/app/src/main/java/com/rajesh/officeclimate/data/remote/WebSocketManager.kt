package com.rajesh.officeclimate.data.remote

import android.util.Log
import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.util.Defaults
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.serialization.json.Json
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.TimeUnit

class WebSocketManager(
    private val client: OkHttpClient,
    private val json: Json,
) {
    private var webSocket: WebSocket? = null
    private var reconnectDelay = Defaults.WS_RECONNECT_BASE_MS
    private var shouldReconnect = true

    private val _statusFlow = MutableSharedFlow<ApiStatus>(extraBufferCapacity = 1)
    val statusFlow: SharedFlow<ApiStatus> = _statusFlow

    private val _connected = MutableStateFlow(false)
    val connected: StateFlow<Boolean> = _connected

    fun connect(url: String, token: String) {
        shouldReconnect = true
        reconnectDelay = Defaults.WS_RECONNECT_BASE_MS

        val wsUrl = url
            .replace("https://", "wss://")
            .replace("http://", "ws://")
            .trimEnd('/') + "/ws"

        val requestBuilder = Request.Builder().url(wsUrl)
        if (token.isNotBlank()) {
            requestBuilder.header("Authorization", "Bearer $token")
        }

        webSocket = client.newWebSocket(requestBuilder.build(), object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                Log.d(TAG, "WebSocket connected")
                _connected.value = true
                reconnectDelay = Defaults.WS_RECONNECT_BASE_MS
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                try {
                    val status = json.decodeFromString<ApiStatus>(text)
                    _statusFlow.tryEmit(status)
                } catch (e: Exception) {
                    Log.w(TAG, "Failed to parse WS message: ${e.message}")
                }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                Log.w(TAG, "WebSocket failure: ${t.message}")
                _connected.value = false
                scheduleReconnect(url, token)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                Log.d(TAG, "WebSocket closed: $reason")
                _connected.value = false
                scheduleReconnect(url, token)
            }
        })
    }

    private fun scheduleReconnect(url: String, token: String) {
        if (!shouldReconnect) return
        Thread {
            Thread.sleep(reconnectDelay)
            reconnectDelay = (reconnectDelay * 2).coerceAtMost(Defaults.WS_RECONNECT_MAX_MS)
            if (shouldReconnect) connect(url, token)
        }.start()
    }

    fun disconnect() {
        shouldReconnect = false
        webSocket?.close(1000, "User disconnect")
        webSocket = null
        _connected.value = false
    }

    companion object {
        private const val TAG = "WebSocketManager"
    }
}
