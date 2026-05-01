package com.rajesh.officeclimate.data.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class ApiStatus(
    val state: String = "UNKNOWN",
    @SerialName("is_present") val isPresent: Boolean = false,
    @SerialName("safety_interlock") val safetyInterlock: Boolean = false,
    @SerialName("erv_should_run") val ervShouldRun: Boolean = false,
    val sensors: Sensors = Sensors(),
    @SerialName("air_quality") val airQuality: AirQuality = AirQuality(),
    val erv: ErvStatus = ErvStatus(),
    val hvac: HvacStatus = HvacStatus(),
    @SerialName("manual_override") val manualOverride: ManualOverride? = null,
    val notifications: List<AppNotification> = emptyList(),
)

@Serializable
data class Sensors(
    @SerialName("mac_active") val macActive: Boolean = false,
    @SerialName("external_monitor") val externalMonitor: Boolean = false,
    @SerialName("motion_detected") val motionDetected: Boolean = false,
    @SerialName("door_open") val doorOpen: Boolean = false,
    @SerialName("window_open") val windowOpen: Boolean = false,
    @SerialName("co2_ppm") val co2Ppm: Int? = null,
)

@Serializable
data class AirQuality(
    @SerialName("co2_ppm") val co2Ppm: Int? = null,
    @SerialName("temp_c") val tempC: Double? = null,
    val humidity: Double? = null,
    val pm25: Double? = null,
    val pm10: Double? = null,
    val tvoc: Int? = null,
    @SerialName("noise_db") val noiseDb: Double? = null,
    @SerialName("last_update") val lastUpdate: String? = null,
    @SerialName("report_interval") val reportInterval: Int? = null,
)

@Serializable
data class ErvStatus(
    val running: Boolean = false,
    val speed: String? = null,
)

@Serializable
data class AppNotification(
    val id: String,
    val type: String,
    val severity: String = "info",
    val title: String,
    val message: String,
    @SerialName("created_at") val createdAt: String? = null,
    val active: Boolean = false,
    @SerialName("runbook_path") val runbookPath: String? = null,
)

@Serializable
data class HvacStatus(
    val mode: String = "off",
    @SerialName("setpoint_c") val setpointC: Double = 0.0,
    val suspended: Boolean = false,
)

@Serializable
data class ManualOverride(
    val erv: Boolean = false,
    @SerialName("erv_speed") val ervSpeed: String? = null,
    @SerialName("erv_expires_in") val ervExpiresIn: Int? = null,
    val hvac: Boolean = false,
    @SerialName("hvac_mode") val hvacMode: String? = null,
    @SerialName("hvac_setpoint_f") val hvacSetpointF: Int? = null,
    @SerialName("hvac_expires_in") val hvacExpiresIn: Int? = null,
)
