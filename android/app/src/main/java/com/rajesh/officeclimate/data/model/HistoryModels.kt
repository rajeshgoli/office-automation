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
