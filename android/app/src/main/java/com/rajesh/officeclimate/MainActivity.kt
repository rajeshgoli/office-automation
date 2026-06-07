package com.rajesh.officeclimate

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import com.rajesh.officeclimate.ui.navigation.AppNavigation
import com.rajesh.officeclimate.ui.theme.OfficeClimateTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            OfficeClimateTheme {
                AppNavigation()
            }
        }
    }
}
