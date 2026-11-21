package sh.amux.features

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import sh.amux.design.AmuxTheme
import sh.amux.design.Space

@Composable
internal fun DiffDeltaCounts(
    added: Int,
    removed: Int,
    modifier: Modifier = Modifier,
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
                style = AmuxTheme.typography.monoSmall,
                color = colors.added,
            )
        }
        if (removed > 0) {
            Text(
                text = stringResource(R.string.review_diff_removed, removed),
                style = AmuxTheme.typography.monoSmall,
                color = colors.removed,
            )
        }
    }
}
