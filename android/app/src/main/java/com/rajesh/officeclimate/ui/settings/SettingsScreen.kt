package com.rajesh.officeclimate.ui.settings

import android.net.Uri
import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import com.rajesh.officeclimate.ui.theme.Emerald
import com.rajesh.officeclimate.ui.theme.Red
import com.rajesh.officeclimate.ui.theme.TextSecondary

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun SettingsScreen(
    onNavigateToDashboard: () -> Unit,
    viewModel: SettingsViewModel = viewModel(),
) {
    val state by viewModel.uiState.collectAsState()
    val context = LocalContext.current
    var showServerUrl by remember { mutableStateOf(false) }

    val textFieldColors = OutlinedTextFieldDefaults.colors(
        focusedBorderColor = Emerald,
        unfocusedBorderColor = MaterialTheme.colorScheme.outline,
        focusedLabelColor = Emerald,
        cursorColor = Emerald,
    )

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp)
            .verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            text = "Office Climate",
            style = MaterialTheme.typography.headlineMedium,
            color = MaterialTheme.colorScheme.onBackground,
            modifier = Modifier.combinedClickable(
                onClick = {},
                onLongClick = { showServerUrl = !showServerUrl },
            ),
        )

        Spacer(Modifier.height(8.dp))

        Text(
            text = if (state.isLoggedIn) "Signed in as ${state.userEmail}" else "Sign in to your climate server",
            style = MaterialTheme.typography.bodyMedium,
            color = if (state.isLoggedIn) Emerald else TextSecondary,
        )

        if (showServerUrl) {
            Spacer(Modifier.height(16.dp))

            OutlinedTextField(
                value = state.serverUrl,
                onValueChange = viewModel::updateServerUrl,
                label = { Text("Server URL") },
                placeholder = { Text("https://office.rajeshgo.li") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                modifier = Modifier.fillMaxWidth(),
                colors = textFieldColors,
            )
        }

        Spacer(Modifier.height(24.dp))

        if (state.isLoggedIn) {
            Button(
                onClick = onNavigateToDashboard,
                modifier = Modifier.fillMaxWidth(),
                colors = ButtonDefaults.buttonColors(containerColor = Emerald),
            ) {
                Text("Open Dashboard", color = MaterialTheme.colorScheme.background)
            }

            Spacer(Modifier.height(12.dp))

            OutlinedButton(
                onClick = viewModel::logout,
                modifier = Modifier.fillMaxWidth(),
                colors = ButtonDefaults.outlinedButtonColors(contentColor = Red),
            ) {
                Text("Sign Out")
            }
        } else {
            Button(
                onClick = {
                    viewModel.getOAuthUrl { authUrl ->
                        if (authUrl != null) {
                            val customTabsIntent = CustomTabsIntent.Builder().build()
                            customTabsIntent.launchUrl(context, Uri.parse(authUrl))
                        }
                    }
                },
                enabled = !state.loading && state.serverUrl.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
                colors = ButtonDefaults.buttonColors(containerColor = Emerald),
            ) {
                if (state.loading) {
                    CircularProgressIndicator(
                        modifier = Modifier.height(20.dp),
                        strokeWidth = 2.dp,
                        color = MaterialTheme.colorScheme.background,
                    )
                } else {
                    Text("Sign in with Google", color = MaterialTheme.colorScheme.background)
                }
            }
        }

        state.error?.let { error ->
            Spacer(Modifier.height(12.dp))
            Text(
                text = error,
                style = MaterialTheme.typography.bodySmall,
                color = Red,
            )
        }
    }
}
