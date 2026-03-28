package com.rajesh.officeclimate.ui.history

import androidx.compose.foundation.Canvas
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
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.rajesh.officeclimate.data.model.CO2Candle
import com.rajesh.officeclimate.data.model.DailyStat
import com.rajesh.officeclimate.data.model.SessionDay
import com.rajesh.officeclimate.data.model.SessionSummary
import com.rajesh.officeclimate.data.model.TempPoint
import com.rajesh.officeclimate.ui.theme.*

// Time axis: 6am (360 min) to 10pm (1320 min)
private const val AXIS_START_MIN = 360
private const val AXIS_END_MIN = 1320
private const val AXIS_RANGE = AXIS_END_MIN - AXIS_START_MIN

private fun timeToFraction(timeStr: String): Float {
    val parts = timeStr.split(":")
    val h = parts[0].toIntOrNull() ?: 0
    val m = parts.getOrNull(1)?.toIntOrNull() ?: 0
    val totalMin = h * 60 + m
    return ((totalMin - AXIS_START_MIN).toFloat() / AXIS_RANGE).coerceIn(0f, 1f)
}

private fun formatTime12h(timeStr: String): String {
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

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun HistoryScreen(
    viewModel: HistoryViewModel = viewModel(),
) {
    val sessions by viewModel.sessions.collectAsState()
    val ohlcData by viewModel.ohlcData.collectAsState()
    val dailyStats by viewModel.dailyStats.collectAsState()
    val temperature by viewModel.temperature.collectAsState()
    val isLoading by viewModel.isLoading.collectAsState()
    val selectedRange by viewModel.selectedRange.collectAsState()

    val error by viewModel.error.collectAsState()
    val allFailed = !isLoading && sessions == null && ohlcData == null && dailyStats == null

    Box(modifier = Modifier.fillMaxSize().background(Background)) {
        if (isLoading && sessions == null) {
            Column(
                modifier = Modifier.fillMaxSize(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                CircularProgressIndicator(color = Emerald)
                Spacer(Modifier.height(16.dp))
                Text("Loading history...", color = TextSecondary)
            }
        } else {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                // Header
                Text(
                    text = "History",
                    style = MaterialTheme.typography.headlineMedium,
                    color = TextPrimary,
                )

                if (allFailed) {
                    Spacer(Modifier.height(32.dp))
                    Column(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text(
                            "Could not load history data",
                            style = MaterialTheme.typography.titleMedium,
                            color = Red,
                        )
                        Spacer(Modifier.height(8.dp))
                        Text(
                            error ?: "Check that the server is updated with history endpoints",
                            style = MaterialTheme.typography.bodySmall,
                            color = TextSecondary,
                        )
                        Spacer(Modifier.height(16.dp))
                        Button(
                            onClick = { viewModel.loadData() },
                            colors = ButtonDefaults.buttonColors(
                                containerColor = Emerald.copy(alpha = 0.2f),
                                contentColor = Emerald,
                            ),
                        ) {
                            Text("Retry")
                        }
                    }
                }

                // Section 1: Office Sessions
                sessions?.let { data ->
                    SessionsSection(sessions = data.sessions, summary = data.summary)
                }

                // Section 2: CO2 OHLC
                ohlcData?.let { data ->
                    OHLCSection(
                        candles = data.candles,
                        selectedRange = selectedRange,
                        onRangeSelected = viewModel::selectOHLCRange,
                    )
                }

                // Section 3: Temperature
                temperature?.let { data ->
                    if (data.points.isNotEmpty()) {
                        TemperatureSection(points = data.points)
                    }
                }

                // Section 4: Daily Stats
                dailyStats?.let { data ->
                    if (data.stats.isNotEmpty()) {
                        Text(
                            "TODAY",
                            style = MaterialTheme.typography.labelLarge,
                            color = TextPrimary,
                        )
                        DailyStatsSection(stat = data.stats.last())
                    }
                }

                Spacer(Modifier.height(64.dp)) // Bottom nav padding
            }
        }
    }
}

// ── Section 1: Sessions Timeline ──

// Time grid labels and their fractions on the 6a-10p axis
private val TIME_GRID = listOf(
    "6a" to 0f,      // 6:00
    "8a" to 0.125f,   // 8:00
    "10a" to 0.25f,   // 10:00
    "12p" to 0.375f,  // 12:00
    "2p" to 0.5f,     // 14:00
    "4p" to 0.625f,   // 16:00
    "6p" to 0.75f,    // 18:00
    "8p" to 0.875f,   // 20:00
    "10p" to 1.0f,    // 22:00
)

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SessionsSection(sessions: List<SessionDay>, summary: SessionSummary) {
    val shape = RoundedCornerShape(12.dp)

    // Timeline card
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
            // Time axis header
            Row(modifier = Modifier.fillMaxWidth()) {
                Spacer(Modifier.width(36.dp)) // Day label width
                Box(modifier = Modifier.weight(1f)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        TIME_GRID.forEach { (label, _) ->
                            Text(
                                label,
                                style = MaterialTheme.typography.labelSmall.copy(fontSize = 8.sp, fontFamily = FontFamily.Monospace),
                                color = TextSecondary.copy(alpha = 0.6f),
                            )
                        }
                    }
                }
                Spacer(Modifier.width(44.dp)) // Duration column width
            }

            Spacer(Modifier.height(4.dp))

            // Session rows with shared grid
            sessions.forEach { session ->
                SessionRow(session)
                Spacer(Modifier.height(2.dp))
            }
        }
    }

    // Summary card (separate)
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
    val dayOfWeek = try {
        val parts = session.date.split("-")
        val y = parts[0].toInt(); val m = parts[1].toInt(); val d = parts[2].toInt()
        java.time.LocalDate.of(y, m, d).dayOfWeek.name.take(3)
    } catch (_: Exception) { "???" }

    val arrivalFrac = timeToFraction(session.arrival)
    val departureFrac = timeToFraction(session.departure)
    val gridColor = Border.copy(alpha = 0.3f)

    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // Day label
        Text(
            text = dayOfWeek,
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.Bold,
                fontSize = 10.sp,
            ),
            color = TextPrimary,
            modifier = Modifier.width(36.dp),
        )

        // Timeline bar with grid
        Box(
            modifier = Modifier
                .weight(1f)
                .height(16.dp)
                .drawBehind {
                    // Draw vertical grid lines
                    TIME_GRID.forEach { (_, frac) ->
                        val x = frac * size.width
                        drawLine(
                            color = gridColor,
                            start = Offset(x, 0f),
                            end = Offset(x, size.height),
                            strokeWidth = 0.5f,
                        )
                    }

                    // Draw session segments
                    if (session.gaps.isEmpty()) {
                        drawRect(
                            color = Emerald.copy(alpha = 0.45f),
                            topLeft = Offset(arrivalFrac * size.width, 1f),
                            size = Size(((departureFrac - arrivalFrac) * size.width).coerceAtLeast(2f), size.height - 2f),
                        )
                    } else {
                        var segStart = arrivalFrac
                        for (gap in session.gaps) {
                            val gapStart = timeToFraction(gap.left)
                            val gapEnd = timeToFraction(gap.returned)
                            drawRect(
                                color = Emerald.copy(alpha = 0.45f),
                                topLeft = Offset(segStart * size.width, 1f),
                                size = Size(((gapStart - segStart) * size.width).coerceAtLeast(0f), size.height - 2f),
                            )
                            segStart = gapEnd
                        }
                        drawRect(
                            color = Emerald.copy(alpha = 0.45f),
                            topLeft = Offset(segStart * size.width, 1f),
                            size = Size(((departureFrac - segStart) * size.width).coerceAtLeast(0f), size.height - 2f),
                        )
                    }
                },
        )

        // Duration column
        Text(
            text = "${session.durationHours}h",
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontSize = 10.sp,
            ),
            color = TextSecondary,
            modifier = Modifier.width(44.dp).padding(start = 8.dp),
        )
    }
}


// ── Section 2: CO2 OHLC ──

@Composable
private fun OHLCSection(
    candles: List<CO2Candle>,
    selectedRange: OHLCRange,
    onRangeSelected: (OHLCRange) -> Unit,
) {
    val shape = RoundedCornerShape(12.dp)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("CO2 TREND", style = MaterialTheme.typography.labelLarge, color = TextPrimary)

            Row(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                OHLCRange.entries.forEach { range ->
                    val isSelected = range == selectedRange
                    Button(
                        onClick = { onRangeSelected(range) },
                        modifier = Modifier.height(28.dp),
                        shape = RoundedCornerShape(6.dp),
                        colors = ButtonDefaults.buttonColors(
                            containerColor = if (isSelected) Emerald.copy(alpha = 0.2f) else Color.Transparent,
                            contentColor = if (isSelected) Emerald else TextSecondary,
                        ),
                        contentPadding = ButtonDefaults.ContentPadding.let {
                            androidx.compose.foundation.layout.PaddingValues(horizontal = 10.dp, vertical = 0.dp)
                        },
                    ) {
                        Text(range.label, style = MaterialTheme.typography.labelSmall)
                    }
                }
            }
        }

        Spacer(Modifier.height(16.dp))

        // Candlestick chart
        if (candles.isNotEmpty()) {
            CandlestickChart(candles = candles, modifier = Modifier.fillMaxWidth().height(200.dp))

            Spacer(Modifier.height(12.dp))

            // Summary row
            val avg = candles.map { it.avg }.average().toInt()
            val peak = candles.maxOf { it.high }
            val low = candles.minOf { it.low }

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceEvenly,
            ) {
                OHLCStat("AVERAGE", "$avg ppm", TextPrimary)
                OHLCStat("PEAK", "$peak ppm", Red)
                OHLCStat("LOW", "$low ppm", Emerald)
            }

            Spacer(Modifier.height(8.dp))

            // Legend
            Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                LegendDot(Emerald, "FALLING")
                LegendDot(Amber, "RISING")
            }
        } else {
            Text("No CO2 data for this period", color = TextSecondary, style = MaterialTheme.typography.bodySmall)
        }
    }
}

@Composable
private fun CandlestickChart(candles: List<CO2Candle>, modifier: Modifier = Modifier) {
    val leftPadding = 40f // Space for Y-axis labels
    val chartPadding = 8f
    val dataHigh = candles.maxOf { it.high }
    val dataLow = candles.minOf { it.low }
    val yMax = maxOf(dataHigh + 100, if (dataHigh > 700) 900 else dataHigh + 200)
        .let { if (dataHigh > 1800) maxOf(it, 2200) else it }
    val yMin = maxOf(0, dataLow - 100)

    // Compute nice grid steps
    val yRange = yMax - yMin
    val gridStep = when {
        yRange <= 400 -> 100
        yRange <= 800 -> 200
        yRange <= 1600 -> 400
        else -> 500
    }
    val gridStart = ((yMin / gridStep) + 1) * gridStep
    val gridLines = (gridStart..yMax step gridStep).toList()

    val textMeasurer = rememberTextMeasurer()
    val gridLabelStyle = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontSize = 8.sp,
        color = TextSecondary.copy(alpha = 0.6f),
    )

    Canvas(modifier = modifier) {
        val chartLeft = leftPadding
        val chartWidth = size.width - chartLeft - chartPadding
        val chartHeight = size.height - chartPadding * 2
        val candleCount = candles.size
        if (candleCount == 0) return@Canvas

        val candleSpacing = chartWidth / candleCount
        val candleWidth = (candleSpacing * 0.4f).coerceIn(2f, 12f)
        val wickWidth = 1.5f

        fun yPos(ppm: Int): Float {
            return chartPadding + chartHeight * (1f - (ppm - yMin).toFloat() / (yMax - yMin))
        }

        // Draw horizontal grid lines with labels
        gridLines.forEach { ppm ->
            val y = yPos(ppm)
            val isTarget = ppm == 800
            val isCritical = ppm == 2000
            val lineColor = when {
                isCritical -> Red.copy(alpha = 0.3f)
                isTarget -> Emerald.copy(alpha = 0.3f)
                else -> Border.copy(alpha = 0.2f)
            }
            val effect = if (isTarget || isCritical) {
                PathEffect.dashPathEffect(floatArrayOf(8f, 6f))
            } else null

            drawLine(
                color = lineColor,
                start = Offset(chartLeft, y),
                end = Offset(size.width - chartPadding, y),
                strokeWidth = 0.5f,
                pathEffect = effect,
            )

            // Y-axis label
            val label = "$ppm"
            val textResult = textMeasurer.measure(label, gridLabelStyle)
            drawText(
                textResult,
                topLeft = Offset(chartLeft - textResult.size.width - 4f, y - textResult.size.height / 2f),
            )
        }

        // Draw candles
        candles.forEachIndexed { i, candle ->
            val centerX = chartLeft + candleSpacing * (i + 0.5f)
            val isFalling = candle.close < candle.open
            val color = if (isFalling) Emerald else Amber

            drawLine(
                color = color.copy(alpha = 0.6f),
                start = Offset(centerX, yPos(candle.high)),
                end = Offset(centerX, yPos(candle.low)),
                strokeWidth = wickWidth,
            )

            val bodyTop = yPos(maxOf(candle.open, candle.close))
            val bodyBottom = yPos(minOf(candle.open, candle.close))
            val bodyHeight = (bodyBottom - bodyTop).coerceAtLeast(2f)

            drawRect(
                color = color.copy(alpha = 0.8f),
                topLeft = Offset(centerX - candleWidth / 2, bodyTop),
                size = Size(candleWidth, bodyHeight),
            )
        }
    }
}

@Composable
private fun OHLCStat(label: String, value: String, color: Color) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Text(label, style = MaterialTheme.typography.labelSmall.copy(fontSize = 8.sp), color = TextSecondary)
        Text(value, style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace), color = color)
    }
}

@Composable
private fun LegendDot(color: Color, label: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(modifier = Modifier.size(8.dp).clip(RoundedCornerShape(2.dp)).background(color))
        Spacer(Modifier.width(4.dp))
        Text(label, style = MaterialTheme.typography.labelSmall.copy(fontSize = 9.sp), color = TextSecondary)
    }
}

// ── Section 3: Daily Stats ──

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DailyStatsSection(stat: DailyStat) {
    FlowRow(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
        maxItemsInEachRow = 2,
    ) {
        val tileModifier = Modifier.weight(1f)

        StatTile("PRESENCE HOURS", "${stat.presenceHours}h", Emerald, tileModifier)
        StatTile("DOOR EVENTS", "${stat.doorEvents}", Amber, tileModifier)
        StatTile(
            "VENT RUNTIME",
            formatMinutes(stat.ervRuntimeMin),
            Emerald,
            tileModifier,
        )
        StatTile(
            "HVAC RUNTIME",
            formatMinutes(stat.hvacRuntimeMin),
            Amber,
            tileModifier,
        )
    }
}

@Composable
private fun StatTile(label: String, value: String, accentColor: Color, modifier: Modifier = Modifier) {
    val shape = RoundedCornerShape(12.dp)

    Column(
        modifier = modifier
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .drawBehind {
                // Left accent border
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

private fun formatMinutes(minutes: Int): String {
    if (minutes < 60) return "${minutes}m"
    val h = minutes / 60
    val m = minutes % 60
    return if (m > 0) "${h}h ${m}m" else "${h}h"
}

// ── Section 3: Temperature Line Chart ──

@Composable
private fun TemperatureSection(points: List<TempPoint>) {
    val shape = RoundedCornerShape(12.dp)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
    ) {
        Text("TEMPERATURE", style = MaterialTheme.typography.labelLarge, color = TextPrimary)

        Spacer(Modifier.height(16.dp))

        TemperatureChart(points = points, modifier = Modifier.fillMaxWidth().height(160.dp))

        Spacer(Modifier.height(12.dp))

        // Summary
        val avg = points.map { it.avgF }.average()
        val high = points.maxOf { it.maxF }
        val low = points.minOf { it.minF }

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceEvenly,
        ) {
            OHLCStat("AVERAGE", "${"%.1f".format(avg)}°F", TextPrimary)
            OHLCStat("HIGH", "${"%.1f".format(high)}°F", Orange)
            OHLCStat("LOW", "${"%.1f".format(low)}°F", Blue)
        }
    }
}

@Composable
private fun TemperatureChart(points: List<TempPoint>, modifier: Modifier = Modifier) {
    val chartPadding = 8f
    val dataMax = points.maxOf { it.maxF }.toFloat()
    val dataMin = points.minOf { it.minF }.toFloat()
    val yPadding = maxOf(2f, (dataMax - dataMin) * 0.15f)
    val yMax = dataMax + yPadding
    val yMin = dataMin - yPadding

    Canvas(modifier = modifier) {
        val chartWidth = size.width - chartPadding * 2
        val chartHeight = size.height - chartPadding * 2
        if (points.isEmpty()) return@Canvas

        fun yPos(temp: Float): Float {
            return chartPadding + chartHeight * (1f - (temp - yMin) / (yMax - yMin))
        }

        // Draw range band (min to max) as subtle fill
        for (i in 0 until points.size - 1) {
            val x1 = chartPadding + chartWidth * i / (points.size - 1).toFloat()
            val x2 = chartPadding + chartWidth * (i + 1) / (points.size - 1).toFloat()
            val segWidth = x2 - x1

            // Min-max band
            val topY = minOf(yPos(points[i].maxF.toFloat()), yPos(points[i + 1].maxF.toFloat()))
            val botY = maxOf(yPos(points[i].minF.toFloat()), yPos(points[i + 1].minF.toFloat()))
            drawRect(
                color = Orange.copy(alpha = 0.08f),
                topLeft = Offset(x1, topY),
                size = Size(segWidth, (botY - topY).coerceAtLeast(1f)),
            )
        }

        // Draw average line
        for (i in 0 until points.size - 1) {
            val x1 = chartPadding + chartWidth * i / (points.size - 1).toFloat()
            val x2 = chartPadding + chartWidth * (i + 1) / (points.size - 1).toFloat()
            val y1 = yPos(points[i].avgF.toFloat())
            val y2 = yPos(points[i + 1].avgF.toFloat())

            drawLine(
                color = Orange,
                start = Offset(x1, y1),
                end = Offset(x2, y2),
                strokeWidth = 2f,
            )
        }

        // Draw dots at each point
        points.forEachIndexed { i, point ->
            val x = chartPadding + chartWidth * i / (points.size - 1).coerceAtLeast(1).toFloat()
            val y = yPos(point.avgF.toFloat())
            drawCircle(
                color = Orange,
                radius = 3f,
                center = Offset(x, y),
            )
        }
    }
}
