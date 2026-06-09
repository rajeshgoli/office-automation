package com.rajesh.officeclimate.data.repository

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringSetPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import com.rajesh.officeclimate.util.Defaults
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.first
import java.io.ByteArrayInputStream
import java.net.URI
import java.security.KeyStore
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate

private val Context.dataStore: DataStore<Preferences> by preferencesDataStore(name = "settings")

class SettingsRepository(private val context: Context) {

    private object Keys {
        val SERVER_URL = stringPreferencesKey("server_url")
        val JWT_TOKEN = stringPreferencesKey("jwt_token")
        val USER_EMAIL = stringPreferencesKey("user_email")
        val DEVICE_CERTIFICATE_ALIAS = stringPreferencesKey("device_certificate_alias")
        val DEVICE_CERTIFICATE_CHAIN_PEM = stringPreferencesKey("device_certificate_chain_pem")
        val DEVICE_PRIVATE_KEY_PKCS8 = stringPreferencesKey("device_private_key_pkcs8")
        val DISMISSED_UPDATE_ARTIFACT_HASH = stringPreferencesKey("dismissed_update_artifact_hash")
        val DISMISSED_APP_NOTIFICATION_IDS = stringSetPreferencesKey("dismissed_app_notification_ids")
    }

    val serverUrl: Flow<String> = context.dataStore.data.map { prefs ->
        prefs[Keys.SERVER_URL] ?: Defaults.SERVER_URL
    }

    val deviceCertificateAlias: Flow<String> = context.dataStore.data.map { prefs ->
        prefs[Keys.DEVICE_CERTIFICATE_ALIAS] ?: ""
    }

    val deviceCertificateChainPem: Flow<String> = context.dataStore.data.map { prefs ->
        prefs[Keys.DEVICE_CERTIFICATE_CHAIN_PEM] ?: ""
    }

    val jwtToken: Flow<String> = context.dataStore.data.map { prefs ->
        prefs[Keys.JWT_TOKEN] ?: ""
    }

    val userEmail: Flow<String> = context.dataStore.data.map { prefs ->
        prefs[Keys.USER_EMAIL] ?: ""
    }

    val hasDeviceCredential: Flow<Boolean> = context.dataStore.data.map { prefs ->
        hasValidDeviceCredential(prefs)
    }

    val isAuthenticated: Flow<Boolean> = context.dataStore.data.map { prefs ->
        hasValidDeviceCredential(prefs)
    }

    val dismissedUpdateArtifactHash: Flow<String?> = context.dataStore.data.map { prefs ->
        prefs[Keys.DISMISSED_UPDATE_ARTIFACT_HASH]
    }

    val dismissedAppNotificationIds: Flow<Set<String>> = context.dataStore.data.map { prefs ->
        prefs[Keys.DISMISSED_APP_NOTIFICATION_IDS] ?: emptySet()
    }

    suspend fun saveServerUrl(serverUrl: String) {
        val normalized = serverUrl.trimEnd('/')
        require(isSecurePublicUrl(normalized)) {
            "Public server URL must use HTTPS. Use the local pairing URL only for device enrollment."
        }
        context.dataStore.edit { prefs ->
            prefs[Keys.SERVER_URL] = normalized
        }
    }

    suspend fun saveDeviceCertificateAlias(alias: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.DEVICE_CERTIFICATE_ALIAS] = alias.trim()
        }
    }

    suspend fun saveDeviceCertificateChainPem(chainPem: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.DEVICE_CERTIFICATE_CHAIN_PEM] = chainPem.trim()
        }
    }

    suspend fun clearDeviceCertificateAlias() {
        context.dataStore.edit { prefs ->
            prefs.remove(Keys.DEVICE_CERTIFICATE_ALIAS)
        }
    }

    suspend fun clearDeviceCertificateChainPem() {
        context.dataStore.edit { prefs ->
            prefs.remove(Keys.DEVICE_CERTIFICATE_CHAIN_PEM)
        }
    }

    suspend fun saveDevicePrivateKeyPkcs8(privateKeyPkcs8: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.DEVICE_PRIVATE_KEY_PKCS8] = privateKeyPkcs8.trim()
        }
    }

    suspend fun clearDevicePrivateKeyPkcs8() {
        context.dataStore.edit { prefs ->
            prefs.remove(Keys.DEVICE_PRIVATE_KEY_PKCS8)
        }
    }

    suspend fun devicePrivateKeyPkcs8(): String =
        context.dataStore.data.first()[Keys.DEVICE_PRIVATE_KEY_PKCS8]?.trim().orEmpty()

    suspend fun clearLegacyAuthAndInvalidDeviceCredentialIfNeeded() {
        context.dataStore.edit { prefs ->
            if (
                !prefs[Keys.DEVICE_CERTIFICATE_ALIAS].isNullOrBlank() &&
                    !hasValidDeviceCredential(prefs)
            ) {
                prefs.remove(Keys.DEVICE_CERTIFICATE_ALIAS)
                prefs.remove(Keys.DEVICE_CERTIFICATE_CHAIN_PEM)
                prefs.remove(Keys.DEVICE_PRIVATE_KEY_PKCS8)
                prefs.remove(Keys.JWT_TOKEN)
                prefs.remove(Keys.USER_EMAIL)
            }
        }
    }

    suspend fun saveAuth(token: String, email: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.JWT_TOKEN] = token.trim()
            prefs[Keys.USER_EMAIL] = email.trim()
        }
    }

    suspend fun clearAuth() {
        context.dataStore.edit { prefs ->
            prefs.remove(Keys.JWT_TOKEN)
            prefs.remove(Keys.USER_EMAIL)
        }
    }

    suspend fun saveDismissedUpdateArtifactHash(artifactHash: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.DISMISSED_UPDATE_ARTIFACT_HASH] = artifactHash
        }
    }

    suspend fun dismissAppNotification(id: String) {
        context.dataStore.edit { prefs ->
            val dismissed = prefs[Keys.DISMISSED_APP_NOTIFICATION_IDS].orEmpty().toMutableSet()
            dismissed.add(id)
            prefs[Keys.DISMISSED_APP_NOTIFICATION_IDS] = dismissed
        }
    }

    private fun isSecurePublicUrl(url: String): Boolean {
        val uri = runCatching { URI(url) }.getOrNull() ?: return false
        return uri.scheme.equals("https", ignoreCase = true)
    }

    private fun hasValidDeviceCredential(prefs: Preferences): Boolean {
        val alias = prefs[Keys.DEVICE_CERTIFICATE_ALIAS]?.trim().orEmpty()
        val certificateChain = prefs[Keys.DEVICE_CERTIFICATE_CHAIN_PEM]?.trim().orEmpty()
        val privateKeyPkcs8 = prefs[Keys.DEVICE_PRIVATE_KEY_PKCS8]?.trim().orEmpty()
        return alias.isNotBlank() &&
            certificateChain.isNotBlank() &&
            hasRsaDeviceCertificate(certificateChain) &&
            (deviceKeyExists(alias) || privateKeyPkcs8.isNotBlank())
    }

    private fun hasRsaDeviceCertificate(certificateChainPem: String): Boolean =
        runCatching {
            val certificates = CertificateFactory.getInstance("X.509")
                .generateCertificates(ByteArrayInputStream(certificateChainPem.toByteArray()))
                .filterIsInstance<X509Certificate>()
            certificates.firstOrNull()?.publicKey?.algorithm.equals("RSA", ignoreCase = true)
        }.getOrDefault(false)

    private fun deviceKeyExists(alias: String): Boolean =
        runCatching {
            val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
            keyStore.getKey(alias, null) != null
        }.getOrDefault(false)

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
    }
}
