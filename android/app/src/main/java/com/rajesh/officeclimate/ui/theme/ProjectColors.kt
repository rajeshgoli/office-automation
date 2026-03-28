package com.rajesh.officeclimate.ui.theme

import androidx.compose.ui.graphics.Color

val KnownProjectColors = mapOf(
    "session-manager" to Emerald,
    "agent-os" to Amber,
    "office-automate" to Blue,
    "taskbar" to Cyan,
    "deskbar" to Orange,
    "engram" to Color(0xFFF43F5E),
    "fractal" to Color(0xFF8B5CF6),
)

private val ProjectFallbackPalette = listOf(
    Emerald,
    Amber,
    Blue,
    Cyan,
    Orange,
    Color(0xFFF43F5E),
    Color(0xFF8B5CF6),
)

fun projectColorFor(name: String): Color {
    KnownProjectColors[name]?.let { return it }
    val index = Math.floorMod(name.lowercase().hashCode(), ProjectFallbackPalette.size)
    return ProjectFallbackPalette[index]
}
