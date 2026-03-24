package com.rajesh.officeclimate.util

import kotlin.math.roundToInt

fun Double.celsiusToFahrenheit(): Int = (this * 9.0 / 5.0 + 32).roundToInt()

fun Int?.formatPpm(): String = this?.toString() ?: "--"

fun Double?.formatTemp(): String = this?.celsiusToFahrenheit()?.toString()?.plus("°F") ?: "--°F"

fun Double?.formatPercent(): String = this?.roundToInt()?.toString()?.plus("%") ?: "--%"

fun Double?.formatDecimal(digits: Int = 1): String =
    this?.let { "%.${digits}f".format(it) } ?: "--"

fun Int.secondsToMinutes(): String {
    val min = this / 60
    return if (min > 0) "${min}m" else "${this}s"
}
