package com.rajesh.officeclimate.data.repository

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import com.rajesh.officeclimate.util.Defaults
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

private val Context.dataStore: DataStore<Preferences> by preferencesDataStore(name = "settings")

class SettingsRepository(private val context: Context) {

    private object Keys {
        val SERVER_URL = stringPreferencesKey("server_url")
        val JWT_TOKEN = stringPreferencesKey("jwt_token")
        val USER_EMAIL = stringPreferencesKey("user_email")
    }

    val serverUrl: Flow<String> = context.dataStore.data.map { prefs ->
        prefs[Keys.SERVER_URL] ?: Defaults.SERVER_URL
    }

    val jwtToken: Flow<String> = context.dataStore.data.map { prefs ->
        prefs[Keys.JWT_TOKEN] ?: ""
    }

    val userEmail: Flow<String> = context.dataStore.data.map { prefs ->
        prefs[Keys.USER_EMAIL] ?: ""
    }

    val isLoggedIn: Flow<Boolean> = context.dataStore.data.map { prefs ->
        !prefs[Keys.JWT_TOKEN].isNullOrBlank()
    }

    suspend fun saveServerUrl(serverUrl: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.SERVER_URL] = serverUrl.trimEnd('/')
        }
    }

    suspend fun saveAuth(token: String, email: String) {
        context.dataStore.edit { prefs ->
            prefs[Keys.JWT_TOKEN] = token
            prefs[Keys.USER_EMAIL] = email
        }
    }

    suspend fun clearAuth() {
        context.dataStore.edit { prefs ->
            prefs.remove(Keys.JWT_TOKEN)
            prefs.remove(Keys.USER_EMAIL)
        }
    }
}
