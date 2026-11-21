package sh.amux.features

import sh.amux.design.Motion
import androidx.compose.animation.animateColorAsState
import sh.amux.design.ThumbTarget
import androidx.compose.foundation.layout.heightIn
import sh.amux.design.IconSize
import sh.amux.design.AmuxShapes
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.ListAlt
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.NorthWest
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.RateReview
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxChrome
import sh.amux.design.spillTarget
import androidx.compose.foundation.indication
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.material3.ripple
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AgentKey
import sh.amux.values.Attention
import sh.amux.values.ChangeTotals
import sh.amux.values.ChatFrame
import sh.amux.values.FamilyHeader
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.Offline
import sh.amux.values.PhaseView
import sh.amux.values.Reach
import sh.amux.values.Row
import sh.amux.words.ChatWords
import sh.amux.words.PlaceNames
import sh.amux.words.RowState

@Composable
internal fun chatPlaceLine(
    subject: ChatSubject,
    frame: ChatFrame?,
    detached: Boolean,
): String {
    if (detached && subject.reachable) {
        return stringResource(R.string.chat_place_out_of_reach, subject.host)
    }
    val exited = frame?.phase as? PhaseView.Exited
    if (exited != null) {
        return ChatWords.exited(exited.cause, settled = subject.filing == Filing.Closed)
    }
    if (subject.connecting) {
        return PlaceNames.place(host = subject.host.takeIf { it.isNotEmpty() }, directory = subject.directory)
    }
    return when (val reach = subject.reach) {
        is Reach.Online -> PlaceNames.place(
            host = subject.host.takeIf { it.isNotEmpty() },
            directory = subject.directory,
        )
        Reach.Away -> stringResource(R.string.chat_place_away_free, subject.host)
        is Reach.Offline -> when (reach.value) {
            Offline.Revoked -> stringResource(R.string.chat_place_unpaired, subject.host)
            Offline.SignedOut -> stringResource(R.string.chat_place_signed_out, subject.host)
            Offline.Plain -> stringResource(R.string.chat_place_away, subject.host)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ChatTopBar(
    scrolled: Boolean,
    subject: ChatSubject,
    placeLine: String,
    unconfirmed: Boolean,
    changes: ChangeTotals?,
    hasOverviewHighlights: Boolean,
    settingsAvailable: Boolean,
    onBack: () -> Unit,
    onOpenOverview: () -> Unit,
    onOpenSettings: () -> Unit,
    onOpenReview: () -> Unit,
    onPin: () -> Unit,
    onUnpin: () -> Unit,
    onOpenRename: () -> Unit,
    onCopyAddress: () -> Unit,
    onSettle: () -> Unit,
    onUnsettle: () -> Unit,
    onCancelSettle: () -> Unit,
    onOpenDelete: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val backLabel = stringResource(R.string.action_back)
    val moreLabel = stringResource(R.string.action_more_options)
    val openOverviewDesc = stringResource(R.string.chat_header_open_overview)
    val titleSpoken = stringResource(R.string.chat_title_spoken, subject.name, placeLine, openOverviewDesc)
    var menuExpanded by remember { mutableStateOf(false) }
    val fill by animateColorAsState(
        targetValue = if (scrolled) colors.barScrolled else colors.ground,
        animationSpec = Motion.standardAnimationSpec(),
        label = "chatBarFill",
    )

    Column(modifier = Modifier.testTag(ChatScreenTags.TOP_BAR)) {
        TopAppBar(
            colors = TopAppBarDefaults.topAppBarColors(
                containerColor = fill,
                titleContentColor = colors.ink,
                navigationIconContentColor = colors.ink,
                actionIconContentColor = colors.ink,
            ),
            navigationIcon = {
                IconButton(
                    onClick = onBack,
                    modifier = Modifier.testTag(TestTags.NAV_BACK),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                        contentDescription = backLabel,
                    )
                }
            },
            title = {
                // The bar's height is fixed by Material, so its title and
                // capsules stop growing past the size iOS caps the same
                // header at, rather than clipping. Its menu is not capped.
                CappedFontScale {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = ThumbTarget.MinSize)
                            .clip(RoundedCornerShape(Space.s))
                            .clickable(role = Role.Button, onClick = onOpenOverview)
                            .semantics {
                                heading()
                                contentDescription = titleSpoken
                            }
                            .padding(vertical = Space.xs)
                            .testTag(ChatScreenTags.TITLE),
                        verticalArrangement = Arrangement.spacedBy(1.dp),
                    ) {
                        Text(
                            text = subject.name,
                            style = AmuxTheme.typography.identifier,
                            color = colors.ink,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        if (placeLine.isNotEmpty()) {
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(Space.xs),
                                modifier = Modifier.testTag(ChatScreenTags.HEADER),
                            ) {
                                if (unconfirmed) {
                                    AmuxStateMark(
                                        state = RowState.HostAway(subject.host),
                                        size = 11.dp,
                                    )
                                }
                                Text(
                                    text = placeLine,
                                    style = AmuxTheme.typography.caption,
                                    color = colors.inkMuted,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                    }
                }
            },
            actions = {
                if (changes != null) {
                    val spokenChanges = ChatWords.changes(
                        files = changes.files,
                        added = changes.added,
                        removed = changes.removed,
                    )
                    Box(
                        modifier = Modifier
                            .thumbTarget()
                            .clip(AmuxShapes.Pill)
                            .clickable(role = Role.Button, onClick = onOpenReview)
                            .semantics { contentDescription = spokenChanges }
                            .testTag(ChatScreenTags.CHANGES),
                        contentAlignment = Alignment.Center,
                    ) {
                        CappedFontScale {
                            Surface(
                                shape = AmuxShapes.Pill,
                                color = colors.sunken,
                                border = BorderStroke(AmuxTheme.metrics.hairline, colors.hairline),
                            ) {
                                Box(
                                    modifier = Modifier.padding(horizontal = Space.m, vertical = Space.xs + Space.xxs),
                                    contentAlignment = Alignment.Center,
                                ) {
                                    DiffDeltaCounts(added = changes.added, removed = changes.removed)
                                }
                            }
                        }
                    }
                } else if (hasOverviewHighlights) {
                    IconButton(
                        onClick = onOpenOverview,
                        modifier = Modifier.testTag(ChatScreenTags.OVERVIEW_ACTION),
                    ) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Outlined.ListAlt,
                            contentDescription = openOverviewDesc,
                            tint = colors.inkMuted,
                            modifier = Modifier.size(IconSize.Row),
                        )
                    }
                }

                Box {
                    IconButton(
                        onClick = { menuExpanded = true },
                        modifier = Modifier.testTag(ChatScreenTags.MORE),
                    ) {
                        Icon(
                            imageVector = Icons.Outlined.MoreVert,
                            contentDescription = moreLabel,
                        )
                    }

                    val closed = subject.filing == Filing.Closed
                    DropdownMenu(
                        expanded = menuExpanded,
                        onDismissRequest = { menuExpanded = false },
                        containerColor = colors.raised,
                    ) {
                        DropdownMenuItem(
                            text = { Text(text = stringResource(R.string.chat_menu_overview), color = colors.ink) },
                            leadingIcon = {
                                Icon(
                                    imageVector = Icons.AutoMirrored.Outlined.ListAlt,
                                    contentDescription = null,
                                    tint = colors.inkMuted,
                                )
                            },
                            onClick = {
                                menuExpanded = false
                                onOpenOverview()
                            },
                            modifier = Modifier.testTag(ChatScreenTags.MENU_OVERVIEW),
                        )
                        if (settingsAvailable) {
                            DropdownMenuItem(
                                text = { Text(text = stringResource(R.string.chat_menu_settings), color = colors.ink) },
                                leadingIcon = {
                                    Icon(
                                        imageVector = Icons.Outlined.Tune,
                                        contentDescription = null,
                                        tint = colors.inkMuted,
                                    )
                                },
                                onClick = {
                                    menuExpanded = false
                                    onOpenSettings()
                                },
                                modifier = Modifier.testTag(ChatScreenTags.MENU_SETTINGS),
                            )
                        }
                        if (changes != null) {
                            DropdownMenuItem(
                                text = { Text(text = stringResource(R.string.chat_menu_review), color = colors.ink) },
                                leadingIcon = {
                                    Icon(
                                        imageVector = Icons.Outlined.RateReview,
                                        contentDescription = null,
                                        tint = colors.inkMuted,
                                    )
                                },
                                onClick = {
                                    menuExpanded = false
                                    onOpenReview()
                                },
                                modifier = Modifier.testTag(ChatScreenTags.MENU_REVIEW),
                            )
                        }
                        if (subject.topLevel && !closed) {
                            if (subject.filing == Filing.Pinned) {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_unpin), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.Outlined.PushPin,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onUnpin()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_UNPIN),
                                )
                            } else {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_pin), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.Outlined.PushPin,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onPin()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_PIN),
                                )
                            }
                        }
                        DropdownMenuItem(
                            text = { Text(text = stringResource(R.string.chat_menu_rename), color = colors.ink) },
                            leadingIcon = {
                                Icon(
                                    imageVector = Icons.Outlined.Edit,
                                    contentDescription = null,
                                    tint = colors.inkMuted,
                                )
                            },
                            onClick = {
                                menuExpanded = false
                                onOpenRename()
                            },
                            modifier = Modifier.testTag(ChatScreenTags.MENU_RENAME),
                        )
                        if (!subject.native) {
                            DropdownMenuItem(
                                text = { Text(text = stringResource(R.string.chat_menu_copy_address), color = colors.ink) },
                                leadingIcon = {
                                    Icon(
                                        imageVector = Icons.Outlined.ContentCopy,
                                        contentDescription = null,
                                        tint = colors.inkMuted,
                                    )
                                },
                                onClick = {
                                    menuExpanded = false
                                    onCopyAddress()
                                },
                                modifier = Modifier.testTag(ChatScreenTags.MENU_COPY_ADDRESS),
                            )
                        }
                        when {
                            subject.closeHeld -> {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_cancel_stop), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.AutoMirrored.Outlined.Undo,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onCancelSettle()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_CANCEL_STOP),
                                )
                            }
                            closed -> {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_unsettle), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.Outlined.Unarchive,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onUnsettle()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_UNSETTLE),
                                )
                                if (!subject.native) {
                                    DropdownMenuItem(
                                        text = { Text(text = stringResource(R.string.chat_menu_delete), color = colors.removed) },
                                        leadingIcon = {
                                            Icon(
                                                imageVector = Icons.Outlined.DeleteOutline,
                                                contentDescription = null,
                                                tint = colors.removed,
                                            )
                                        },
                                        onClick = {
                                            menuExpanded = false
                                            onOpenDelete()
                                        },
                                        modifier = Modifier.testTag(ChatScreenTags.MENU_DELETE),
                                    )
                                }
                            }
                            else -> {
                                DropdownMenuItem(
                                    text = { Text(text = stringResource(R.string.chat_menu_settle), color = colors.ink) },
                                    leadingIcon = {
                                        Icon(
                                            imageVector = Icons.Outlined.CheckCircle,
                                            contentDescription = null,
                                            tint = colors.inkMuted,
                                        )
                                    },
                                    onClick = {
                                        menuExpanded = false
                                        onSettle()
                                    },
                                    modifier = Modifier.testTag(ChatScreenTags.MENU_SETTLE),
                                )
                            }
                        }
                    }
                }
            },
        )
    }
}

/**
 * Who started this agent, one tap from its own chat, with a mark when it
 * needs the person. The agents this one started dock above the composer.
 */
@Composable
internal fun ChatFamilyBar(
    family: FamilyHeader,
    onOpenAgent: (AgentKey) -> Unit,
) {
    val parent = family.parent ?: return
    val colors = AmuxTheme.colors
    val parentNeedsYou = parent.familyAttention == Attention.NeedsYou ||
        parent.attention == Attention.NeedsYou
    val interaction = remember { MutableInteractionSource() }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            // A little room above, so the pill's target, which spills past
            // its drawn edge, stops short of the top bar's.
            .padding(start = Space.margin, end = Space.margin, top = Space.xxs, bottom = Space.s),
    ) {
        Box(
            modifier = Modifier
                .spillTarget()
                .clickable(
                    interactionSource = interaction,
                    indication = null,
                    role = Role.Button,
                ) { onOpenAgent(parent.agent) }
                .testTag(ChatScreenTags.FAMILY_PARENT),
            contentAlignment = Alignment.Center,
        ) {
            Row(
                modifier = Modifier
                    .clip(AmuxShapes.Pill)
                    .background(colors.raised)
                    .indication(interaction, ripple())
                    .padding(horizontal = Space.s + Space.xxs, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                if (parentNeedsYou) {
                    AmuxAttentionDot(size = 7.dp)
                } else {
                    Icon(
                        imageVector = Icons.Outlined.NorthWest,
                        contentDescription = null,
                        tint = colors.inkFaint,
                        modifier = Modifier.size(12.dp),
                    )
                }
                Text(
                    text = stringResource(R.string.chat_family_from, parent.name),
                    style = AmuxTheme.typography.caption,
                    color = colors.ink,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/** A settle that has not gone yet, with the way to take it back. */
@Composable
internal fun HeldStopBanner(
    host: String,
    onCancelStop: () -> Unit,
) {
    val colors = AmuxTheme.colors
    AmuxChrome(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(ChatScreenTags.HELD_STOP),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.m + Space.xxs, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s + Space.xxs),
        ) {
            Icon(
                imageVector = Icons.Outlined.Schedule,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(IconSize.Inline),
            )
            Text(
                text = stringResource(R.string.chat_settle_held, host),
                style = AmuxTheme.typography.detail,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
            AmuxActionButton(
                label = stringResource(R.string.chat_settle_held_cancel),
                kind = ActionButtonKind.Outline,
                onClick = onCancelStop,
                modifier = Modifier.testTag(ChatScreenTags.HELD_STOP_CANCEL),
            )
        }
    }
}

@Composable
internal fun EndedChatBanner(message: String) {
    val colors = AmuxTheme.colors
    AmuxPanel(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Space.margin, vertical = Space.xs)
            .testTag(ChatScreenTags.ENDED_BANNER),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.m, vertical = Space.s),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Icon(
                imageVector = Icons.Outlined.Info,
                contentDescription = null,
                tint = colors.inkMuted,
                modifier = Modifier.size(IconSize.Inline),
            )
            Text(
                text = message,
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
                modifier = Modifier.weight(1f),
            )
        }
    }
}
