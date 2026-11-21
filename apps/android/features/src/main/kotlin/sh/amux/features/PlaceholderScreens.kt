package sh.amux.features

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DetailScaffold(
    title: String,
    subtitle: String? = null,
    screenTag: String,
    onBack: () -> Unit,
    content: @Composable () -> Unit,
) {
    val colors = AmuxTheme.colors
    val backLabel = stringResource(R.string.action_back)
    AmuxGround(modifier = Modifier.testTag(screenTag)) {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = colors.ink,
            topBar = {
                Column {
                    TopAppBar(
                        colors = TopAppBarDefaults.topAppBarColors(
                            containerColor = colors.ground,
                            titleContentColor = colors.ink,
                            navigationIconContentColor = colors.ink,
                        ),
                        navigationIcon = {
                            IconButton(
                                onClick = onBack,
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(TestTags.NAV_BACK),
                            ) {
                                Icon(
                                    imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                                    contentDescription = backLabel,
                                )
                            }
                        },
                        title = {
                            Column {
                                Text(
                                    text = title,
                                    style = AmuxTheme.typography.identifier,
                                    color = colors.ink,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                if (!subtitle.isNullOrEmpty()) {
                                    Text(
                                        text = subtitle,
                                        style = AmuxTheme.typography.caption,
                                        color = colors.inkMuted,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                    )
                                }
                            }
                        },
                    )
                    AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
                }
            },
        ) { innerPadding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .padding(horizontal = Space.margin, vertical = Space.l),
                verticalArrangement = Arrangement.spacedBy(Space.m),
            ) {
                content()
            }
        }
    }
}

@Composable
fun ConversationPlaceholder(
    agentName: String,
    hostName: String?,
    onBack: () -> Unit,
) {
    DetailScaffold(
        title = agentName,
        subtitle = hostName,
        screenTag = TestTags.SCREEN_CONVERSATION,
        onBack = onBack,
    ) {
        AmuxEmptyCard(
            title = agentName,
            detail = stringResource(R.string.placeholder_conversation_detail),
        )
    }
}

@Composable
fun ChangesPlaceholder(
    agentName: String,
    onBack: () -> Unit,
) {
    DetailScaffold(
        title = stringResource(R.string.placeholder_changes_title),
        subtitle = agentName,
        screenTag = TestTags.SCREEN_CHANGES,
        onBack = onBack,
    ) {
        AmuxEmptyCard(
            title = agentName,
            detail = stringResource(R.string.placeholder_changes_detail),
        )
    }
}

@Composable
fun HostPlaceholder(
    hostName: String,
    reachLabel: String?,
    onPairAgain: () -> Unit,
    onRevoke: () -> Unit,
    onBack: () -> Unit,
) {
    val colors = AmuxTheme.colors
    var confirmingRevoke by remember { mutableStateOf(false) }

    DetailScaffold(
        title = hostName,
        subtitle = reachLabel,
        screenTag = TestTags.SCREEN_HOST,
        onBack = onBack,
    ) {
        AmuxEmptyCard(
            title = hostName,
            detail = reachLabel ?: stringResource(R.string.placeholder_host_detail),
        )
        AmuxActionButton(
            label = stringResource(R.string.placeholder_host_pair_again),
            kind = ActionButtonKind.Outline,
            fillWidth = true,
            onClick = onPairAgain,
        )
        AmuxActionButton(
            label = stringResource(R.string.placeholder_host_revoke),
            kind = ActionButtonKind.Destructive,
            fillWidth = true,
            onClick = { confirmingRevoke = true },
            modifier = Modifier.testTag(TestTags.HOST_REVOKE_BUTTON),
        )
    }

    if (confirmingRevoke) {
        AlertDialog(
            onDismissRequest = { confirmingRevoke = false },
            containerColor = colors.ground,
            titleContentColor = colors.ink,
            textContentColor = colors.inkMuted,
            title = {
                Text(
                    text = stringResource(R.string.placeholder_host_revoke_title, hostName),
                    style = AmuxTheme.typography.identifier,
                )
            },
            text = {
                Text(
                    text = stringResource(R.string.placeholder_host_revoke_detail, hostName),
                    style = AmuxTheme.typography.detail,
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmingRevoke = false
                        onRevoke()
                    },
                    modifier = Modifier.testTag(TestTags.HOST_REVOKE_CONFIRM),
                ) {
                    Text(
                        text = stringResource(R.string.placeholder_host_revoke),
                        color = colors.accent,
                        style = AmuxTheme.typography.bodyEmphasis,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmingRevoke = false }) {
                    Text(
                        text = stringResource(R.string.action_cancel),
                        color = colors.inkMuted,
                        style = AmuxTheme.typography.bodyEmphasis,
                    )
                }
            },
        )
    }
}

@Composable
fun NewAgentPlaceholder(
    hostNames: List<String>,
    onBack: () -> Unit,
) {
    DetailScaffold(
        title = stringResource(R.string.placeholder_new_agent_title),
        subtitle = hostNames.joinToString(" · ").takeIf { it.isNotEmpty() },
        screenTag = TestTags.SCREEN_NEW_AGENT,
        onBack = onBack,
    ) {
        AmuxEmptyCard(
            title = stringResource(R.string.placeholder_new_agent_title),
            detail = stringResource(R.string.placeholder_new_agent_detail),
        )
    }
}

@Composable
fun SettingsPlaceholder(
    accountEmail: String?,
    deviceName: String,
    onBack: () -> Unit,
) {
    val colors = AmuxTheme.colors
    DetailScaffold(
        title = stringResource(R.string.placeholder_settings_title),
        screenTag = TestTags.SCREEN_SETTINGS,
        onBack = onBack,
    ) {
        AmuxPanel(modifier = Modifier.fillMaxWidth()) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(Space.l),
                verticalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Text(
                    text = stringResource(R.string.placeholder_settings_account),
                    style = AmuxTheme.typography.caption,
                    color = colors.inkMuted,
                )
                Text(
                    text = accountEmail ?: stringResource(R.string.placeholder_settings_signed_out),
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.ink,
                )
            }
        }
        AmuxPanel(modifier = Modifier.fillMaxWidth()) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(Space.l),
                verticalArrangement = Arrangement.spacedBy(Space.xs),
            ) {
                Text(
                    text = stringResource(R.string.placeholder_settings_this_phone),
                    style = AmuxTheme.typography.caption,
                    color = colors.inkMuted,
                )
                Text(
                    text = deviceName,
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.ink,
                )
            }
        }
    }
}

@Composable
fun PaywallPlaceholder(
    onBack: () -> Unit,
) {
    DetailScaffold(
        title = stringResource(R.string.placeholder_paywall_title),
        screenTag = TestTags.SCREEN_PAYWALL,
        onBack = onBack,
    ) {
        AmuxEmptyCard(
            title = stringResource(R.string.placeholder_paywall_title),
            detail = stringResource(R.string.placeholder_paywall_detail),
        )
    }
}
