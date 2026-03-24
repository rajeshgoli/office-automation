package com.rajesh.officeclimate.ui.dashboard

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.data.model.ManualOverride
import com.rajesh.officeclimate.ui.theme.*
import kotlin.math.roundToInt

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
    onErvSpeed: (String) -> Unit,
    onHvacMode: (String, Int?) -> Unit,
    modifier: Modifier = Modifier,
) {
    val shape = RoundedCornerShape(12.dp)
    val override = status.manualOverride

    Column(
        modifier = modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
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
    }
}

@Composable
private fun ControlButton(
    label: String,
    isActive: Boolean,
    isLoading: Boolean = false,
    isOverride: Boolean = false,
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
        enabled = !isLoading,
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
