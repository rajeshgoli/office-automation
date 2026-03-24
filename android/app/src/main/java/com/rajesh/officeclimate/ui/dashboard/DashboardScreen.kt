package com.rajesh.officeclimate.ui.dashboard

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Snackbar
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.rajesh.officeclimate.ui.theme.*
import com.rajesh.officeclimate.util.celsiusToFahrenheit
import kotlin.math.roundToInt

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun DashboardScreen(
    onNavigateToSettings: () -> Unit,
    viewModel: DashboardViewModel = viewModel(),
) {
    val status by viewModel.status.collectAsState()
    val apiConnected by viewModel.apiConnected.collectAsState()
    val wsConnected by viewModel.wsConnected.collectAsState()
    val error by viewModel.error.collectAsState()
    val controlLoading by viewModel.controlLoading.collectAsState()
    val controlError by viewModel.controlError.collectAsState()

    val snackbarHostState = remember { SnackbarHostState() }
    LaunchedEffect(controlError) {
        controlError?.let {
            snackbarHostState.showSnackbar(it)
            viewModel.clearControlError()
        }
    }

    Box(modifier = Modifier.fillMaxSize().background(Background)) {

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
    ) {
        // Header
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = "Office Climate",
                style = MaterialTheme.typography.headlineMedium,
                color = TextPrimary,
            )
            IconButton(onClick = onNavigateToSettings) {
                Text("⚙", style = MaterialTheme.typography.headlineMedium)
            }
        }

        Spacer(Modifier.height(16.dp))

        val currentStatus = status
        if (currentStatus == null) {
            // Loading / error state
            Column(
                modifier = Modifier.fillMaxWidth().padding(top = 64.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                if (error != null) {
                    Text(
                        text = "Connection Error",
                        style = MaterialTheme.typography.titleMedium,
                        color = Red,
                    )
                    Spacer(Modifier.height(8.dp))
                    Text(
                        text = error ?: "",
                        style = MaterialTheme.typography.bodySmall,
                        color = TextSecondary,
                    )
                } else {
                    CircularProgressIndicator(color = Emerald)
                    Spacer(Modifier.height(16.dp))
                    Text(
                        text = "Connecting...",
                        style = MaterialTheme.typography.bodyMedium,
                        color = TextSecondary,
                    )
                }
            }
            return
        }

        // Status Hero
        StatusHero(status = currentStatus)

        Spacer(Modifier.height(16.dp))

        // Vitals Grid
        val aq = currentStatus.airQuality
        val tempF = aq.tempC?.celsiusToFahrenheit()

        FlowRow(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            maxItemsInEachRow = 2,
        ) {
            val tileModifier = Modifier.weight(1f)

            VitalTile(
                label = "TEMPERATURE",
                value = tempF?.toString() ?: "--",
                unit = "°F",
                modifier = tileModifier,
            )
            VitalTile(
                label = "HUMIDITY",
                value = aq.humidity?.roundToInt()?.toString() ?: "--",
                unit = "%",
                modifier = tileModifier,
            )
            VitalTile(
                label = "tVOC",
                value = aq.tvoc?.toString() ?: "--",
                unit = "index",
                accentColor = if ((aq.tvoc ?: 0) > 250) Orange else TextPrimary,
                modifier = tileModifier,
            )
            VitalTile(
                label = "NOISE",
                value = aq.noiseDb?.let { "%.0f".format(it) } ?: "--",
                unit = "dB",
                modifier = tileModifier,
            )
            VitalTile(
                label = "DOOR",
                value = if (currentStatus.sensors.doorOpen) "OPEN" else "CLOSED",
                accentColor = if (currentStatus.sensors.doorOpen) Cyan else Emerald,
                modifier = tileModifier,
            )
            VitalTile(
                label = "WINDOW",
                value = if (currentStatus.sensors.windowOpen) "OPEN" else "CLOSED",
                accentColor = if (currentStatus.sensors.windowOpen) Cyan else Emerald,
                modifier = tileModifier,
            )
            VitalTile(
                label = "HVAC",
                value = currentStatus.hvac.mode.uppercase(),
                unit = if (currentStatus.hvac.mode != "off")
                    "${currentStatus.hvac.setpointC.celsiusToFahrenheit()}°F" else "",
                accentColor = when (currentStatus.hvac.mode) {
                    "heat" -> Orange
                    "cool" -> Blue
                    else -> TextSecondary
                },
                modifier = tileModifier,
            )
            VitalTile(
                label = "VENT",
                value = when (currentStatus.erv.speed) {
                    "quiet" -> "QUIET"
                    "medium" -> "MEDIUM"
                    "turbo" -> "TURBO"
                    else -> "OFF"
                },
                accentColor = if (currentStatus.erv.running) Emerald else TextSecondary,
                modifier = tileModifier,
            )
            VitalTile(
                label = "PM2.5",
                value = aq.pm25?.let { "%.0f".format(it) } ?: "--",
                unit = "µg/m³",
                modifier = tileModifier,
            )
            VitalTile(
                label = "MOTION",
                value = if (currentStatus.sensors.motionDetected) "ACTIVE" else "CLEAR",
                accentColor = if (currentStatus.sensors.motionDetected) Yellow else TextSecondary,
                modifier = tileModifier,
            )
        }

        Spacer(Modifier.height(16.dp))

        // Quick Controls
        QuickControls(
            status = currentStatus,
            controlLoading = controlLoading,
            onErvSpeed = viewModel::setErvSpeed,
            onHvacMode = viewModel::setHvacMode,
        )

        Spacer(Modifier.height(16.dp))

        // Connection Status Bar
        ConnectionBar(apiConnected = apiConnected, wsConnected = wsConnected, status = currentStatus)

        Spacer(Modifier.height(16.dp))
    }

    SnackbarHost(
        hostState = snackbarHostState,
        modifier = Modifier.align(Alignment.BottomCenter).padding(16.dp),
    ) { data ->
        Snackbar(
            snackbarData = data,
            containerColor = Color(0xFF7F1D1D),
            contentColor = Color.White,
        )
    }

    } // Box
}

@Composable
fun ConnectionBar(
    apiConnected: Boolean,
    wsConnected: Boolean,
    status: com.rajesh.officeclimate.data.model.ApiStatus,
) {
    val shape = RoundedCornerShape(12.dp)

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(12.dp),
        horizontalArrangement = Arrangement.SpaceEvenly,
    ) {
        ConnectionDot("API", apiConnected)
        ConnectionDot("WS", wsConnected)
        ConnectionDot("Qingping", status.airQuality.co2Ppm != null)
        ConnectionDot("YoLink", true) // YoLink is cloud-based, assume connected if API works
    }
}

@Composable
private fun ConnectionDot(label: String, connected: Boolean) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(
            modifier = Modifier
                .size(8.dp)
                .clip(CircleShape)
                .background(if (connected) Emerald else Red)
        )
        Spacer(Modifier.width(4.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = TextSecondary,
        )
    }
}
