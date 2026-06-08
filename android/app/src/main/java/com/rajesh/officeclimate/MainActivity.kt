package com.rajesh.officeclimate

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.mutableIntStateOf
import androidx.lifecycle.lifecycleScope
import com.rajesh.officeclimate.data.repository.SettingsRepository
import com.rajesh.officeclimate.ui.navigation.AppNavigation
import com.rajesh.officeclimate.ui.theme.OfficeClimateTheme
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {
    private val authCallbackCount = mutableIntStateOf(0)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        handleAuthIntent(intent)
        enableEdgeToEdge()
        setContent {
            OfficeClimateTheme {
                AppNavigation(authCallbackCount.intValue)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleAuthIntent(intent)
    }

    private fun handleAuthIntent(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme != "officeclimate" || uri.host != "auth") return
        val token = uri.getQueryParameter("token")?.trim().orEmpty()
        val email = uri.getQueryParameter("email")?.trim().orEmpty()
        if (token.isBlank() || email.isBlank()) return
        lifecycleScope.launch {
            SettingsRepository(applicationContext).saveAuth(token, email)
            authCallbackCount.intValue += 1
        }
    }
}
