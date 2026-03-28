package com.rajesh.officeclimate.ui.history

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
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
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.rajesh.officeclimate.data.model.CO2Candle
import com.rajesh.officeclimate.data.model.DailyStat
import com.rajesh.officeclimate.data.model.OpeningDay
import com.rajesh.officeclimate.data.model.OpeningPeriod
import com.rajesh.officeclimate.data.model.TempPoint
import com.rajesh.officeclimate.ui.theme.Amber
import com.rajesh.officeclimate.ui.theme.Background
import com.rajesh.officeclimate.ui.theme.Blue
import com.rajesh.officeclimate.ui.theme.Border
import com.rajesh.officeclimate.ui.theme.Cyan
import com.rajesh.officeclimate.ui.theme.Emerald
import com.rajesh.officeclimate.ui.theme.Orange
import com.rajesh.officeclimate.ui.theme.Red
import com.rajesh.officeclimate.ui.theme.Surface
import com.rajesh.officeclimate.ui.theme.TextPrimary
import com.rajesh.officeclimate.ui.theme.TextSecondary
import java.time.LocalDate
import kotlin.math.ceil
import kotlin.math.floor

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun HistoryScreen(
    viewModel: HistoryViewModel = viewModel(),
) {
    val ohlcData by viewModel.ohlcData.collectAsState()
    val dailyStats by viewModel.dailyStats.collectAsState()
    val temperature by viewModel.temperature.collectAsState()
    val openings by viewModel.openings.collectAsState()
    val isLoading by viewModel.isLoading.collectAsState()
    val selectedRange by viewModel.selectedRange.collectAsState()
    val error by viewModel.error.collectAsState()

    val allFailed = !isLoading &&
        ohlcData == null &&
        dailyStats == null &&
        temperature == null &&
        openings == null

    Box(modifier = Modifier.fillMaxSize().background(Background)) {
        if (isLoading && ohlcData == null) {
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

                ohlcData?.let { data ->
                    OHLCSection(
                        candles = data.candles,
                        selectedRange = selectedRange,
                        onRangeSelected = viewModel::selectOHLCRange,
                    )
                }

                openings?.let { data ->
                    OpeningsSection(days = data.days)
                }

                temperature?.let { data ->
                    if (data.points.isNotEmpty()) {
                        TemperatureSection(points = data.points)
                    }
                }

                dailyStats?.let { data ->
                    val todayStat = data.stats.firstOrNull { it.date == LocalDate.now().toString() }
                    if (todayStat != null) {
                        Text(
                            "TODAY",
                            style = MaterialTheme.typography.labelLarge,
                            color = TextPrimary,
                        )
                        DailyStatsSection(stat = todayStat)
                    }
                }

                Spacer(Modifier.height(64.dp))
            }
        }
    }
}

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
                        contentPadding = PaddingValues(horizontal = 10.dp, vertical = 0.dp),
                    ) {
                        Text(range.label, style = MaterialTheme.typography.labelSmall)
                    }
                }
            }
        }

        Spacer(Modifier.height(16.dp))

        if (candles.isNotEmpty()) {
            CandlestickChart(candles = candles, modifier = Modifier.fillMaxWidth().height(200.dp))
            Spacer(Modifier.height(12.dp))

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
    val leftPadding = 40f
    val chartPadding = 8f
    val dataHigh = candles.maxOf { it.high }
    val dataLow = candles.minOf { it.low }
    val yMax = maxOf(dataHigh + 100, if (dataHigh > 700) 900 else dataHigh + 200)
        .let { if (dataHigh > 1800) maxOf(it, 2200) else it }
    val yMin = maxOf(0, dataLow - 100)

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
        if (candles.isEmpty()) return@Canvas

        val candleSpacing = chartWidth / candles.size
        val candleWidth = (candleSpacing * 0.4f).coerceIn(2f, 12f)

        fun yPos(ppm: Int): Float {
            return chartPadding + chartHeight * (1f - (ppm - yMin).toFloat() / (yMax - yMin))
        }

        gridLines.forEach { ppm ->
            val y = yPos(ppm)
            val isTarget = ppm == 800
            val isCritical = ppm == 2000
            val lineColor = when {
                isCritical -> Red.copy(alpha = 0.3f)
                isTarget -> Emerald.copy(alpha = 0.3f)
                else -> Border.copy(alpha = 0.2f)
            }
            val effect = if (isTarget || isCritical) PathEffect.dashPathEffect(floatArrayOf(8f, 6f)) else null

            drawLine(
                color = lineColor,
                start = Offset(chartLeft, y),
                end = Offset(size.width - chartPadding, y),
                strokeWidth = 0.5f,
                pathEffect = effect,
            )

            val textResult = textMeasurer.measure("$ppm", gridLabelStyle)
            drawText(
                textResult,
                topLeft = Offset(chartLeft - textResult.size.width - 4f, y - textResult.size.height / 2f),
            )
        }

        candles.forEachIndexed { i, candle ->
            val centerX = chartLeft + candleSpacing * (i + 0.5f)
            val color = if (candle.close < candle.open) Emerald else Amber

            drawLine(
                color = color.copy(alpha = 0.6f),
                start = Offset(centerX, yPos(candle.high)),
                end = Offset(centerX, yPos(candle.low)),
                strokeWidth = 1.5f,
            )

            val bodyTop = yPos(maxOf(candle.open, candle.close))
            val bodyBottom = yPos(minOf(candle.open, candle.close))
            drawRect(
                color = color.copy(alpha = 0.8f),
                topLeft = Offset(centerX - candleWidth / 2, bodyTop),
                size = Size(candleWidth, (bodyBottom - bodyTop).coerceAtLeast(2f)),
            )
        }
    }
}

@Composable
private fun OpeningsSection(days: List<OpeningDay>) {
    val shape = RoundedCornerShape(12.dp)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
    ) {
        Text("DOOR / WINDOW", style = MaterialTheme.typography.labelLarge, color = TextPrimary)
        Spacer(Modifier.height(12.dp))
        TimeAxisHeader(durationColumnWidth = 0)
        Spacer(Modifier.height(4.dp))
        days.forEach { day ->
            OpeningDayRow(day)
            Spacer(Modifier.height(4.dp))
        }

        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            LegendDot(Cyan, "DOOR")
            LegendDot(Blue, "WINDOW")
        }
    }
}

@Composable
private fun OpeningDayRow(day: OpeningDay) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = dayOfWeekLabel(day.date),
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontSize = 10.sp,
            ),
            color = TextPrimary,
            modifier = Modifier.width(36.dp),
        )

        Canvas(
            modifier = Modifier
                .weight(1f)
                .height(18.dp),
        ) {
            TIME_GRID.forEach { (_, frac) ->
                val x = frac * size.width
                drawLine(
                    color = Border.copy(alpha = 0.3f),
                    start = Offset(x, 0f),
                    end = Offset(x, size.height),
                    strokeWidth = 0.5f,
                )
            }

            drawIntervals(day.door, yTop = 2f, bandHeight = 5f, color = Cyan)
            drawIntervals(day.window, yTop = 11f, bandHeight = 5f, color = Blue)
        }
    }
}

private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawIntervals(
    periods: List<OpeningPeriod>,
    yTop: Float,
    bandHeight: Float,
    color: Color,
) {
    periods.forEach { period ->
        val startX = timeToFraction(period.open) * size.width
        val endX = timeToFraction(period.close) * size.width
        drawRect(
            color = color.copy(alpha = 0.65f),
            topLeft = Offset(startX, yTop),
            size = Size((endX - startX).coerceAtLeast(2f), bandHeight),
        )
    }
}

@Composable
private fun OHLCStat(label: String, value: String, color: Color) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Text(label, style = MaterialTheme.typography.labelSmall.copy(fontSize = 8.sp), color = TextSecondary)
        Text(value, style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace), color = color)
    }
}

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
        StatTile("VENT RUNTIME", formatMinutes(stat.ervRuntimeMin), Emerald, tileModifier)
        StatTile("HVAC RUNTIME", formatMinutes(stat.hvacRuntimeMin), Amber, tileModifier)
    }
}

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
    val leftPadding = 40f
    val chartPadding = 8f
    val dataMax = points.maxOf { it.maxF }.toFloat()
    val dataMin = points.minOf { it.minF }.toFloat()
    val yPadding = maxOf(2f, (dataMax - dataMin) * 0.15f)
    val yMax = dataMax + yPadding
    val yMin = dataMin - yPadding
    val gridStep = 5
    val gridStart = floor(yMin / gridStep).toInt() * gridStep
    val gridEnd = ceil(yMax / gridStep).toInt() * gridStep
    val gridLines = (gridStart..gridEnd step gridStep).toList()
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
        if (points.isEmpty()) return@Canvas

        fun yPos(temp: Float): Float {
            return chartPadding + chartHeight * (1f - (temp - yMin) / (yMax - yMin))
        }

        gridLines.forEach { temp ->
            val y = yPos(temp.toFloat())
            drawLine(
                color = Border.copy(alpha = 0.3f),
                start = Offset(chartLeft, y),
                end = Offset(size.width - chartPadding, y),
                strokeWidth = 0.5f,
            )
            val textResult = textMeasurer.measure("$temp", gridLabelStyle)
            drawText(
                textResult,
                topLeft = Offset(chartLeft - textResult.size.width - 4f, y - textResult.size.height / 2f),
            )
        }

        for (i in 0 until points.size - 1) {
            val x1 = chartLeft + chartWidth * i / (points.size - 1).toFloat()
            val x2 = chartLeft + chartWidth * (i + 1) / (points.size - 1).toFloat()
            val segWidth = x2 - x1

            val topY = minOf(yPos(points[i].maxF.toFloat()), yPos(points[i + 1].maxF.toFloat()))
            val botY = maxOf(yPos(points[i].minF.toFloat()), yPos(points[i + 1].minF.toFloat()))
            drawRect(
                color = Orange.copy(alpha = 0.08f),
                topLeft = Offset(x1, topY),
                size = Size(segWidth, (botY - topY).coerceAtLeast(1f)),
            )
        }

        for (i in 0 until points.size - 1) {
            val x1 = chartLeft + chartWidth * i / (points.size - 1).toFloat()
            val x2 = chartLeft + chartWidth * (i + 1) / (points.size - 1).toFloat()
            drawLine(
                color = Orange,
                start = Offset(x1, yPos(points[i].avgF.toFloat())),
                end = Offset(x2, yPos(points[i + 1].avgF.toFloat())),
                strokeWidth = 2f,
            )
        }

        points.forEachIndexed { i, point ->
            val x = chartLeft + chartWidth * i / (points.size - 1).coerceAtLeast(1).toFloat()
            drawCircle(
                color = Orange,
                radius = 3f,
                center = Offset(x, yPos(point.avgF.toFloat())),
            )
        }
    }
}
