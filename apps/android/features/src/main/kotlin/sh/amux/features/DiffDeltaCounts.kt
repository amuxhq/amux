package sh.amux.features

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.TextStyle
import sh.amux.design.AmuxTheme
import sh.amux.design.Space

@Composable
internal fun DiffDeltaCounts(
    added: Int,
    removed: Int,
    modifier: Modifier = Modifier,
    style: TextStyle = AmuxTheme.typography.monoSmall,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier,
        horizontalArrangement = Arrangement.spacedBy(Space.xs),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (added > 0) {
            Text(
                text = stringResource(R.string.review_diff_added, added),
                style = style,
                color = colors.added,
            )
        }
        if (removed > 0) {
            Text(
                text = stringResource(R.string.review_diff_removed, removed),
                style = style,
                color = colors.removed,
            )
        }
    }
}
