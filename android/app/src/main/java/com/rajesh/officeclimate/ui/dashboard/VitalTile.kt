package com.rajesh.officeclimate.ui.dashboard

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.rajesh.officeclimate.ui.theme.Border
import com.rajesh.officeclimate.ui.theme.Surface
import com.rajesh.officeclimate.ui.theme.TextPrimary
import com.rajesh.officeclimate.ui.theme.TextSecondary

@Composable
fun VitalTile(
    label: String,
    value: String,
    unit: String = "",
    accentColor: Color = TextPrimary,
    modifier: Modifier = Modifier,
) {
    val shape = RoundedCornerShape(12.dp)

    Column(
        modifier = modifier
            .clip(shape)
            .background(Surface.copy(alpha = 0.5f))
            .border(1.dp, Border, shape)
            .padding(12.dp),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = TextSecondary,
        )
        Text(
            text = value,
            fontSize = 24.sp,
            fontWeight = FontWeight.SemiBold,
            color = accentColor,
            modifier = Modifier.padding(top = 4.dp),
        )
        if (unit.isNotEmpty()) {
            Text(
                text = unit,
                style = MaterialTheme.typography.bodySmall,
                color = TextSecondary,
            )
        }
    }
}
