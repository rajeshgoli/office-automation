package com.rajesh.officeclimate.ui.projects

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material.icons.filled.ArrowDownward
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.rajesh.officeclimate.data.model.EngramCurrent
import com.rajesh.officeclimate.data.model.ProjectLeverageDay
import com.rajesh.officeclimate.data.model.ProjectLeverageProject
import com.rajesh.officeclimate.data.model.ProjectLeverageResponse
import com.rajesh.officeclimate.data.model.ProjectLeverageWeek
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
import java.util.Locale
import kotlin.math.abs
import kotlin.math.roundToInt

private enum class TrendDirection {
    Up,
    Down,
    Flat,
}

private data class ProjectMetricUi(
    val value: String,
    val label: String,
    val accent: Color,
)

private data class ProjectTrendUi(
    val values: List<Float?>,
    val accent: Color,
    val direction: TrendDirection,
    val directionText: String,
    val context: String,
)

private data class ProjectCardUi(
    val title: String,
    val summary: String,
    val color: Color,
    val metrics: List<ProjectMetricUi>,
    val trend: ProjectTrendUi? = null,
    val footer: String? = null,
    val statusLabel: String? = null,
    val statusValue: String? = null,
    val statusColor: Color? = null,
)

@Composable
fun ProjectsScreen(
    viewModel: ProjectsViewModel = viewModel(),
) {
    val projectLeverage by viewModel.projectLeverage.collectAsState()
    val projectLeverageComparison by viewModel.projectLeverageComparison.collectAsState()
    val isLoading by viewModel.isLoading.collectAsState()
    val error by viewModel.error.collectAsState()

    val cards = projectLeverage?.let { buildProjectCards(it, projectLeverageComparison) }.orEmpty()

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Background),
    ) {
        if (isLoading && projectLeverage == null) {
            Column(
                modifier = Modifier.fillMaxSize(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                CircularProgressIndicator(color = Emerald)
                Spacer(Modifier.height(16.dp))
                Text("Loading projects...", color = TextSecondary)
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
                    text = "Projects",
                    style = MaterialTheme.typography.headlineMedium,
                    color = TextPrimary,
                )

                if (!isLoading && projectLeverage == null) {
                    Spacer(Modifier.height(32.dp))
                    Column(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text(
                            "Could not load project leverage",
                            style = MaterialTheme.typography.titleMedium,
                            color = Red,
                        )
                        Spacer(Modifier.height(8.dp))
                        Text(
                            error ?: "Check that the server is updated with project leverage endpoints",
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

                cards.forEach { card ->
                    ProjectCard(card)
                }

                if (projectLeverage != null && cards.isEmpty()) {
                    Text(
                        text = "No active projects in the last 7 days",
                        style = MaterialTheme.typography.bodyMedium,
                        color = TextSecondary,
                    )
                }

                Spacer(Modifier.height(64.dp))
            }
        }
    }
}

@Composable
private fun ProjectCard(card: ProjectCardUi) {
    val shape = RoundedCornerShape(16.dp)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Surface.copy(alpha = 0.92f))
            .border(1.dp, Border, shape)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Box(
                modifier = Modifier
                    .size(12.dp)
                    .clip(CircleShape)
                    .background(card.color),
            )
            Text(
                text = card.title.uppercase(Locale.US),
                style = MaterialTheme.typography.titleMedium,
                color = TextPrimary,
                fontWeight = FontWeight.Bold,
                letterSpacing = 0.6.sp,
            )
        }

        Text(
            text = card.summary,
            style = MaterialTheme.typography.bodyMedium,
            color = TextSecondary,
        )

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            card.metrics.forEach { metric ->
                Column(
                    modifier = Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text(
                        text = metric.value,
                        color = metric.accent,
                        fontSize = 20.sp,
                        fontWeight = FontWeight.SemiBold,
                        fontFamily = FontFamily.Monospace,
                    )
                    Text(
                        text = metric.label.uppercase(Locale.US),
                        color = TextSecondary,
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
            }
        }

        card.trend?.let {
            ProjectTrendBlock(it)
        }

        card.footer?.let {
            Text(
                text = it,
                style = MaterialTheme.typography.bodySmall,
                color = TextSecondary,
            )
        }

        if (card.statusLabel != null && card.statusValue != null && card.statusColor != null) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Text(
                    text = card.statusLabel,
                    style = MaterialTheme.typography.bodySmall,
                    color = TextSecondary,
                )
                Text(
                    text = card.statusValue,
                    style = MaterialTheme.typography.bodySmall,
                    color = card.statusColor,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }
    }
}

@Composable
private fun ProjectTrendBlock(trend: ProjectTrendUi) {
    val shape = RoundedCornerShape(12.dp)
    val directionColor = when (trend.direction) {
        TrendDirection.Up -> Emerald
        TrendDirection.Down -> Red
        TrendDirection.Flat -> TextSecondary
    }
    val directionIcon = when (trend.direction) {
        TrendDirection.Up -> Icons.Filled.ArrowUpward
        TrendDirection.Down -> Icons.Filled.ArrowDownward
        TrendDirection.Flat -> Icons.AutoMirrored.Filled.ArrowForward
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Background.copy(alpha = 0.32f))
            .border(1.dp, Border, shape)
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = "7D ACTIVITY",
            style = MaterialTheme.typography.labelSmall,
            color = TextSecondary,
        )
        SparklineChart(
            values = trend.values,
            accent = trend.accent,
            modifier = Modifier
                .fillMaxWidth()
                .height(40.dp),
        )
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Icon(
                imageVector = directionIcon,
                contentDescription = null,
                tint = directionColor,
                modifier = Modifier.size(14.dp),
            )
            Text(
                text = trend.directionText,
                style = MaterialTheme.typography.labelSmall,
                color = directionColor,
            )
        }
        Text(
            text = trend.context,
            style = MaterialTheme.typography.labelSmall,
            color = TextSecondary,
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
        val maxIndex = values.lastIndex.coerceAtLeast(1)
        val verticalPadding = 2.dp.toPx()

        fun pointAt(index: Int, value: Float): Offset {
            val x = index.toFloat() / maxIndex * width
            val normalized = (value - minValue) / valueRange
            val y = height - (normalized * (height - verticalPadding * 2f)) - verticalPadding
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
            previousPoint?.let {
                drawLine(
                    color = accent,
                    start = it,
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

private fun buildProjectCards(
    response: ProjectLeverageResponse,
    comparisonResponse: ProjectLeverageResponse?,
): List<ProjectCardUi> {
    val projects = response.projects
    val comparisonProjects = comparisonResponse?.projects.orEmpty()

    return listOfNotNull(
        buildAgentOsCard(projects["agent-os"], comparisonProjects["agent-os"]),
        buildSessionManagerCard(projects["session-manager"], comparisonProjects["session-manager"]),
        buildOfficeAutomationCard(projects["office-automate"], comparisonProjects["office-automate"]),
        buildDeskbarTaskbarPlaceholderCard(),
        buildEngramCard(projects["engram"], comparisonProjects["engram"]),
    )
}

private fun buildSessionManagerCard(
    project: ProjectLeverageProject?,
    comparisonProject: ProjectLeverageProject?,
): ProjectCardUi? {
    if (project == null) return null

    val week = project.week ?: ProjectLeverageWeek()
    val telegramUnavailable = week.smTelegramIn == 0 && week.smTelegramOut == 0
    val footer = buildList {
        add("${formatCount(week.smActiveSessions)} active sessions")
        add("${formatCount(week.smReminds)} reminds")
        if (!telegramUnavailable && week.smTelegramOut > 0) {
            add("${formatCount(week.smTelegramOut)} Telegram out")
        }
    }.joinToString(" · ")

    return ProjectCardUi(
        title = "session-manager",
        summary = project.summary,
        color = projectColorFor("session-manager"),
        metrics = listOf(
            ProjectMetricUi(formatCount(week.smDispatches), "dispatches", Emerald),
            ProjectMetricUi(formatCount(week.smSends), "sends", Amber),
            ProjectMetricUi(if (telegramUnavailable) "--" else formatCount(week.smTelegramIn), "telegram", Blue),
        ),
        trend = buildProjectTrend(comparisonProject?.days ?: project.days, projectColorFor("session-manager")) { day ->
            day.smDispatches + day.smSends + day.smReminds + day.smActiveSessions + day.smTelegramIn + day.smTelegramOut
        },
        footer = footer,
    )
}

private fun buildEngramCard(
    project: ProjectLeverageProject?,
    comparisonProject: ProjectLeverageProject?,
): ProjectCardUi? {
    if (project == null) return null

    val current = project.current ?: EngramCurrent()
    val freshness = foldFreshness(current.lastFoldAgeHours)

    return ProjectCardUi(
        title = "engram",
        summary = project.summary,
        color = projectColorFor("engram"),
        metrics = listOf(
            ProjectMetricUi(current.lastFoldAgeHours.formatHoursOrDash(), "since fold", Emerald),
            ProjectMetricUi(formatCount(current.activeConcepts), "concepts", Amber),
            ProjectMetricUi(formatCount(current.folds7d), "folds / 7d", Cyan),
        ),
        trend = buildProjectTrend(comparisonProject?.days ?: project.days, projectColorFor("engram")) { day ->
            day.engramActiveConcepts + day.engramFolds7d
        },
        statusLabel = "Fold status:",
        statusValue = freshness.first,
        statusColor = freshness.second,
    )
}

private fun buildAgentOsCard(
    project: ProjectLeverageProject?,
    comparisonProject: ProjectLeverageProject?,
): ProjectCardUi? {
    if (project == null) return null

    val week = project.week ?: ProjectLeverageWeek()

    return ProjectCardUi(
        title = "agent-os",
        summary = project.summary,
        color = projectColorFor("agent-os"),
        metrics = listOf(
            ProjectMetricUi(formatCount(week.personaReads), "persona reads", Blue),
            ProjectMetricUi(formatCount(week.personaProjects), "projects", Cyan),
        ),
        trend = buildProjectTrend(comparisonProject?.days ?: project.days, projectColorFor("agent-os")) { day ->
            day.personaReads + day.personaProjects
        },
    )
}

private fun buildOfficeAutomationCard(
    project: ProjectLeverageProject?,
    comparisonProject: ProjectLeverageProject?,
): ProjectCardUi? {
    if (project == null) return null

    val week = project.week ?: ProjectLeverageWeek()

    return ProjectCardUi(
        title = "office-automate",
        summary = project.summary,
        color = projectColorFor("office-automate"),
        metrics = listOf(
            ProjectMetricUi(formatCount(week.automationEvents), "automations", Emerald),
            ProjectMetricUi(formatCount(week.stateTransitions), "transitions", Amber),
        ),
        trend = buildProjectTrend(comparisonProject?.days ?: project.days, projectColorFor("office-automate")) { day ->
            day.automationEvents + day.stateTransitions
        },
    )
}

private fun buildDeskbarTaskbarPlaceholderCard(): ProjectCardUi = ProjectCardUi(
    title = "deskbar / taskbar",
    summary = "Project card reserved. Telemetry is not wired into project leverage yet.",
    color = projectColorFor("taskbar"),
    metrics = listOf(
        ProjectMetricUi("--", "window switches", Cyan),
        ProjectMetricUi("--", "focus time", Amber),
        ProjectMetricUi("--", "uptime", Blue),
    ),
    footer = "Placeholder card stays visible until taskbar metrics land.",
)

private fun buildProjectTrend(
    days: List<ProjectLeverageDay>,
    accent: Color,
    activityForDay: (ProjectLeverageDay) -> Int,
): ProjectTrendUi? {
    if (days.isEmpty()) return null

    val recentDays = days.takeLast(7)
    if (recentDays.isEmpty()) return null

    val recentValues = recentDays.map { activityForDay(it).toFloat() }
    val today = activityForDay(recentDays.last())
    val weekAverage = recentDays.map(activityForDay).average()
    val recentTotal = recentDays.sumOf(activityForDay)
    val priorDays = days.dropLast(recentDays.size).takeLast(7)
    val priorTotal = priorDays.sumOf(activityForDay)
    val (direction, directionText) = buildWeekOverWeekSummary(
        recentTotal = recentTotal,
        priorTotal = priorTotal,
        hasPriorWeek = priorDays.isNotEmpty(),
    )

    return ProjectTrendUi(
        values = recentValues,
        accent = accent,
        direction = direction,
        directionText = directionText,
        context = "${formatCount(today)} today vs 7d avg ${formatDecimal(weekAverage)}",
    )
}

private fun buildWeekOverWeekSummary(
    recentTotal: Int,
    priorTotal: Int,
    hasPriorWeek: Boolean,
): Pair<TrendDirection, String> {
    if (!hasPriorWeek) {
        return TrendDirection.Flat to "Prior week unavailable"
    }
    if (recentTotal == 0 && priorTotal == 0) {
        return TrendDirection.Flat to "Flat vs prior week"
    }
    if (priorTotal == 0) {
        return TrendDirection.Up to "New vs prior week"
    }

    val deltaPercent = ((recentTotal - priorTotal).toDouble() / priorTotal.toDouble()) * 100.0
    if (abs(deltaPercent) < 1.0) {
        return TrendDirection.Flat to "Flat vs prior week"
    }

    val direction = if (deltaPercent > 0) TrendDirection.Up else TrendDirection.Down
    return direction to "${formatDecimal(abs(deltaPercent))}% vs prior week"
}

private fun foldFreshness(lastFoldAgeHours: Double?): Pair<String, Color> {
    if (lastFoldAgeHours == null) return "NO DATA" to TextSecondary
    return when {
        lastFoldAgeHours < 12.0 -> "FRESH" to Emerald
        lastFoldAgeHours <= 48.0 -> "STALE" to Amber
        else -> "OUTDATED" to Red
    }
}

private fun formatCount(value: Int): String = String.format(Locale.US, "%,d", value)

private fun formatDecimal(value: Double): String {
    val rounded = (value * 10.0).roundToInt() / 10.0
    return if (abs(rounded - rounded.roundToInt().toDouble()) < 0.05) {
        formatCount(rounded.roundToInt())
    } else {
        String.format(Locale.US, "%.1f", rounded)
    }
}

private fun Double?.formatHoursOrDash(): String = this?.let { String.format(Locale.US, "%.1fh", it) } ?: "--"
