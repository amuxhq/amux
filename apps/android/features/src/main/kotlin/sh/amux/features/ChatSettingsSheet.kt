package sh.amux.features

import sh.amux.design.IconSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.Changeable
import sh.amux.values.CommandView
import sh.amux.values.ContextView
import sh.amux.values.EffortChoice
import sh.amux.values.Kind
import sh.amux.values.ModeChoice
import sh.amux.values.ModelChoice
import sh.amux.values.PermissionChoice
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView
import sh.amux.words.ChatWords

/** Pure UI state rendered by [ChatSettingsSheetContent] and [ChatSettingsModalSheet]. */
@Immutable
data class ChatSettingsState(
    val settings: SettingsView,
    val kind: Kind? = null,
    val context: ContextView? = null,
)

/**
 * M3 [ModalBottomSheet] presenting chat model, effort, permission, mode,
 * context usage, and available slash commands.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatSettingsModalSheet(
    state: ChatSettingsState,
    onChange: (SettingChange) -> Unit,
    onDismiss: () -> Unit,
    onSelectCommand: ((CommandView) -> Unit)? = null,
) {
    val colors = AmuxTheme.colors
    AmuxSheet(
        onDismissRequest = onDismiss,
    ) {
        ChatSettingsSheetContent(
            state = state,
            onChange = onChange,
            onDismiss = onDismiss,
            onSelectCommand = onSelectCommand,
        )
    }
}

/**
 * Pure Compose body of the chat settings sheet. Exposed directly for headless
 * storybook and Robolectric tests.
 */
@Composable
fun ChatSettingsSheetContent(
    state: ChatSettingsState,
    onChange: (SettingChange) -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
    onSelectCommand: ((CommandView) -> Unit)? = null,
) {
    val colors = AmuxTheme.colors
    val view = state.settings
    val shownModes = if (view.changeable.mode == Changeable.Pick) {
        view.modes.filter { it.settable || it.current }
    } else {
        view.modes.filter { it.current }
    }

    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.ground)
            .verticalScroll(rememberScrollState())
            .padding(bottom = Space.xl)
            .testTag(AskReviewTags.CHAT_SETTINGS_SHEET),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        // Header
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = stringResource(R.string.chat_settings_title),
                style = AmuxTheme.typography.screenTitle,
                color = colors.ink,
                modifier = Modifier
                    .weight(1f)
                    .semantics { heading() },
            )
            val closeLabel = stringResource(R.string.action_close)
            IconButton(
                onClick = onDismiss,
                modifier = Modifier
                    .thumbTarget()
                    .testTag(AskReviewTags.CHAT_SETTINGS_CLOSE),
            ) {
                Icon(
                    imageVector = Icons.Outlined.Close,
                    contentDescription = closeLabel,
                    tint = colors.inkMuted,
                    modifier = Modifier.size(IconSize.Row),
                )
            }
        }

        // Models
        if (view.models.isNotEmpty()) {
            ModelsSection(
                models = view.models,
                changeable = view.changeable.model,
                onPick = { onChange(SettingChange.Model(it)) },
            )
        }

        // Efforts
        if (view.efforts.isNotEmpty()) {
            EffortsSection(
                efforts = view.efforts,
                changeable = view.changeable.effort,
                onPick = { onChange(SettingChange.Effort(it)) },
            )
        }

        val byTypingHint = ChatWords.byTyping(view.changeable)
        if (byTypingHint != null) {
            Text(
                text = byTypingHint,
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
                modifier = Modifier
                    .padding(horizontal = Space.margin)
                    .testTag(AskReviewTags.CHAT_SETTINGS_TYPING_HINT),
            )
        }

        // Permissions
        if (view.permissions.isNotEmpty() || view.changeable.permission == Changeable.Cycle) {
            PermissionsSection(
                permissions = view.permissions,
                kind = state.kind,
                changeable = view.changeable.permission,
                onPick = { onChange(SettingChange.Permission(it)) },
                onCycle = { onChange(SettingChange.CyclePermission) },
            )
        }

        // Modes (Codex default / plan)
        if (shownModes.isNotEmpty()) {
            ModesSection(
                modes = shownModes,
                changeable = view.changeable.mode,
                onPick = { onChange(SettingChange.Mode(it)) },
            )
        }

        // Context meter
        val context = state.context
        if (context != null) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(AskReviewTags.CHAT_SETTINGS_CONTEXT),
            ) {
                AmuxSectionHeader(
                    title = stringResource(R.string.chat_settings_section_context),
                    trailing = ChatWords.contextLeft(context),
                )
                AmuxPanel(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = Space.margin),
                    borderColor = if (context.nearFull) colors.accent.copy(alpha = 0.55f) else colors.hairline,
                ) {
                    ContextMeterBlock(
                        context = context,
                        modifier = Modifier.padding(Space.m),
                    )
                }
            }
        }

        // Available slash commands
        if (view.commands.isNotEmpty()) {
            CommandsSection(
                commands = view.commands,
                onSelect = onSelectCommand,
            )
        }
    }
}

@Composable
private fun ModelsSection(
    models: List<ModelChoice>,
    changeable: Changeable,
    onPick: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(title = stringResource(R.string.chat_settings_section_model))
        if (changeable == Changeable.Pick) {
            AmuxPanel(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin),
            ) {
                Column(modifier = Modifier.fillMaxWidth().selectableGroup()) {
                    models.forEachIndexed { index, choice ->
                        if (index > 0) AmuxHairline()
                        val detail = if (choice.unlisted) {
                            stringResource(R.string.chat_settings_reported_by_agent)
                        } else {
                            choice.description
                        }
                        AmuxRadioRow(
                            title = choice.displayName,
                            detail = detail,
                            selected = choice.current,
                            warn = false,
                            enabled = true,
                            onClick = { if (!choice.current) onPick(choice.value) },
                            modifier = Modifier.testTag(AskReviewTags.chatSettingsModel(choice.value)),
                        )
                    }
                }
            }
        } else {
            val current = models.firstOrNull { it.current }
            if (current != null) {
                Text(
                    text = current.displayName,
                    style = AmuxTheme.typography.mono,
                    color = colors.ink,
                    modifier = Modifier
                        .padding(horizontal = Space.margin)
                        .testTag(AskReviewTags.chatSettingsModel(current.value)),
                )
            }
        }
    }
}

@Composable
private fun EffortsSection(
    efforts: List<EffortChoice>,
    changeable: Changeable,
    onPick: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    val picksEffort = changeable == Changeable.Pick && efforts.size > 1
    val defaultEffort = efforts.firstOrNull { it.default }

    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(
            title = stringResource(R.string.chat_settings_section_effort),
            trailing = if (picksEffort && defaultEffort != null) {
                stringResource(R.string.chat_settings_default_effort, defaultEffort.value)
            } else {
                null
            },
        )
        if (picksEffort) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = Space.margin),
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                efforts.forEach { effort ->
                    FilterChip(
                        selected = effort.current,
                        onClick = { if (!effort.current) onPick(effort.value) },
                        label = {
                            Text(
                                text = effort.value,
                                style = AmuxTheme.typography.caption,
                            )
                        },
                        colors = FilterChipDefaults.filterChipColors(
                            selectedContainerColor = colors.ink,
                            selectedLabelColor = colors.ground,
                            containerColor = colors.sunken,
                            labelColor = colors.inkMuted,
                        ),
                        border = FilterChipDefaults.filterChipBorder(
                            enabled = true,
                            selected = effort.current,
                            borderColor = colors.hairline,
                            selectedBorderColor = colors.ink,
                        ),
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(AskReviewTags.chatSettingsEffort(effort.value)),
                    )
                }
            }
        } else {
            val current = efforts.firstOrNull { it.current }
            if (current != null) {
                Text(
                    text = current.value,
                    style = AmuxTheme.typography.mono,
                    color = colors.ink,
                    modifier = Modifier
                        .padding(horizontal = Space.margin)
                        .testTag(AskReviewTags.chatSettingsEffort(current.value)),
                )
            }
        }
    }
}

@Composable
private fun PermissionsSection(
    permissions: List<PermissionChoice>,
    kind: Kind?,
    changeable: Changeable,
    onPick: (String) -> Unit,
    onCycle: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val current = permissions.firstOrNull { it.current }

    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        AmuxSectionHeader(title = ChatWords.permissionsHeading(kind))
        when (changeable) {
            Changeable.Pick -> {
                val pickable = permissions.filter { it.settable || it.current }
                AmuxPanel(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = Space.margin),
                ) {
                    Column(modifier = Modifier.fillMaxWidth().selectableGroup()) {
                        pickable.forEachIndexed { index, choice ->
                            if (index > 0) AmuxHairline()
                            AmuxRadioRow(
                                title = ChatWords.permission(choice),
                                detail = ChatWords.permissionDetail(choice),
                                selected = choice.current,
                                warn = choice.neverAsks,
                                enabled = choice.settable,
                                onClick = {
                                    if (!choice.current && choice.settable) {
                                        onPick(choice.value)
                                    }
                                },
                                modifier = Modifier.testTag(AskReviewTags.chatSettingsPermission(choice.value)),
                            )
                        }
                    }
                }
            }
            Changeable.Cycle -> {
                AmuxPanel(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = Space.margin),
                ) {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = Space.m, vertical = Space.s),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.m),
                    ) {
                        if (current != null) {
                            Column(
                                modifier = Modifier.weight(1f),
                                verticalArrangement = Arrangement.spacedBy(Space.xxs),
                            ) {
                                Text(
                                    text = ChatWords.permission(current),
                                    style = AmuxTheme.typography.bodyEmphasis,
                                    color = if (current.neverAsks) colors.removed else colors.ink,
                                )
                                val detail = ChatWords.permissionDetail(current)
                                if (detail.isNotEmpty()) {
                                    Text(
                                        text = detail,
                                        style = AmuxTheme.typography.detail,
                                        color = colors.inkMuted,
                                    )
                                }
                            }
                        } else {
                            Spacer(modifier = Modifier.weight(1f))
                        }
                        AmuxActionButton(
                            label = stringResource(R.string.chat_settings_cycle),
                            kind = ActionButtonKind.Outline,
                            onClick = onCycle,
                            modifier = Modifier.testTag(AskReviewTags.CHAT_SETTINGS_CYCLE),
                        )
                    }
                }
                Text(
                    text = ChatWords.CYCLES_ONLY,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    modifier = Modifier
                        .padding(horizontal = Space.margin)
                        .testTag(AskReviewTags.CHAT_SETTINGS_CYCLES_HINT),
                )
            }
            is Changeable.ByTyping -> {
                if (current != null) {
                    Text(
                        text = ChatWords.permission(current),
                        style = AmuxTheme.typography.mono,
                        color = colors.ink,
                        modifier = Modifier.padding(horizontal = Space.margin),
                    )
                }
                Text(
                    text = stringResource(R.string.chat_settings_permission_by_typing, changeable.value),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    modifier = Modifier.padding(horizontal = Space.margin),
                )
            }
            Changeable.NotOffered -> {
                if (current != null) {
                    Text(
                        text = ChatWords.permission(current),
                        style = AmuxTheme.typography.mono,
                        color = colors.ink,
                        modifier = Modifier.padding(horizontal = Space.margin),
                    )
                }
            }
        }
    }
}

@Composable
private fun ModesSection(
    modes: List<ModeChoice>,
    changeable: Changeable,
    onPick: (String) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        AmuxSectionHeader(title = stringResource(R.string.chat_settings_section_mode))
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
        ) {
            Column(modifier = Modifier.fillMaxWidth().selectableGroup()) {
                modes.forEachIndexed { index, choice ->
                    if (index > 0) AmuxHairline()
                    val detail = if (choice.unlisted) {
                        stringResource(R.string.chat_settings_reported_by_agent)
                    } else {
                        ""
                    }
                    AmuxRadioRow(
                        title = ChatWords.mode(choice),
                        detail = detail,
                        selected = choice.current,
                        warn = false,
                        enabled = changeable == Changeable.Pick && choice.settable,
                        onClick = {
                            if (!choice.current && changeable == Changeable.Pick && choice.settable) {
                                onPick(choice.value)
                            }
                        },
                        modifier = Modifier.testTag(AskReviewTags.chatSettingsMode(choice.value)),
                    )
                }
            }
        }
        if (changeable is Changeable.ByTyping) {
            Text(
                text = stringResource(R.string.chat_settings_mode_by_typing, changeable.value),
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
                modifier = Modifier.padding(horizontal = Space.margin),
            )
        }
    }
}

@Composable
private fun CommandsSection(
    commands: List<CommandView>,
    onSelect: ((CommandView) -> Unit)?,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(
            title = stringResource(R.string.chat_settings_section_commands),
            trailing = commands.size.toString(),
        )
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
        ) {
            Column(modifier = Modifier.fillMaxWidth()) {
                commands.forEachIndexed { index, cmd ->
                    if (index > 0) AmuxHairline()
                    val invocation = buildString {
                        append(cmd.sigil.ifEmpty { "/" })
                        append(cmd.name)
                        if (cmd.argumentHint.isNotEmpty()) {
                            append(' ')
                            append(cmd.argumentHint)
                        }
                    }
                    val rowModifier = if (onSelect != null) {
                        Modifier
                            .fillMaxWidth()
                            .clickable(role = Role.Button) { onSelect(cmd) }
                            .thumbTarget()
                    } else {
                        Modifier.fillMaxWidth()
                    }
                    Column(
                        modifier = rowModifier
                            .padding(horizontal = Space.m, vertical = Space.s)
                            .testTag(AskReviewTags.chatSettingsCommand(cmd.name)),
                        verticalArrangement = Arrangement.spacedBy(Space.xxs),
                    ) {
                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(Space.s),
                        ) {
                            Text(
                                text = invocation,
                                style = AmuxTheme.typography.mono,
                                color = colors.ink,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.weight(1f),
                            )
                            if (cmd.source.isNotEmpty()) {
                                Text(
                                    text = cmd.source,
                                    style = AmuxTheme.typography.caption,
                                    color = colors.inkFaint,
                                )
                            }
                        }
                        if (cmd.description.isNotEmpty()) {
                            Text(
                                text = cmd.description,
                                style = AmuxTheme.typography.detail,
                                color = colors.inkMuted,
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                }
            }
        }
    }
}
