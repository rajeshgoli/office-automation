package com.rajesh.officeclimate.data.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class DeviceFlowStartResponse(
    @SerialName("device_code") val deviceCode: String,
    @SerialName("user_code") val userCode: String,
    @SerialName("verification_url") val verificationUrl: String,
    @SerialName("expires_in") val expiresIn: Int,
    val interval: Int = 5,
)

@Serializable
data class DeviceFlowPollRequest(
    @SerialName("device_code") val deviceCode: String,
)

@Serializable
data class DeviceFlowPollResponse(
    val status: String,
    val message: String? = null,
    val email: String? = null,
    @SerialName("access_token") val accessToken: String? = null,
    @SerialName("expires_in") val expiresIn: Int? = null,
)
