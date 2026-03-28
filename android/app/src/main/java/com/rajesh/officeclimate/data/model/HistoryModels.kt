package com.rajesh.officeclimate.data.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

// --- Sessions ---

@Serializable
data class SessionsResponse(
    val ok: Boolean = false,
    val days: Int = 7,
    val sessions: List<SessionDay> = emptyList(),
    val summary: SessionSummary = SessionSummary(),
)

@Serializable
data class SessionDay(
    val date: String = "",
    val arrival: String = "",
    val departure: String = "",
    @SerialName("duration_hours") val durationHours: Double = 0.0,
    val gaps: List<SessionGap> = emptyList(),
)

@Serializable
data class SessionGap(
    val left: String = "",
    val returned: String = "",
    @SerialName("duration_min") val durationMin: Int = 0,
)

@Serializable
data class SessionSummary(
    @SerialName("avg_arrival") val avgArrival: String = "00:00:00",
    @SerialName("avg_departure") val avgDeparture: String = "00:00:00",
    @SerialName("avg_duration_hours") val avgDurationHours: Double = 0.0,
    @SerialName("std_arrival_min") val stdArrivalMin: Int = 0,
    @SerialName("std_departure_min") val stdDepartureMin: Int = 0,
    @SerialName("total_hours_week") val totalHoursWeek: Double = 0.0,
)

// --- CO2 OHLC ---

@Serializable
data class OHLCResponse(
    val ok: Boolean = false,
    val hours: Int = 24,
    @SerialName("bucket_minutes") val bucketMinutes: Int = 60,
    val candles: List<CO2Candle> = emptyList(),
)

@Serializable
data class CO2Candle(
    val timestamp: String = "",
    val open: Int = 0,
    val high: Int = 0,
    val low: Int = 0,
    val close: Int = 0,
    val avg: Int = 0,
    val readings: Int = 0,
)

// --- Daily Stats ---

@Serializable
data class DailyStatsResponse(
    val ok: Boolean = false,
    val days: Int = 7,
    val stats: List<DailyStat> = emptyList(),
)

// --- Temperature ---

@Serializable
data class TemperatureResponse(
    val ok: Boolean = false,
    val hours: Int = 24,
    @SerialName("bucket_minutes") val bucketMinutes: Int = 30,
    val points: List<TempPoint> = emptyList(),
)

@Serializable
data class TempPoint(
    val timestamp: String = "",
    @SerialName("avg_f") val avgF: Double = 0.0,
    @SerialName("min_f") val minF: Double = 0.0,
    @SerialName("max_f") val maxF: Double = 0.0,
    val readings: Int = 0,
)

@Serializable
data class DailyStat(
    val date: String = "",
    @SerialName("door_events") val doorEvents: Int = 0,
    @SerialName("erv_runtime_min") val ervRuntimeMin: Int = 0,
    @SerialName("hvac_runtime_min") val hvacRuntimeMin: Int = 0,
    @SerialName("presence_hours") val presenceHours: Double = 0.0,
)

// --- Openings ---

@Serializable
data class OpeningsResponse(
    val ok: Boolean = false,
    val days: List<OpeningDay> = emptyList(),
)

@Serializable
data class OpeningDay(
    val date: String = "",
    val door: List<OpeningPeriod> = emptyList(),
    val window: List<OpeningPeriod> = emptyList(),
)

@Serializable
data class OpeningPeriod(
    val open: String = "",
    val close: String = "",
)

// --- Productivity ---

@Serializable
data class OrchestrationResponse(
    val ok: Boolean = false,
    val days: List<OrchestrationDay> = emptyList(),
)

@Serializable
data class OrchestrationDay(
    val date: String = "",
    val messages: Int = 0,
    val sessions: Int = 0,
    @SerialName("first_prompt") val firstPrompt: String? = null,
    @SerialName("last_prompt") val lastPrompt: String? = null,
    @SerialName("by_tool") val byTool: Map<String, Int> = emptyMap(),
    val timestamps: List<PromptTimestamp> = emptyList(),
)

@Serializable
data class PromptTimestamp(
    val time: String = "",
    val tool: String = "",
)

@Serializable
data class ProjectFocusResponse(
    val ok: Boolean = false,
    val days: List<ProjectFocusDay> = emptyList(),
)

@Serializable
data class ProjectFocusDay(
    val date: String = "",
    val total: Int = 0,
    val projects: List<ProjectCount> = emptyList(),
)

@Serializable
data class ProjectCount(
    val name: String = "",
    val messages: Int = 0,
)

@Serializable
data class LeverageResponse(
    val ok: Boolean = false,
    val days: List<LeverageDay> = emptyList(),
    val week: LeverageWeek = LeverageWeek(),
)

@Serializable
data class LeverageDay(
    val date: String = "",
    val prompts: Int = 0,
    val sessions: Int = 0,
    @SerialName("lines_added") val linesAdded: Int = 0,
    @SerialName("lines_removed") val linesRemoved: Int = 0,
    @SerialName("lines_changed") val linesChanged: Int = 0,
    @SerialName("files_modified") val filesModified: Int = 0,
    val commits: Int = 0,
    @SerialName("prs_merged") val prsMerged: Int = 0,
    @SerialName("prs_opened") val prsOpened: Int = 0,
    @SerialName("avg_pr_cycle_hours") val avgPrCycleHours: Double? = null,
    @SerialName("lines_per_prompt") val linesPerPrompt: Double? = null,
    @SerialName("commits_per_prompt") val commitsPerPrompt: Double? = null,
    @SerialName("lines_per_session_minute") val linesPerSessionMinute: Double? = null,
)

@Serializable
data class LeverageWeek(
    val prompts: Int = 0,
    val sessions: Int = 0,
    @SerialName("lines_added") val linesAdded: Int = 0,
    @SerialName("lines_removed") val linesRemoved: Int = 0,
    @SerialName("lines_changed") val linesChanged: Int = 0,
    @SerialName("files_modified") val filesModified: Int = 0,
    val commits: Int = 0,
    @SerialName("prs_merged") val prsMerged: Int = 0,
    @SerialName("prs_opened") val prsOpened: Int = 0,
    @SerialName("avg_pr_cycle_hours") val avgPrCycleHours: Double? = null,
    @SerialName("lines_per_prompt") val linesPerPrompt: Double? = null,
    @SerialName("commits_per_prompt") val commitsPerPrompt: Double? = null,
    @SerialName("lines_per_session_minute") val linesPerSessionMinute: Double? = null,
    @SerialName("active_days") val activeDays: Int = 0,
)

@Serializable
data class ProjectLeverageResponse(
    val ok: Boolean = false,
    val projects: Map<String, ProjectLeverageProject> = emptyMap(),
)

@Serializable
data class ProjectLeverageProject(
    val summary: String = "",
    val days: List<ProjectLeverageDay> = emptyList(),
    val week: ProjectLeverageWeek? = null,
    val current: EngramCurrent? = null,
)

@Serializable
data class ProjectLeverageDay(
    val date: String = "",
    @SerialName("sm_dispatches") val smDispatches: Int = 0,
    @SerialName("sm_sends") val smSends: Int = 0,
    @SerialName("sm_reminds") val smReminds: Int = 0,
    @SerialName("sm_active_sessions") val smActiveSessions: Int = 0,
    @SerialName("sm_telegram_in") val smTelegramIn: Int = 0,
    @SerialName("sm_telegram_out") val smTelegramOut: Int = 0,
    @SerialName("engram_last_fold_age_hours") val engramLastFoldAgeHours: Double = 0.0,
    @SerialName("engram_folds_7d") val engramFolds7d: Int = 0,
    @SerialName("engram_active_concepts") val engramActiveConcepts: Int = 0,
    @SerialName("persona_reads") val personaReads: Int = 0,
    @SerialName("persona_projects") val personaProjects: Int = 0,
    @SerialName("automation_events") val automationEvents: Int = 0,
    @SerialName("state_transitions") val stateTransitions: Int = 0,
)

@Serializable
data class ProjectLeverageWeek(
    @SerialName("sm_dispatches") val smDispatches: Int = 0,
    @SerialName("sm_sends") val smSends: Int = 0,
    @SerialName("sm_reminds") val smReminds: Int = 0,
    @SerialName("sm_active_sessions") val smActiveSessions: Int = 0,
    @SerialName("sm_telegram_in") val smTelegramIn: Int = 0,
    @SerialName("sm_telegram_out") val smTelegramOut: Int = 0,
    @SerialName("persona_reads") val personaReads: Int = 0,
    @SerialName("persona_projects") val personaProjects: Int = 0,
    @SerialName("automation_events") val automationEvents: Int = 0,
    @SerialName("state_transitions") val stateTransitions: Int = 0,
)

@Serializable
data class EngramCurrent(
    @SerialName("last_fold_age_hours") val lastFoldAgeHours: Double? = null,
    @SerialName("active_concepts") val activeConcepts: Int = 0,
    @SerialName("folds_7d") val folds7d: Int = 0,
)
