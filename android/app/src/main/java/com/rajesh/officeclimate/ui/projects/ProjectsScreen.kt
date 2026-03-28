package com.rajesh.officeclimate.ui.projects

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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.rajesh.officeclimate.data.model.EngramCurrent
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

private data class ProjectMetricUi(
    val value: String,
    val label: String,
    val accent: Color,
)

private data class ProjectCardUi(
    val title: String,
    val summary: String,
    val color: Color,
    val metrics: List<ProjectMetricUi>,
    val footer: String? = null,
    val statusLabel: String? = null,
    val statusValue: String? = null,
    val statusColor: Color? = null,
    val activity: Int,
)

@Composable
fun ProjectsScreen(
    viewModel: ProjectsViewModel = viewModel(),
) {
    val projectLeverage by viewModel.projectLeverage.collectAsState()
    val isLoading by viewModel.isLoading.collectAsState()
    val error by viewModel.error.collectAsState()

    val cards = projectLeverage?.let(::buildProjectCards).orEmpty()

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

private fun buildProjectCards(response: ProjectLeverageResponse): List<ProjectCardUi> {
    val projects = response.projects

    return listOfNotNull(
        buildAgentOsCard(projects["agent-os"]),
        buildSessionManagerCard(projects["session-manager"]),
        buildOfficeAutomationCard(projects["office-automate"]),
        buildDeskbarTaskbarPlaceholderCard(),
        buildEngramCard(projects["engram"]),
    )
}

private fun buildSessionManagerCard(project: ProjectLeverageProject?): ProjectCardUi? {
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

    val activity = week.smDispatches + week.smSends + week.smReminds + week.smActiveSessions + week.smTelegramIn + week.smTelegramOut

    return ProjectCardUi(
        title = "session-manager",
        summary = project.summary,
        color = projectColorFor("session-manager"),
        metrics = listOf(
            ProjectMetricUi(formatCount(week.smDispatches), "dispatches", Emerald),
            ProjectMetricUi(formatCount(week.smSends), "sends", Amber),
            ProjectMetricUi(if (telegramUnavailable) "--" else formatCount(week.smTelegramIn), "telegram", Blue),
        ),
        footer = footer,
        activity = activity,
    )
}

private fun buildEngramCard(project: ProjectLeverageProject?): ProjectCardUi? {
    if (project == null) return null

    val current = project.current ?: EngramCurrent()
    val freshness = foldFreshness(current.lastFoldAgeHours)
    val activity = current.activeConcepts + current.folds7d

    return ProjectCardUi(
        title = "engram",
        summary = project.summary,
        color = projectColorFor("engram"),
        metrics = listOf(
            ProjectMetricUi(current.lastFoldAgeHours.formatHoursOrDash(), "since fold", Emerald),
            ProjectMetricUi(formatCount(current.activeConcepts), "concepts", Amber),
            ProjectMetricUi(formatCount(current.folds7d), "folds / 7d", Cyan),
        ),
        statusLabel = "Fold status:",
        statusValue = freshness.first,
        statusColor = freshness.second,
        activity = activity,
    )
}

private fun buildAgentOsCard(project: ProjectLeverageProject?): ProjectCardUi? {
    if (project == null) return null

    val week = project.week ?: ProjectLeverageWeek()
    val activity = week.personaReads + week.personaProjects

    return ProjectCardUi(
        title = "agent-os",
        summary = project.summary,
        color = projectColorFor("agent-os"),
        metrics = listOf(
            ProjectMetricUi(formatCount(week.personaReads), "persona reads", Blue),
            ProjectMetricUi(formatCount(week.personaProjects), "projects", Cyan),
        ),
        activity = activity,
    )
}

private fun buildOfficeAutomationCard(project: ProjectLeverageProject?): ProjectCardUi? {
    if (project == null) return null

    val week = project.week ?: ProjectLeverageWeek()
    val activity = week.automationEvents + week.stateTransitions

    return ProjectCardUi(
        title = "office-automate",
        summary = project.summary,
        color = projectColorFor("office-automate"),
        metrics = listOf(
            ProjectMetricUi(formatCount(week.automationEvents), "automations", Emerald),
            ProjectMetricUi(formatCount(week.stateTransitions), "transitions", Amber),
        ),
        activity = activity,
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
    activity = 0,
)

private fun foldFreshness(lastFoldAgeHours: Double?): Pair<String, Color> {
    if (lastFoldAgeHours == null) return "NO DATA" to TextSecondary
    return when {
        lastFoldAgeHours < 12.0 -> "FRESH" to Emerald
        lastFoldAgeHours <= 48.0 -> "STALE" to Amber
        else -> "OUTDATED" to Red
    }
}

private fun formatCount(value: Int): String = String.format(Locale.US, "%,d", value)

private fun Double?.formatHoursOrDash(): String = this?.let { String.format(Locale.US, "%.1fh", it) } ?: "--"
