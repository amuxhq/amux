package sh.amux.design

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily

object AmuxFonts {
    val Sans: FontFamily = FontFamily(Font(R.font.instrument_sans))
    val Mono: FontFamily = FontFamily(Font(R.font.geist_mono))
}

@Composable
fun AmuxTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    val colorScheme = if (darkTheme) darkColorScheme() else lightColorScheme()
    val base = Typography()
    val typography = base.copy(
        displayLarge = base.displayLarge.copy(fontFamily = AmuxFonts.Sans),
        headlineMedium = base.headlineMedium.copy(fontFamily = AmuxFonts.Sans),
        titleMedium = base.titleMedium.copy(fontFamily = AmuxFonts.Sans),
        bodyLarge = base.bodyLarge.copy(fontFamily = AmuxFonts.Sans),
        bodyMedium = base.bodyMedium.copy(fontFamily = AmuxFonts.Sans),
        labelLarge = base.labelLarge.copy(fontFamily = AmuxFonts.Sans),
    )
    MaterialTheme(
        colorScheme = colorScheme,
        typography = typography,
        content = content,
    )
}
