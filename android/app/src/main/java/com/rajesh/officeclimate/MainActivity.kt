package com.rajesh.officeclimate

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import com.rajesh.officeclimate.ui.navigation.AppNavigation
import com.rajesh.officeclimate.ui.theme.OfficeClimateTheme
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

class MainActivity : ComponentActivity() {

    private val _authResult = MutableStateFlow<AuthResult?>(null)
    val authResult: StateFlow<AuthResult?> = _authResult

    data class AuthResult(val token: String, val email: String)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        handleAuthIntent(intent)
        setContent {
            OfficeClimateTheme {
                AppNavigation(authResult = authResult)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleAuthIntent(intent)
    }

    private fun handleAuthIntent(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme == "officeclimate" && uri.host == "auth") {
            val token = uri.getQueryParameter("token") ?: return
            val email = uri.getQueryParameter("email") ?: return
            _authResult.value = AuthResult(token, email)
        }
    }

    fun clearAuthResult() {
        _authResult.value = null
    }
}
