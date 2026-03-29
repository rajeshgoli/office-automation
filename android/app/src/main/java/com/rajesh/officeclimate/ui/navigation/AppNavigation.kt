package com.rajesh.officeclimate.ui.navigation

import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Apps
import androidx.compose.material.icons.filled.Dashboard
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Insights
import androidx.compose.material.icons.outlined.Apps
import androidx.compose.material.icons.outlined.Dashboard
import androidx.compose.material.icons.outlined.History
import androidx.compose.material.icons.outlined.Insights
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import com.rajesh.officeclimate.MainActivity
import com.rajesh.officeclimate.data.repository.SettingsRepository
import com.rajesh.officeclimate.ui.dashboard.DashboardScreen
import com.rajesh.officeclimate.ui.history.HistoryScreen
import com.rajesh.officeclimate.ui.productivity.ProductivityScreen
import com.rajesh.officeclimate.ui.projects.ProjectsScreen
import com.rajesh.officeclimate.ui.settings.SettingsScreen
import com.rajesh.officeclimate.ui.settings.SettingsViewModel
import com.rajesh.officeclimate.ui.theme.*
import kotlinx.coroutines.flow.StateFlow

object Routes {
    const val SETTINGS = "settings"
    const val DASHBOARD = "dashboard"
    const val HISTORY = "history"
    const val PRODUCTIVITY = "productivity"
    const val PROJECTS = "projects"
}

private data class NavItem(
    val route: String,
    val label: String,
    val selectedIcon: ImageVector,
    val unselectedIcon: ImageVector,
)

private val navItems = listOf(
    NavItem(Routes.DASHBOARD, "Climate", Icons.Filled.Dashboard, Icons.Outlined.Dashboard),
    NavItem(Routes.HISTORY, "History", Icons.Filled.History, Icons.Outlined.History),
    NavItem(Routes.PRODUCTIVITY, "Productivity", Icons.Filled.Insights, Icons.Outlined.Insights),
    NavItem(Routes.PROJECTS, "Projects", Icons.Filled.Apps, Icons.Outlined.Apps),
)

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

    val navBackStackEntry by navController.currentBackStackEntryAsState()
    val currentRoute = navBackStackEntry?.destination?.route
    val showBottomBar = currentRoute in listOf(
        Routes.DASHBOARD,
        Routes.HISTORY,
        Routes.PRODUCTIVITY,
        Routes.PROJECTS,
    )

    Scaffold(
        containerColor = Background,
        bottomBar = {
            if (showBottomBar) {
                NavigationBar(
                    containerColor = Color(0xFF0E0E10),
                    contentColor = Emerald,
                ) {
                    navItems.forEach { item ->
                        val selected = currentRoute == item.route
                        NavigationBarItem(
                            selected = selected,
                            onClick = {
                                if (!selected) {
                                    navController.navigate(item.route) {
                                        popUpTo(Routes.DASHBOARD) { saveState = true }
                                        launchSingleTop = true
                                        restoreState = true
                                    }
                                }
                            },
                            icon = {
                                Icon(
                                    imageVector = if (selected) item.selectedIcon else item.unselectedIcon,
                                    contentDescription = item.label,
                                )
                            },
                            label = {
                                Text(
                                    item.label.uppercase(),
                                    style = MaterialTheme.typography.labelSmall,
                                )
                            },
                            colors = NavigationBarItemDefaults.colors(
                                selectedIconColor = Emerald,
                                selectedTextColor = Emerald,
                                unselectedIconColor = TextSecondary,
                                unselectedTextColor = TextSecondary,
                                indicatorColor = Emerald.copy(alpha = 0.1f),
                            ),
                        )
                    }
                }
            }
        },
    ) { innerPadding ->
        NavHost(
            navController = navController,
            startDestination = startDestination,
            modifier = Modifier.padding(innerPadding),
        ) {
            composable(Routes.SETTINGS) {
                val settingsViewModel: SettingsViewModel = viewModel()
                val auth by authResult.collectAsState()

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
                        navController.navigate(Routes.SETTINGS) {
                            popUpTo(Routes.DASHBOARD) { inclusive = true }
                        }
                    },
                )
            }
            composable(Routes.HISTORY) {
                HistoryScreen()
            }
            composable(Routes.PRODUCTIVITY) {
                ProductivityScreen()
            }
            composable(Routes.PROJECTS) {
                ProjectsScreen()
            }
        }
    }
}
