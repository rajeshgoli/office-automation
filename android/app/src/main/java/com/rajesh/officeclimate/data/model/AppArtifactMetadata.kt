package com.rajesh.officeclimate.data.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class AppArtifactMetadata(
    @SerialName("artifact_hash") val artifactHash: String? = null,
    @SerialName("uploaded_at") val uploadedAt: String? = null,
    @SerialName("size_bytes") val sizeBytes: Long? = null,
    @SerialName("uploaded_by") val uploadedBy: String? = null,
    @SerialName("version_code") val versionCode: Long? = null,
    @SerialName("version_name") val versionName: String? = null,
)
