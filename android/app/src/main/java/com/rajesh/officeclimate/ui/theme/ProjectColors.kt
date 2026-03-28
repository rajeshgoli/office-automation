package com.rajesh.officeclimate.ui.theme

import androidx.compose.ui.graphics.Color

val KnownProjectColors = mapOf(
    "session-manager" to Emerald,
    "engram" to Color(0xFFA855F7),
    "agent-os" to Blue,
    "office-automate" to Cyan,
)

private val ProjectFallbackPalette = listOf(
    Emerald,
    Amber,
    Blue,
    Cyan,
    Orange,
    Color(0xFFA855F7),
)

fun projectColorFor(name: String): Color {
    KnownProjectColors[name]?.let { return it }
    val index = Math.floorMod(name.lowercase().hashCode(), ProjectFallbackPalette.size)
    return ProjectFallbackPalette[index]
}
