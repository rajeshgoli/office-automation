package com.rajesh.officeclimate.ui.navigation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import com.rajesh.officeclimate.MainActivity
import com.rajesh.officeclimate.data.repository.SettingsRepository
import com.rajesh.officeclimate.ui.dashboard.DashboardScreen
import com.rajesh.officeclimate.ui.settings.SettingsScreen
import com.rajesh.officeclimate.ui.settings.SettingsViewModel
import kotlinx.coroutines.flow.StateFlow

object Routes {
    const val SETTINGS = "settings"
    const val DASHBOARD = "dashboard"
}

@Composable
fun AppNavigation(authResult: StateFlow<MainActivity.AuthResult?>) {
    val navController = rememberNavController()
    val context = LocalContext.current
    val settingsRepo = SettingsRepository(context)
    val isLoggedIn by settingsRepo.isLoggedIn.collectAsState(initial = null)

    val startDestination = when (isLoggedIn) {
        true -> Routes.DASHBOARD
        false -> Routes.SETTINGS
        null -> return // Still loading
    }

    NavHost(navController = navController, startDestination = startDestination) {
        composable(Routes.SETTINGS) {
            val settingsViewModel: SettingsViewModel = viewModel()
            val auth by authResult.collectAsState()

            // Handle OAuth callback
            LaunchedEffect(auth) {
                auth?.let { result ->
                    settingsViewModel.onAuthCallback(result.token, result.email) {
                        navController.navigate(Routes.DASHBOARD) {
                            popUpTo(Routes.SETTINGS) { inclusive = true }
                        }
                    }
                    (context as? MainActivity)?.clearAuthResult()
                }
            }

            SettingsScreen(
                onNavigateToDashboard = {
                    navController.navigate(Routes.DASHBOARD) {
                        popUpTo(Routes.SETTINGS) { inclusive = true }
                    }
                },
                viewModel = settingsViewModel,
            )
        }
        composable(Routes.DASHBOARD) {
            DashboardScreen(
                onNavigateToSettings = {
                    navController.navigate(Routes.SETTINGS)
                }
            )
        }
    }
}
