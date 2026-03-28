package com.rajesh.officeclimate.ui.productivity

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
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.rajesh.officeclimate.data.model.LeverageDay
import com.rajesh.officeclimate.data.model.LeverageResponse
import com.rajesh.officeclimate.data.model.ProjectCount
import com.rajesh.officeclimate.data.model.ProjectFocusDay
import com.rajesh.officeclimate.ui.history.LegendDot
import com.rajesh.officeclimate.ui.history.SessionsSection
import com.rajesh.officeclimate.ui.history.StatTile
import com.rajesh.officeclimate.ui.history.TIME_GRID
import com.rajesh.officeclimate.ui.history.TimeAxisHeader
import com.rajesh.officeclimate.ui.history.dayOfWeekLabel
import com.rajesh.officeclimate.ui.history.timeToFraction
import com.rajesh.officeclimate.ui.theme.Amber
import com.rajesh.officeclimate.ui.theme.Background
import com.rajesh.officeclimate.ui.theme.Blue
import com.rajesh.officeclimate.ui.theme.Border
import com.rajesh.officeclimate.ui.theme.Cyan
import com.rajesh.officeclimate.ui.theme.Emerald
import com.rajesh.officeclimate.ui.theme.Red
import com.rajesh.officeclimate.ui.theme.Surface
import com.rajesh.officeclimate.ui.theme.TextPrimary
import com.rajesh.officeclimate.ui.theme.TextSecondary
import com.rajesh.officeclimate.ui.theme.projectColorFor
import java.time.LocalDate
import java.util.Locale

private data class MetricTileSpec(
    val label: String,
    val value: String,
    val accent: Color,
    val trend: List<Float?> = emptyList(),
    val trendLabel: String = "",
)

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun ProductivityScreen(
    viewModel: ProductivityViewModel = viewModel(),
) {
    val sessions by viewModel.sessions.collectAsState()
    val orchestration by viewModel.orchestration.collectAsState()
    val projectFocus by viewModel.projectFocus.collectAsState()
    val leverage by viewModel.leverage.collectAsState()
    val isLoading by viewModel.isLoading.collectAsState()
    val error by viewModel.error.collectAsState()

    val allFailed = !isLoading &&
        sessions == null &&
        orchestration == null &&
        projectFocus == null &&
        leverage == null

    val today = LocalDate.now().toString()
    val todayOrchestration = orchestration?.days?.firstOrNull { it.date == today }
    val todayProjectFocus = projectFocus?.days?.firstOrNull { it.date == today }
    val todayLeverage = leverage?.days?.firstOrNull { it.date == today }
    val topProject = todayProjectFocus?.projects?.maxByOrNull { it.messages }?.name ?: "none"
    val activeHours = when {
        todayOrchestration?.firstPrompt != null && todayOrchestration?.lastPrompt != null ->
            "${todayOrchestration?.firstPrompt} - ${todayOrchestration?.lastPrompt}"
        else -> "--"
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Background),
    ) {
        if (isLoading && sessions == null) {
            Column(
                modifier = Modifier.fillMaxSize(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                CircularProgressIndicator(color = Emerald)
                Spacer(Modifier.height(16.dp))
                Text("Loading productivity...", color = TextSecondary)
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
                    text = "Productivity",
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
                            "Could not load productivity data",
                            style = MaterialTheme.typography.titleMedium,
                            color = Red,
                        )
                        Spacer(Modifier.height(8.dp))
                        Text(
                            error ?: "Check that the server is updated with productivity endpoints",
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

                sessions?.let { data ->
                    SessionsSection(sessions = data.sessions, summary = data.summary)
                }

                orchestration?.let { data ->
                    OrchestrationSection(days = data.days)
                }

                projectFocus?.let { data ->
                    ProjectFocusSection(days = data.days)
                }

                Text(
                    "TODAY",
                    style = MaterialTheme.typography.labelLarge,
                    color = TextPrimary,
                )
                FlowRow(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    maxItemsInEachRow = 2,
                ) {
                    val tileModifier = Modifier.weight(1f)
                    StatTile("MESSAGES", formatCount(todayOrchestration?.messages ?: 0), Emerald, tileModifier)
                    StatTile("SESSIONS", formatCount(todayOrchestration?.sessions ?: 0), Amber, tileModifier)
                    StatTile("TOP PROJECT", topProject, Blue, tileModifier)
                    StatTile("ACTIVE HOURS", activeHours, Cyan, tileModifier)
                }

                leverage?.let { leverageData ->
                    LeverageTilesSection(
                        title = "TODAY LEVERAGE",
                        tiles = todayLeverageTiles(todayLeverage, leverageData.days),
                    )
                    LeverageTilesSection(
                        title = "THIS WEEK LEVERAGE",
                        tiles = weekLeverageTiles(leverageData),
                    )
                }

                Spacer(Modifier.height(64.dp))
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LeverageTilesSection(
    title: String,
    tiles: List<MetricTileSpec>,
) {
    Text(
        text = title,
        style = MaterialTheme.typography.labelLarge,
        color = TextPrimary,
    )
    FlowRow(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
        maxItemsInEachRow = 2,
    ) {
        tiles.forEach { tile ->
            LeverageTile(
                tile = tile,
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun LeverageTile(tile: MetricTileSpec, modifier: Modifier = Modifier) {
    val shape = RoundedCornerShape(12.dp)

    Column(
        modifier = modifier
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = tile.label,
            style = MaterialTheme.typography.labelSmall,
            color = TextSecondary,
        )
        Text(
            text = tile.value,
            style = MaterialTheme.typography.titleLarge,
            color = tile.accent,
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.SemiBold,
        )
        SparklineChart(
            values = tile.trend,
            accent = tile.accent,
            modifier = Modifier
                .fillMaxWidth()
                .height(40.dp),
        )
        if (tile.trendLabel.isNotEmpty()) {
            Text(
                text = tile.trendLabel,
                style = MaterialTheme.typography.labelSmall,
                color = TextSecondary,
            )
        }
    }
}

@Composable
private fun OrchestrationSection(days: List<com.rajesh.officeclimate.data.model.OrchestrationDay>) {
    val shape = RoundedCornerShape(12.dp)
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
    ) {
        Text("ORCHESTRATION ACTIVITY", style = MaterialTheme.typography.labelLarge, color = TextPrimary)
        Spacer(Modifier.height(12.dp))
        TimeAxisHeader()
        Spacer(Modifier.height(4.dp))
        days.forEach { day ->
            OrchestrationDayRow(day)
            Spacer(Modifier.height(4.dp))
        }

        Spacer(Modifier.height(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            LegendDot(Emerald, "CLAUDE")
            LegendDot(Amber, "CODEX")
        }
    }
}

@Composable
private fun OrchestrationDayRow(day: com.rajesh.officeclimate.data.model.OrchestrationDay) {
    val grouped = day.timestamps.groupBy { it.time }
    val textMeasurer = rememberTextMeasurer()
    val labelStyle = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontSize = 8.sp,
        color = TextSecondary,
    )

    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = dayOfWeekLabel(day.date),
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.Bold,
                fontSize = 10.sp,
            ),
            color = TextPrimary,
            modifier = Modifier.width(36.dp),
        )

        Canvas(
            modifier = Modifier
                .weight(1f)
                .height(28.dp),
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

            grouped.forEach { (time, prompts) ->
                val x = timeToFraction(time) * size.width
                prompts.take(3).forEachIndexed { index, prompt ->
                    val y = 7f + index * 7f
                    drawCircle(
                        color = if (prompt.tool == "codex") Amber else Emerald,
                        radius = 4f,
                        center = Offset(x, y),
                    )
                }
                if (prompts.size > 3) {
                    val overflow = textMeasurer.measure("+${prompts.size - 3}", labelStyle)
                    drawText(
                        overflow,
                        topLeft = Offset(
                            (x + 6f).coerceAtMost(size.width - overflow.size.width),
                            size.height - overflow.size.height,
                        ),
                    )
                }
            }
        }

        Text(
            text = formatCount(day.messages),
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

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ProjectFocusSection(days: List<ProjectFocusDay>) {
    val shape = RoundedCornerShape(12.dp)
    val projectColors = days
        .flatMap { day -> day.projects.map(ProjectCount::name) }
        .distinct()
        .sorted()
        .associateWith(::projectColorFor)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
    ) {
        Text("PROJECT FOCUS", style = MaterialTheme.typography.labelLarge, color = TextPrimary)
        Spacer(Modifier.height(12.dp))
        TimeAxisHeader()
        Spacer(Modifier.height(4.dp))

        days.forEach { day ->
            ProjectFocusRow(day = day, projectColors = projectColors)
            Spacer(Modifier.height(6.dp))
        }

        if (projectColors.isNotEmpty()) {
            Spacer(Modifier.height(8.dp))
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                projectColors.forEach { (name, color) ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(
                            modifier = Modifier
                                .width(8.dp)
                                .height(8.dp)
                                .clip(CircleShape)
                                .background(color),
                        )
                        Spacer(Modifier.width(4.dp))
                        Text(name, style = MaterialTheme.typography.labelSmall, color = TextSecondary)
                    }
                }
            }
        }
    }
}

@Composable
private fun ProjectFocusRow(day: ProjectFocusDay, projectColors: Map<String, Color>) {
    val rowHeight = (day.projects.size * 12 + (day.projects.size - 1).coerceAtLeast(0) * 4)
        .coerceAtLeast(16)

    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.Top,
    ) {
        Text(
            text = dayOfWeekLabel(day.date),
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.Bold,
                fontSize = 10.sp,
            ),
            color = TextPrimary,
            modifier = Modifier
                .width(36.dp)
                .padding(top = 2.dp),
        )

        Canvas(
            modifier = Modifier
                .weight(1f)
                .height(rowHeight.dp),
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

            if (day.total == 0 || day.projects.isEmpty()) {
                return@Canvas
            }

            val barHeight = 10.dp.toPx()
            val barSpacing = 4.dp.toPx()

            day.projects.forEachIndexed { index, project ->
                val start = project.firstPrompt?.let(::timeToFraction) ?: 0f
                val end = project.lastPrompt?.let(::timeToFraction) ?: start
                val top = index * (barHeight + barSpacing)
                val width = ((end - start) * size.width).coerceAtLeast(8.dp.toPx())

                drawRoundRect(
                    color = (projectColors[project.name] ?: TextSecondary).copy(alpha = 0.8f),
                    topLeft = Offset(start * size.width, top),
                    size = Size(width, barHeight),
                    cornerRadius = CornerRadius(4.dp.toPx(), 4.dp.toPx()),
                )
            }
        }

        Text(
            text = formatCount(day.total),
            style = MaterialTheme.typography.labelSmall.copy(
                fontFamily = FontFamily.Monospace,
                fontSize = 10.sp,
            ),
            color = TextSecondary,
            modifier = Modifier
                .width(44.dp)
                .padding(top = 2.dp)
                .padding(start = 8.dp),
        )
    }
}

@Composable
private fun SparklineChart(
    values: List<Float?>,
    accent: Color,
    modifier: Modifier = Modifier,
) {
    val definedValues = values.filterNotNull()

    Canvas(modifier = modifier) {
        val gridColor = Border.copy(alpha = 0.35f)
        val height = size.height
        val width = size.width

        drawLine(
            color = gridColor,
            start = Offset(0f, height),
            end = Offset(width, height),
            strokeWidth = 1f,
        )
        drawLine(
            color = gridColor,
            start = Offset(0f, height / 2f),
            end = Offset(width, height / 2f),
            strokeWidth = 1f,
        )

        if (definedValues.isEmpty()) {
            return@Canvas
        }

        val minValue = definedValues.minOrNull() ?: 0f
        val maxValue = definedValues.maxOrNull() ?: 0f
        val valueRange = (maxValue - minValue).takeIf { it > 0.001f } ?: 1f
        val maxIndex = (values.lastIndex).coerceAtLeast(1)

        fun pointAt(index: Int, value: Float): Offset {
            val x = index.toFloat() / maxIndex * width
            val normalized = (value - minValue) / valueRange
            val y = height - (normalized * (height - 4.dp.toPx())) - 2.dp.toPx()
            return Offset(x, y)
        }

        var previousPoint: Offset? = null
        var lastPoint: Offset? = null

        values.forEachIndexed { index, value ->
            if (value == null) {
                previousPoint = null
                return@forEachIndexed
            }

            val point = pointAt(index, value)
            if (previousPoint != null) {
                drawLine(
                    color = accent,
                    start = previousPoint!!,
                    end = point,
                    strokeWidth = 3f,
                    cap = StrokeCap.Round,
                )
            }
            previousPoint = point
            lastPoint = point
        }

        lastPoint?.let {
            drawCircle(
                color = accent,
                radius = 3.5.dp.toPx(),
                center = it,
            )
        }
    }
}

private fun todayLeverageTiles(today: LeverageDay?, days: List<LeverageDay>): List<MetricTileSpec> = listOf(
    MetricTileSpec(
        "LINES/PROMPT",
        today?.linesPerPrompt.formatDecimalOrDash(),
        Emerald,
        trend = days.map { it.linesPerPrompt?.toFloat() },
        trendLabel = "7D TREND",
    ),
    MetricTileSpec(
        "COMMITS",
        formatCount(today?.commits ?: 0),
        Amber,
        trend = days.map { it.commits.toFloat() },
        trendLabel = "7D TREND",
    ),
    MetricTileSpec(
        "PRS MERGED",
        formatCount(today?.prsMerged ?: 0),
        Blue,
        trend = days.map { it.prsMerged.toFloat() },
        trendLabel = "7D TREND",
    ),
    MetricTileSpec(
        "LINES CHANGED",
        formatCount(today?.linesChanged ?: 0),
        Cyan,
        trend = days.map { it.linesChanged.toFloat() },
        trendLabel = "7D TREND",
    ),
)

private fun weekLeverageTiles(leverage: LeverageResponse): List<MetricTileSpec> = listOf(
    MetricTileSpec(
        "WEEK LINES",
        formatCount(leverage.week.linesChanged),
        Emerald,
        trend = leverage.days.map { it.linesChanged.toFloat() },
        trendLabel = "DAILY TOTALS",
    ),
    MetricTileSpec(
        "WEEK COMMITS",
        formatCount(leverage.week.commits),
        Amber,
        trend = leverage.days.map { it.commits.toFloat() },
        trendLabel = "DAILY TOTALS",
    ),
    MetricTileSpec(
        "WEEK PRS",
        formatCount(leverage.week.prsMerged),
        Blue,
        trend = leverage.days.map { it.prsMerged.toFloat() },
        trendLabel = "DAILY TOTALS",
    ),
    MetricTileSpec(
        "AVG L/PROMPT",
        leverage.week.linesPerPrompt.formatDecimalOrDash(),
        Cyan,
        trend = leverage.days.map { it.linesPerPrompt?.toFloat() },
        trendLabel = "DAILY EFFICIENCY",
    ),
)

private fun formatCount(value: Int): String = String.format(Locale.US, "%,d", value)

private fun Double?.formatDecimalOrDash(): String = this?.let { String.format(Locale.US, "%.1f", it) } ?: "--"
