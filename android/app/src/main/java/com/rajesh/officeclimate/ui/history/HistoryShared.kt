package com.rajesh.officeclimate.ui.history

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.rajesh.officeclimate.data.model.SessionDay
import com.rajesh.officeclimate.data.model.SessionSummary
import com.rajesh.officeclimate.ui.theme.Amber
import com.rajesh.officeclimate.ui.theme.Border
import com.rajesh.officeclimate.ui.theme.Emerald
import com.rajesh.officeclimate.ui.theme.Surface
import com.rajesh.officeclimate.ui.theme.TextPrimary
import com.rajesh.officeclimate.ui.theme.TextSecondary

const val AXIS_START_MIN = 360
const val AXIS_END_MIN = 1320
const val AXIS_RANGE = AXIS_END_MIN - AXIS_START_MIN

val TIME_GRID = listOf(
    "6a" to 0f,
    "8a" to 0.125f,
    "10a" to 0.25f,
    "12p" to 0.375f,
    "2p" to 0.5f,
    "4p" to 0.625f,
    "6p" to 0.75f,
    "8p" to 0.875f,
    "10p" to 1.0f,
)

fun timeToFraction(timeStr: String): Float {
    val parts = timeStr.split(":")
    val h = parts[0].toIntOrNull() ?: 0
    val m = parts.getOrNull(1)?.toIntOrNull() ?: 0
    val totalMin = h * 60 + m
    return ((totalMin - AXIS_START_MIN).toFloat() / AXIS_RANGE).coerceIn(0f, 1f)
}

fun formatTime12h(timeStr: String): String {
    val parts = timeStr.split(":")
    val h = parts[0].toIntOrNull() ?: 0
    val m = parts.getOrNull(1)?.toIntOrNull() ?: 0
    val ampm = if (h < 12) "a" else "p"
    val h12 = when {
        h == 0 -> 12
        h > 12 -> h - 12
        else -> h
    }
    return "$h12:${"%02d".format(m)}$ampm"
}

fun formatMinutes(minutes: Int): String {
    if (minutes < 60) return "${minutes}m"
    val h = minutes / 60
    val m = minutes % 60
    return if (m > 0) "${h}h ${m}m" else "${h}h"
}

fun dayOfWeekLabel(date: String): String {
    return try {
        val parts = date.split("-")
        val y = parts[0].toInt()
        val m = parts[1].toInt()
        val d = parts[2].toInt()
        java.time.LocalDate.of(y, m, d).dayOfWeek.name.take(3)
    } catch (_: Exception) {
        "???"
    }
}

@Composable
fun TimeAxisHeader(durationColumnWidth: Int = 44) {
    Row(modifier = Modifier.fillMaxWidth()) {
        Spacer(Modifier.width(36.dp))
        Box(modifier = Modifier.weight(1f)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                TIME_GRID.forEach { (label, _) ->
                    Text(
                        label,
                        style = MaterialTheme.typography.labelSmall.copy(
                            fontSize = 8.sp,
                            fontFamily = FontFamily.Monospace,
                        ),
                        color = TextSecondary.copy(alpha = 0.6f),
                    )
                }
            }
        }
        Spacer(Modifier.width(durationColumnWidth.dp))
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun SessionsSection(sessions: List<SessionDay>, summary: SessionSummary) {
    val shape = RoundedCornerShape(12.dp)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
    ) {
        Text("OFFICE SESSIONS", style = MaterialTheme.typography.labelLarge, color = TextPrimary)
        Spacer(Modifier.height(12.dp))

        if (sessions.isEmpty()) {
            Text("No sessions this week", color = TextSecondary, style = MaterialTheme.typography.bodySmall)
        } else {
            TimeAxisHeader()
            Spacer(Modifier.height(4.dp))
            sessions.forEach { session ->
                SessionRow(session)
                Spacer(Modifier.height(2.dp))
            }
        }
    }

    if (sessions.isNotEmpty()) {
        FlowRow(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            maxItemsInEachRow = 2,
        ) {
            val tileModifier = Modifier.weight(1f)
            StatTile("AVG ARRIVAL", formatTime12h(summary.avgArrival), Emerald, tileModifier)
            StatTile("AVG DEPARTURE", formatTime12h(summary.avgDeparture), Emerald, tileModifier)
            StatTile("AVG DURATION", "${summary.avgDurationHours}h", Emerald, tileModifier)
            StatTile("TOTAL HOURS", "${summary.totalHoursWeek}h", Amber, tileModifier)
        }
    }
}

@Composable
private fun SessionRow(session: SessionDay) {
    val arrivalFrac = timeToFraction(session.arrival)
    val departureFrac = timeToFraction(session.departure)
    val gridColor = Border.copy(alpha = 0.3f)

    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = dayOfWeekLabel(session.date),
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.Bold,
                fontSize = 10.sp,
            ),
            color = TextPrimary,
            modifier = Modifier.width(36.dp),
        )

        Box(
            modifier = Modifier
                .weight(1f)
                .height(16.dp)
                .drawBehind {
                    TIME_GRID.forEach { (_, frac) ->
                        val x = frac * size.width
                        drawLine(
                            color = gridColor,
                            start = Offset(x, 0f),
                            end = Offset(x, size.height),
                            strokeWidth = 0.5f,
                        )
                    }

                    if (session.gaps.isEmpty()) {
                        drawRect(
                            color = Emerald.copy(alpha = 0.45f),
                            topLeft = Offset(arrivalFrac * size.width, 1f),
                            size = Size(
                                ((departureFrac - arrivalFrac) * size.width).coerceAtLeast(2f),
                                size.height - 2f,
                            ),
                        )
                    } else {
                        var segStart = arrivalFrac
                        for (gap in session.gaps) {
                            val gapStart = timeToFraction(gap.left)
                            val gapEnd = timeToFraction(gap.returned)
                            drawRect(
                                color = Emerald.copy(alpha = 0.45f),
                                topLeft = Offset(segStart * size.width, 1f),
                                size = Size(
                                    ((gapStart - segStart) * size.width).coerceAtLeast(0f),
                                    size.height - 2f,
                                ),
                            )
                            segStart = gapEnd
                        }
                        drawRect(
                            color = Emerald.copy(alpha = 0.45f),
                            topLeft = Offset(segStart * size.width, 1f),
                            size = Size(
                                ((departureFrac - segStart) * size.width).coerceAtLeast(0f),
                                size.height - 2f,
                            ),
                        )
                    }
                },
        )

        Text(
            text = "${session.durationHours}h",
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontSize = 10.sp,
            ),
            color = TextSecondary,
            modifier = Modifier
                .width(44.dp)
                .padding(start = 8.dp),
        )
    }
}

@Composable
fun StatTile(label: String, value: String, accentColor: Color, modifier: Modifier = Modifier) {
    val shape = RoundedCornerShape(12.dp)

    Column(
        modifier = modifier
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .drawBehind {
                drawRect(
                    color = accentColor,
                    topLeft = Offset.Zero,
                    size = Size(3.dp.toPx(), size.height),
                )
            }
            .padding(start = 14.dp, top = 12.dp, end = 12.dp, bottom = 12.dp),
    ) {
        Text(label, style = MaterialTheme.typography.labelSmall, color = TextSecondary)
        Text(
            text = value,
            fontSize = 24.sp,
            fontWeight = FontWeight.SemiBold,
            fontFamily = FontFamily.Monospace,
            color = accentColor,
            modifier = Modifier.padding(top = 4.dp),
        )
    }
}

@Composable
fun LegendDot(color: Color, label: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(modifier = Modifier
            .width(8.dp)
            .height(8.dp)
            .clip(RoundedCornerShape(2.dp))
            .background(color))
        Spacer(Modifier.width(4.dp))
        Text(label, style = MaterialTheme.typography.labelSmall.copy(fontSize = 9.sp), color = TextSecondary)
    }
}
