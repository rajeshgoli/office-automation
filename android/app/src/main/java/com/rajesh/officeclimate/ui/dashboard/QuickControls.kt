package com.rajesh.officeclimate.ui.dashboard

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.ui.theme.*

private fun ervDisplayLabel(speed: String?): String = when (speed) {
    "quiet" -> "QUIET"
    "medium" -> "MEDIUM"
    "turbo" -> "TURBO"
    else -> "OFF"
}

@Composable
fun QuickControls(
    status: ApiStatus,
    controlLoading: String?,
    bandUpdateInFlight: Boolean,
    onPresenceState: (String) -> Unit,
    onErvSpeed: (String) -> Unit,
    onHvacMode: (String, Int?) -> Unit,
    onTemperatureBandAction: (String, String, Int) -> Unit,
    onTemperatureBandReset: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val shape = RoundedCornerShape(12.dp)
    val override = status.manualOverride
    val isPresent = status.state.lowercase() == "present" || status.isPresent
    var bandsExpanded by rememberSaveable { mutableStateOf(false) }

    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        // Presence Controls
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Presence", style = MaterialTheme.typography.labelLarge, color = TextPrimary)
                Spacer(Modifier.width(8.dp))
                Text(
                    if (isPresent) "Here" else "Away",
                    style = MaterialTheme.typography.labelMedium,
                    color = if (isPresent) Emerald else TextSecondary,
                )
            }

            ControlButton(
                label = if (isPresent) "I'M AWAY" else "I'M HERE",
                isActive = false,
                isLoading = controlLoading == "presence",
                onClick = { onPresenceState(if (isPresent) "away" else "present") },
                modifier = Modifier.weight(0.48f),
            )
        }

        // ERV Controls
        Column {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text("ERV", style = MaterialTheme.typography.labelLarge, color = TextPrimary)
                if (override?.erv == true) {
                    val mins = (override.ervExpiresIn ?: 0) / 60
                    Text(
                        "Override ${mins}m",
                        style = MaterialTheme.typography.labelSmall,
                        color = Amber,
                    )
                }
            }

            Row(
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                val currentSpeed = status.erv.speed ?: "off"
                val ervSpeeds = listOf("off", "quiet", "medium", "turbo")
                ervSpeeds.forEach { speed ->
                    val isActive = currentSpeed == speed
                    val isLoading = controlLoading == "erv_$speed"
                    val isOverride = override?.erv == true && override.ervSpeed == speed

                    ControlButton(
                        label = speed.uppercase(),
                        isActive = isActive,
                        isOverride = isOverride,
                        isLoading = isLoading,
                        onClick = { onErvSpeed(speed) },
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }

        // HVAC Controls
        Column {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text("HVAC", style = MaterialTheme.typography.labelLarge, color = TextPrimary)
                if (override?.hvac == true) {
                    val mins = (override.hvacExpiresIn ?: 0) / 60
                    Text(
                        "Override ${mins}m",
                        style = MaterialTheme.typography.labelSmall,
                        color = Amber,
                    )
                }
            }

            Row(
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                val currentMode = status.hvac.mode

                ControlButton(
                    label = "OFF",
                    isActive = currentMode == "off",
                    isLoading = controlLoading == "hvac_off",
                    onClick = { onHvacMode("off", null) },
                    modifier = Modifier.weight(1f),
                )
                ControlButton(
                    label = "HEAT 70",
                    isActive = currentMode == "heat",
                    isLoading = controlLoading == "hvac_heat",
                    onClick = { onHvacMode("heat", 70) },
                    modifier = Modifier.weight(1f),
                )
                ControlButton(
                    label = "COOL 76",
                    isActive = currentMode == "cool",
                    isLoading = controlLoading == "hvac_cool",
                    onClick = { onHvacMode("cool", 76) },
                    modifier = Modifier.weight(1f),
                )
            }
        }

        status.hvac.temperatureBands?.let { bands ->
            Column {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column {
                        Text(
                            "Hysteresis Bands",
                            style = MaterialTheme.typography.labelLarge,
                            color = TextPrimary,
                        )
                        Text(
                            "Heat ${bands.heatOnTempF}-${bands.heatOffTempF}°F · Cool ${bands.coolOffTempF}-${bands.coolOnTempF}°F",
                            style = MaterialTheme.typography.labelSmall,
                            color = TextSecondary,
                        )
                    }
                    TextButton(onClick = { bandsExpanded = !bandsExpanded }) {
                        Text(if (bandsExpanded) "Hide" else "Edit")
                    }
                }

                if (bandsExpanded) {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(top = 8.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        TemperatureBandRow(
                            label = "Heat",
                            rangeLabel = "${bands.heatOnTempF}-${bands.heatOffTempF}°F",
                            helper = "Resume at ${bands.heatOnTempF}° · pause at ${bands.heatOffTempF}°",
                            loading = bandUpdateInFlight,
                            onMoveDown = { onTemperatureBandAction("heat", "shift", -1) },
                            onMoveUp = { onTemperatureBandAction("heat", "shift", 1) },
                            onTighter = { onTemperatureBandAction("heat", "spread", -1) },
                            onWider = { onTemperatureBandAction("heat", "spread", 1) },
                        )
                        TemperatureBandRow(
                            label = "Cool",
                            rangeLabel = "${bands.coolOffTempF}-${bands.coolOnTempF}°F",
                            helper = "Stop at ${bands.coolOffTempF}° · start at ${bands.coolOnTempF}°",
                            loading = bandUpdateInFlight,
                            onMoveDown = { onTemperatureBandAction("cool", "shift", -1) },
                            onMoveUp = { onTemperatureBandAction("cool", "shift", 1) },
                            onTighter = { onTemperatureBandAction("cool", "spread", -1) },
                            onWider = { onTemperatureBandAction("cool", "spread", 1) },
                        )
                        ControlButton(
                            label = "RESET",
                            isActive = false,
                            isLoading = bandUpdateInFlight,
                            enabled = status.hvac.temperatureBandDefaults != null,
                            onClick = onTemperatureBandReset,
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun TemperatureBandRow(
    label: String,
    rangeLabel: String,
    helper: String,
    loading: Boolean,
    onMoveDown: () -> Unit,
    onMoveUp: () -> Unit,
    onTighter: () -> Unit,
    onWider: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(Color.Black.copy(alpha = 0.12f))
            .border(1.dp, Border, RoundedCornerShape(10.dp))
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(label, style = MaterialTheme.typography.labelLarge, color = TextPrimary)
            Text(rangeLabel, style = MaterialTheme.typography.titleMedium, color = TextPrimary)
        }
        Text(helper, style = MaterialTheme.typography.labelSmall, color = TextSecondary)
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            ControlButton(
                label = "-1°",
                isActive = false,
                isLoading = loading,
                onClick = onMoveDown,
                modifier = Modifier.weight(1f),
            )
            ControlButton(
                label = "+1°",
                isActive = false,
                isLoading = loading,
                onClick = onMoveUp,
                modifier = Modifier.weight(1f),
            )
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            ControlButton(
                label = "TIGHTER",
                isActive = false,
                isLoading = loading,
                onClick = onTighter,
                modifier = Modifier.weight(1f),
            )
            ControlButton(
                label = "WIDER",
                isActive = false,
                isLoading = loading,
                onClick = onWider,
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun ControlButton(
    label: String,
    isActive: Boolean,
    isLoading: Boolean = false,
    isOverride: Boolean = false,
    enabled: Boolean = true,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val bgColor = when {
        isOverride -> Amber.copy(alpha = 0.2f)
        isActive -> Emerald.copy(alpha = 0.2f)
        else -> Color.Transparent
    }
    val borderColor = when {
        isOverride -> Amber
        isActive -> Emerald
        else -> Border
    }
    val textColor = when {
        isOverride -> Amber
        isActive -> Emerald
        else -> TextSecondary
    }

    Button(
        onClick = onClick,
        enabled = enabled && !isLoading,
        modifier = modifier.height(40.dp),
        shape = RoundedCornerShape(8.dp),
        colors = ButtonDefaults.buttonColors(
            containerColor = bgColor,
            contentColor = textColor,
            disabledContainerColor = bgColor,
        ),
        border = ButtonDefaults.outlinedButtonBorder(enabled = true).let {
            androidx.compose.foundation.BorderStroke(1.dp, borderColor)
        },
    ) {
        if (isLoading) {
            CircularProgressIndicator(
                modifier = Modifier.height(16.dp),
                strokeWidth = 2.dp,
                color = Emerald,
            )
        } else {
            Text(label, style = MaterialTheme.typography.labelSmall)
        }
    }
}
