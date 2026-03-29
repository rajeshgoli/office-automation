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
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Snackbar
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.ui.theme.Background
import com.rajesh.officeclimate.ui.theme.Blue
import com.rajesh.officeclimate.ui.theme.Border
import com.rajesh.officeclimate.ui.theme.Cyan
import com.rajesh.officeclimate.ui.theme.Emerald
import com.rajesh.officeclimate.ui.theme.Orange
import com.rajesh.officeclimate.ui.theme.Red
import com.rajesh.officeclimate.ui.theme.Surface as SurfaceColor
import com.rajesh.officeclimate.ui.theme.TextPrimary
import com.rajesh.officeclimate.ui.theme.TextSecondary
import com.rajesh.officeclimate.ui.theme.Yellow
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
    val authExpired by viewModel.authExpired.collectAsState()
    val updateBannerState by viewModel.updateBannerState.collectAsState()

    LaunchedEffect(authExpired) {
        if (authExpired) onNavigateToSettings()
    }

    val snackbarHostState = remember { SnackbarHostState() }
    LaunchedEffect(controlError) {
        controlError?.let {
            snackbarHostState.showSnackbar(it)
            viewModel.clearControlError()
        }
    }
    LaunchedEffect(updateBannerState.error) {
        updateBannerState.error?.let {
            snackbarHostState.showSnackbar(it)
            viewModel.clearUpdateError()
        }
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Background),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "Office",
                    style = MaterialTheme.typography.headlineMedium,
                    color = TextPrimary,
                )
                IconButton(onClick = onNavigateToSettings) {
                    Text("⚙", style = MaterialTheme.typography.headlineMedium)
                }
            }

            Spacer(Modifier.height(16.dp))

            updateBannerState.update?.let { update ->
                UpdateAvailableBanner(
                    versionName = update.versionName,
                    uploadedAt = update.uploadedAt,
                    installing = updateBannerState.installing,
                    onDismiss = viewModel::dismissUpdateBanner,
                    onInstall = viewModel::installUpdate,
                )
                Spacer(Modifier.height(16.dp))
            }

            val currentStatus = status
            if (currentStatus == null) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 64.dp),
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

            StatusHero(status = currentStatus)

            Spacer(Modifier.height(16.dp))

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
                    unit = if (currentStatus.hvac.mode != "off") {
                        "${currentStatus.hvac.setpointC.celsiusToFahrenheit()}°F"
                    } else {
                        ""
                    },
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

            QuickControls(
                status = currentStatus,
                controlLoading = controlLoading,
                onErvSpeed = viewModel::setErvSpeed,
                onHvacMode = viewModel::setHvacMode,
            )

            Spacer(Modifier.height(16.dp))

            ConnectionBar(apiConnected = apiConnected, wsConnected = wsConnected, status = currentStatus)

            Spacer(Modifier.height(16.dp))
        }

        SnackbarHost(
            hostState = snackbarHostState,
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .padding(16.dp),
        ) { data ->
            Snackbar(
                snackbarData = data,
                containerColor = Color(0xFF7F1D1D),
                contentColor = Color.White,
            )
        }
    }
}

@Composable
private fun UpdateAvailableBanner(
    versionName: String,
    uploadedAt: String?,
    installing: Boolean,
    onDismiss: () -> Unit,
    onInstall: () -> Unit,
) {
    val shape = RoundedCornerShape(20.dp)

    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = shape,
        color = Emerald.copy(alpha = 0.12f),
        contentColor = TextPrimary,
        tonalElevation = 0.dp,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .border(1.dp, Emerald.copy(alpha = 0.35f), shape)
                .padding(16.dp),
        ) {
            Text(
                text = "Update available",
                style = MaterialTheme.typography.titleMedium,
                color = TextPrimary,
            )
            Spacer(Modifier.height(4.dp))
            Text(
                text = buildString {
                    append("Version ")
                    append(versionName)
                    append(" is ready to install.")
                    if (!uploadedAt.isNullOrBlank()) {
                        append(" Uploaded ")
                        append(uploadedAt)
                        append(".")
                    }
                },
                style = MaterialTheme.typography.bodySmall,
                color = TextSecondary,
            )
            Spacer(Modifier.height(12.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(onClick = onDismiss, enabled = !installing) {
                    Text("Dismiss")
                }
                Spacer(Modifier.width(8.dp))
                OutlinedButton(onClick = onInstall, enabled = !installing) {
                    if (installing) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(16.dp),
                            strokeWidth = 2.dp,
                            color = Emerald,
                        )
                        Spacer(Modifier.width(8.dp))
                        Text("Downloading...")
                    } else {
                        Text("Install Update")
                    }
                }
            }
        }
    }
}

@Composable
fun ConnectionBar(
    apiConnected: Boolean,
    wsConnected: Boolean,
    status: ApiStatus,
) {
    val shape = RoundedCornerShape(12.dp)

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(SurfaceColor.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(12.dp),
        horizontalArrangement = Arrangement.SpaceEvenly,
    ) {
        ConnectionDot("API", apiConnected)
        ConnectionDot("WS", wsConnected)
        ConnectionDot("Qingping", status.airQuality.co2Ppm != null)
        ConnectionDot("YoLink", true)
    }
}

@Composable
private fun ConnectionDot(label: String, connected: Boolean) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(
            modifier = Modifier
                .size(8.dp)
                .clip(CircleShape)
                .background(if (connected) Emerald else Red),
        )
        Spacer(Modifier.width(4.dp))
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = TextSecondary,
        )
    }
}
