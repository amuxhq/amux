package sh.amux.features

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Storage
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxTheme
import sh.amux.design.Space

/**
 * Full-screen replacement for the shell when the installation or its store
 * fails to open.
 */
@Composable
fun StoreFailureScreen(
    message: String,
    onRelaunch: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val typography = AmuxTheme.typography

    AmuxGround(
        modifier = modifier.testTag(TestTags.STORE_FAILURE_SCREEN),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .systemBarsPadding()
                .padding(horizontal = Space.margin, vertical = Space.xl),
        ) {
            Spacer(modifier = Modifier.height(48.dp))
            Icon(
                imageVector = Icons.Outlined.Storage,
                contentDescription = null,
                tint = colors.accent,
                modifier = Modifier.size(32.dp),
            )
            Spacer(modifier = Modifier.height(Space.l))
            Text(
                text = stringResource(R.string.store_failure_title),
                style = typography.screenTitle,
                color = colors.ink,
                modifier = Modifier.testTag(TestTags.STORE_FAILURE_TITLE),
            )
            Spacer(modifier = Modifier.height(Space.s))
            Text(
                text = stringResource(R.string.store_failure_cause),
                style = typography.body,
                color = colors.inkMuted,
            )
            Spacer(modifier = Modifier.height(Space.l))
            Text(
                text = message,
                style = typography.monoSmall,
                color = colors.inkFaint,
                modifier = Modifier.testTag(TestTags.STORE_FAILURE_CAUSE),
            )
            Spacer(modifier = Modifier.weight(1f))
            AmuxActionButton(
                label = stringResource(R.string.store_failure_relaunch),
                kind = ActionButtonKind.Primary,
                fillWidth = true,
                onClick = onRelaunch,
                modifier = Modifier.testTag(TestTags.STORE_FAILURE_RELAUNCH),
            )
        }
    }
}
