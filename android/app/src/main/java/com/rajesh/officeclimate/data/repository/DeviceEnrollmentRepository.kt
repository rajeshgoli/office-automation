package com.rajesh.officeclimate.data.repository

import com.rajesh.officeclimate.data.remote.HttpClientFactory
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.bouncycastle.asn1.x500.X500Name
import org.bouncycastle.openssl.jcajce.JcaPEMWriter
import org.bouncycastle.operator.jcajce.JcaContentSignerBuilder
import org.bouncycastle.pkcs.PKCS10CertificationRequest
import org.bouncycastle.pkcs.jcajce.JcaPKCS10CertificationRequestBuilder
import java.io.StringWriter
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.spec.ECGenParameterSpec
import java.util.Base64
import java.util.UUID

@Serializable
data class DeviceEnrollmentRequest(
    @SerialName("pairing_code") val pairingCode: String,
    @SerialName("csr_pem") val csrPem: String,
    @SerialName("public_key_pem") val publicKeyPem: String,
)

@Serializable
data class DeviceEnrollmentResponse(
    @SerialName("device_id") val deviceId: String,
    @SerialName("device_name") val deviceName: String,
    @SerialName("pairing_code") val pairingCode: String,
    @SerialName("certificate_pem") val certificatePem: String,
    @SerialName("certificate_chain_pem") val certificateChainPem: String,
    @SerialName("expires_at") val expiresAt: String,
)

data class DeviceEnrollmentResult(
    val alias: String,
    val deviceId: String,
    val deviceName: String,
    val pairingCode: String,
    val expiresAt: String,
)

class DeviceEnrollmentRepository(
    private val settingsRepository: SettingsRepository,
) {
    private val json = Json { ignoreUnknownKeys = true; coerceInputValues = true }
    private val httpClientFactory = HttpClientFactory(settingsRepository)

    suspend fun enrollDevice(
        pairingUrl: String,
        pairingCode: String,
        deviceName: String,
    ): DeviceEnrollmentResult = withContext(Dispatchers.IO) {
        val alias = "office-device-${UUID.randomUUID()}"
        val keyPair = generateKeyPair()
        try {
            val requestBody = DeviceEnrollmentRequest(
                pairingCode = pairingCode.trim(),
                csrPem = buildCsrPem(keyPair, deviceName.trim()),
                publicKeyPem = buildPublicKeyPem(keyPair),
            )
            val request = Request.Builder()
                .url("${pairingUrl.trimEnd('/')}/complete")
                .post(
                    json.encodeToString(DeviceEnrollmentRequest.serializer(), requestBody)
                        .toRequestBody("application/json".toMediaType()),
                )
                .build()

            val response = httpClientFactory.create(connectTimeoutSeconds = 10, readTimeoutSeconds = 20)
                .newCall(request)
                .execute()

            response.use { result ->
                val body = result.body?.string().orEmpty()
                if (!result.isSuccessful) {
                    val error = runCatching {
                        json.parseToJsonElement(body).jsonObject["error"]?.jsonPrimitive?.content
                    }.getOrNull()
                    throw IllegalStateException(error ?: body.ifBlank { "HTTP ${result.code}" })
                }

                val payload = json.decodeFromString(DeviceEnrollmentResponse.serializer(), body)
                settingsRepository.saveDeviceCertificateChainPem(payload.certificatePem)
                settingsRepository.saveDevicePrivateKeyPkcs8(encodePrivateKeyPkcs8(keyPair))
                settingsRepository.saveDeviceCertificateAlias(alias)
                return@withContext DeviceEnrollmentResult(
                    alias = alias,
                    deviceId = payload.deviceId,
                    deviceName = payload.deviceName,
                    pairingCode = payload.pairingCode,
                    expiresAt = payload.expiresAt,
                )
            }
        } catch (e: Exception) {
            clearDeviceCredential()
            throw e
        }
    }

    private fun generateKeyPair(): KeyPair {
        val keyPairGenerator = KeyPairGenerator.getInstance("EC")
        keyPairGenerator.initialize(ECGenParameterSpec("secp256r1"))
        return keyPairGenerator.generateKeyPair()
    }

    private fun buildCsrPem(keyPair: KeyPair, deviceName: String): String {
        val subject = X500Name("CN=${deviceName.ifBlank { "Office Automate Device" }}")
        val builder = JcaPKCS10CertificationRequestBuilder(subject, keyPair.public)
        val signer = JcaContentSignerBuilder("SHA256withECDSA").build(keyPair.private)
        val csr: PKCS10CertificationRequest = builder.build(signer)
        val writer = StringWriter()
        JcaPEMWriter(writer).use { pemWriter ->
            pemWriter.writeObject(csr)
        }
        return writer.toString()
    }

    private fun buildPublicKeyPem(keyPair: KeyPair): String {
        val writer = StringWriter()
        JcaPEMWriter(writer).use { pemWriter ->
            pemWriter.writeObject(keyPair.public)
        }
        return writer.toString()
    }

    private fun encodePrivateKeyPkcs8(keyPair: KeyPair): String =
        Base64.getEncoder().encodeToString(keyPair.private.encoded)

    private suspend fun clearDeviceCredential() {
        settingsRepository.clearDeviceCertificateAlias()
        settingsRepository.clearDeviceCertificateChainPem()
        settingsRepository.clearDevicePrivateKeyPkcs8()
    }
}
