package com.rajesh.officeclimate.ui.dashboard

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.ui.theme.*
import com.rajesh.officeclimate.util.Thresholds

data class StatusInfo(val label: String, val color: Color)

fun getStatusInfo(status: ApiStatus): StatusInfo {
    val doorOpen = status.sensors.doorOpen
    val windowOpen = status.sensors.windowOpen
    val isPresent = status.isPresent
    val co2 = status.airQuality.co2Ppm ?: 0
    val ervRunning = status.erv.running

    return when {
        doorOpen || windowOpen -> if (isPresent)
            StatusInfo("PRESENT - OPEN AIR", Cyan) else StatusInfo("AWAY - OPEN AIR", Cyan)
        isPresent && co2 > Thresholds.CO2_CRITICAL -> StatusInfo("PRESENT - VENTING", Orange)
        isPresent && co2 > Thresholds.CO2_ELEVATED -> StatusInfo("PRESENT - ELEVATED", Yellow)
        isPresent -> StatusInfo("PRESENT - QUIET", Emerald)
        ervRunning -> StatusInfo("AWAY - CLEARING", Blue)
        else -> StatusInfo("AWAY - CLEAR", BlueLight)
    }
}

fun co2Color(ppm: Int): Color = when {
    ppm > Thresholds.CO2_CRITICAL -> Red
    ppm > Thresholds.CO2_ELEVATED -> Orange
    ppm > Thresholds.CO2_NORMAL -> Yellow
    else -> Emerald
}

@Composable
fun StatusHero(status: ApiStatus, modifier: Modifier = Modifier) {
    val info = getStatusInfo(status)
    val co2 = status.airQuality.co2Ppm

    Box(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(16.dp))
            .background(info.color.copy(alpha = 0.1f))
            .padding(24.dp),
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                text = info.label,
                style = MaterialTheme.typography.labelLarge,
                color = info.color,
                letterSpacing = 2.sp,
            )

            if (co2 != null) {
                Text(
                    text = "$co2",
                    fontSize = 64.sp,
                    fontWeight = FontWeight.Bold,
                    color = co2Color(co2),
                    letterSpacing = (-2).sp,
                )
                Text(
                    text = "ppm CO2",
                    style = MaterialTheme.typography.bodySmall,
                    color = TextSecondary,
                )
            } else {
                Text(
                    text = "--",
                    fontSize = 64.sp,
                    fontWeight = FontWeight.Bold,
                    color = TextSecondary,
                )
            }
        }
    }
}
