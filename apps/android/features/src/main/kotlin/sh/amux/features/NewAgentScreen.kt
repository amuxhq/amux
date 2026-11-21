package sh.amux.features

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxHairline
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.design.thumbTarget
import sh.amux.values.AgentNameProblem
import sh.amux.values.Changeable
import sh.amux.values.Directory
import sh.amux.values.HostView
import sh.amux.values.NewAgentPick
import sh.amux.values.Provider
import sh.amux.values.SettingsView
import sh.amux.words.ChatWords
import sh.amux.words.PlaceNames

/** Semantic test tags for [NewAgentScreen] and its picker / browser sheets. */
object NewAgentTags {
    const val SCREEN: String = "new-agent"
    const val BACK: String = "new-agent.back"
    const val NO_HOSTS: String = "new-agent.no-hosts"
    const val HOST_ROW: String = "new-agent.host"
    const val HOST_SHEET: String = "new-agent.host.sheet"
    const val PROVIDER_ROW: String = "new-agent.provider"
    const val DIRECTORY_ROW: String = "new-agent.directory"
    const val DIRECTORY_UNAVAILABLE: String = "new-agent.directory.unavailable"
    const val BROWSE_SHEET: String = "new-agent.browsing"
    const val BROWSE_DONE: String = "new-agent.browsing.done"
    const val BROWSE_SEARCH: String = "new-agent.browsing.search"
    const val BROWSE_SEARCH_CLEAR: String = "new-agent.browsing.search.clear"
    const val BROWSE_EMPTY: String = "new-agent.browsing.empty"
    const val BROWSE_READING: String = "new-agent.browsing.reading"
    const val BROWSE_UNAVAILABLE: String = "new-agent.browsing.unavailable"
    const val BROWSE_TYPED: String = "new-agent.typed"
    const val BROWSE_TYPED_USE: String = "new-agent.typed.use"
    const val MODEL_ROW: String = "new-agent.model"
    const val EFFORT_ROW: String = "new-agent.effort"
    const val PERMISSION_ROW: String = "new-agent.permission"
    const val MODE_ROW: String = "new-agent.mode"
    const val PICKER_SHEET: String = "new-agent.picker.sheet"
    const val WORKTREE_ROW: String = "new-agent.worktree"
    const val WORKTREE_SWITCH: String = "new-agent.worktree.switch"
    const val NAME_FIELD: String = "new-agent.name"
    const val NAME_ERROR: String = "new-agent.name.error"
    const val OFFER_ASKING: String = "new-agent.offer.asking"
    const val OFFER_UNAVAILABLE: String = "new-agent.offer.unavailable"
    const val SIGNED_OUT_WARNING: String = "new-agent.signed-out"
    const val FAILURE: String = "new-agent.refusal"
    const val START_BUTTON: String = "new-agent.start"

    fun hostOption(name: String): String = "new-agent.host.$name"
    fun providerChip(provider: Provider): String = "new-agent.provider.${provider.name.lowercase()}"
    fun recentChip(name: String): String = "new-agent.recent.$name"
    fun browseDirectory(name: String): String = "new-agent.repository.$name"
    fun browsePin(name: String): String = "new-agent.pin.$name"
    fun pickerOption(value: String): String = "new-agent.picker.$value"
}

/** Listing status displayed on [NewAgentScreen] and its folder browser. */
enum class NewAgentListingUi {
    None,
    Asking,
    Ready,
    Unavailable,
}

/** Provider catalogue fetch state displayed on [NewAgentScreen]. */
enum class NewAgentOfferUi {
    Asking,
    Offered,
    Unavailable,
}

/** Pure immutable UI state rendered by [NewAgentScreen]. */
@Immutable
data class NewAgentScreenState(
    val hosts: List<HostView> = emptyList(),
    val selectedHostId: List<Int>? = null,
    val connectingHostIds: Set<List<Int>> = emptySet(),
    val provider: Provider = Provider.Claude,
    val availableProviders: List<Provider> = listOf(Provider.Claude, Provider.Codex),
    val offer: NewAgentOfferUi = NewAgentOfferUi.Asking,
    val settings: SettingsView? = null,
    val listing: NewAgentListingUi = NewAgentListingUi.None,
    val repositories: List<Directory> = emptyList(),
    val recent: List<Directory> = emptyList(),
    val history: List<Directory> = emptyList(),
    val directory: String = "",
    val query: String = "",
    val typedPath: String = "",
    val browsing: Boolean = false,
    val newWorktree: Boolean = false,
    val name: String = "",
    val nameProblem: AgentNameProblem? = null,
    val starting: Boolean = false,
    val failure: String? = null,
) {
    val selectedHost: HostView?
        get() = selectedHostId?.let { id -> hosts.firstOrNull { it.hostId == id } }

    val searching: Boolean
        get() = query.trim().isNotEmpty()

    val found: List<Directory>
        get() {
            val wanted = query.trim().lowercase()
            if (wanted.isEmpty()) return emptyList()
            return (repositories + recent + history).filter { dir ->
                dir.name.lowercase().contains(wanted) || dir.path.lowercase().contains(wanted)
            }
        }

    val likely: List<Directory>
        get() = recent + repositories

    val providerSignedIn: Boolean?
        get() = selectedHost?.providers?.firstOrNull { it.provider == provider }?.signedIn

    fun isPinned(path: String): Boolean = repositories.any { it.path == path }

    val ready: Boolean
        get() {
            val hostId = selectedHostId ?: return false
            val host = hosts.firstOrNull { it.hostId == hostId }
            if (host != null && !host.isOnline) return false
            return directory.isNotEmpty() && nameProblem == null && !starting
        }
}

/** Pure actions emitted by [NewAgentScreen]. */
sealed interface NewAgentAction {
    data object Back : NewAgentAction
    data class SelectHost(val hostId: List<Int>) : NewAgentAction
    data class SelectProvider(val provider: Provider) : NewAgentAction
    data class SetBrowsing(val open: Boolean) : NewAgentAction
    data class ChooseDirectory(val path: String) : NewAgentAction
    data class UpdateQuery(val query: String) : NewAgentAction
    data class UpdateTypedPath(val path: String) : NewAgentAction
    data class PinFolder(val directory: Directory, val pinned: Boolean) : NewAgentAction
    data class PickSetting(val pick: NewAgentPick) : NewAgentAction
    data class SetNewWorktree(val enabled: Boolean) : NewAgentAction
    data class UpdateName(val name: String) : NewAgentAction
    data object Start : NewAgentAction
}

private enum class ActiveNewAgentPicker {
    Host,
    Model,
    Effort,
    Permission,
    Mode,
}

/**
 * Full-screen New Agent form with sticky bottom Start bar, host/provider
 * selection, folder browser sheet, M3 bottom-sheet setting pickers, worktree
 * switch, live name validation, and large-text stacking.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewAgentScreen(
    state: NewAgentScreenState,
    onAction: (NewAgentAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    val fontScale = LocalDensity.current.fontScale
    val isLargeText = fontScale >= 1.3f
    var activePicker by remember { mutableStateOf<ActiveNewAgentPicker?>(null) }

    val hostName = state.selectedHost?.name ?: stringResource(R.string.new_agent_host_fallback)
    val providerTitle = providerLabel(state.provider)

    AmuxGround(
        modifier = modifier.testTag(NewAgentTags.SCREEN),
    ) {
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
                            val backDesc = stringResource(R.string.action_back)
                            IconButton(
                                onClick = { onAction(NewAgentAction.Back) },
                                modifier = Modifier
                                    .thumbTarget()
                                    .testTag(NewAgentTags.BACK),
                            ) {
                                Icon(
                                    imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                                    contentDescription = backDesc,
                                )
                            }
                        },
                        title = {
                            Text(
                                text = stringResource(R.string.new_agent_title),
                                style = AmuxTheme.typography.screenTitle,
                                color = colors.ink,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                    )
                    AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
                }
            },
            bottomBar = {
                NewAgentStartBar(
                    hostName = state.selectedHost?.name,
                    starting = state.starting,
                    ready = state.ready,
                    failure = state.failure,
                    onStart = { onAction(NewAgentAction.Start) },
                )
            },
        ) { innerPadding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(innerPadding)
                    .verticalScroll(rememberScrollState())
                    .padding(top = Space.s, bottom = Space.xl),
                verticalArrangement = Arrangement.spacedBy(Space.l),
            ) {
                // Where: host + directory + quick recent chips
                Column(
                    modifier = Modifier.fillMaxWidth(),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    AmuxSectionHeader(title = stringResource(R.string.new_agent_section_where))
                    if (state.hosts.isEmpty()) {
                        Text(
                            text = stringResource(R.string.new_agent_no_hosts),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                            modifier = Modifier
                                .padding(horizontal = Space.margin)
                                .testTag(NewAgentTags.NO_HOSTS),
                        )
                    } else {
                        val canPickHost = state.hosts.size > 1 || state.selectedHost == null
                        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                            NewAgentFieldRow(
                                label = stringResource(R.string.new_agent_host_label),
                                value = state.selectedHost?.name
                                    ?: stringResource(R.string.new_agent_host_choose),
                                monoValue = false,
                                isLargeText = isLargeText,
                                showChevron = canPickHost,
                                enabled = canPickHost,
                                onClick = { activePicker = ActiveNewAgentPicker.Host },
                                modifier = Modifier.testTag(NewAgentTags.HOST_ROW),
                            )
                            AmuxInsetHairline()
                            val hasDirectory = state.directory.isNotEmpty()
                            NewAgentFieldRow(
                                label = stringResource(R.string.new_agent_directory_label),
                                value = if (hasDirectory) {
                                    PlaceNames.path(state.directory)
                                } else {
                                    stringResource(R.string.new_agent_directory_choose)
                                },
                                monoValue = hasDirectory,
                                isLargeText = isLargeText,
                                showChevron = true,
                                enabled = state.selectedHostId != null,
                                onClick = { onAction(NewAgentAction.SetBrowsing(true)) },
                                modifier = Modifier.testTag(NewAgentTags.DIRECTORY_ROW),
                            )
                        }

                        val chips = remember(state.likely, state.directory) {
                            state.likely.filter { it.path != state.directory }.take(4)
                        }
                        if (chips.isNotEmpty()) {
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .horizontalScroll(rememberScrollState())
                                    .padding(horizontal = Space.margin),
                                horizontalArrangement = Arrangement.spacedBy(Space.s),
                            ) {
                                for (dir in chips) {
                                    val chipSpoken = stringResource(R.string.new_agent_recent_spoken, dir.name)
                                    FilterChip(
                                        selected = false,
                                        onClick = { onAction(NewAgentAction.ChooseDirectory(dir.path)) },
                                        label = {
                                            Text(
                                                text = dir.name,
                                                style = AmuxTheme.typography.monoSmall,
                                                maxLines = 1,
                                                overflow = TextOverflow.Ellipsis,
                                            )
                                        },
                                        colors = FilterChipDefaults.filterChipColors(
                                            containerColor = colors.sunken,
                                            labelColor = colors.ink,
                                        ),
                                        border = FilterChipDefaults.filterChipBorder(
                                            enabled = true,
                                            selected = false,
                                            borderColor = colors.hairline,
                                        ),
                                        modifier = Modifier
                                            .thumbTarget()
                                            .semantics { contentDescription = chipSpoken }
                                            .testTag(NewAgentTags.recentChip(dir.name)),
                                    )
                                }
                            }
                        }
                    }

                    if (state.listing == NewAgentListingUi.Unavailable) {
                        Text(
                            text = stringResource(R.string.new_agent_directory_unavailable, hostName),
                            style = AmuxTheme.typography.detail,
                            color = colors.inkMuted,
                            modifier = Modifier
                                .padding(horizontal = Space.margin)
                                .testTag(NewAgentTags.DIRECTORY_UNAVAILABLE),
                        )
                    }
                }

                // Agent: provider segmented chips + model/effort/permission/mode + worktree
                Column(
                    modifier = Modifier.fillMaxWidth(),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    AmuxSectionHeader(title = stringResource(R.string.new_agent_section_agent))
                    ProviderChipBar(
                        providers = state.availableProviders,
                        selected = state.provider,
                        onSelect = { onAction(NewAgentAction.SelectProvider(it)) },
                    )

                    AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
                        var hasPrecedingRow = false
                        val settings = state.settings
                        if (state.offer == NewAgentOfferUi.Offered && settings != null) {
                            val unnamed = stringResource(R.string.new_agent_unnamed_value)
                            if (settings.models.isNotEmpty()) {
                                val currentModel = settings.models.firstOrNull { it.current }?.displayName ?: unnamed
                                NewAgentFieldRow(
                                    label = stringResource(R.string.new_agent_model_label),
                                    value = currentModel,
                                    monoValue = false,
                                    isLargeText = isLargeText,
                                    showChevron = true,
                                    enabled = true,
                                    onClick = { activePicker = ActiveNewAgentPicker.Model },
                                    modifier = Modifier.testTag(NewAgentTags.MODEL_ROW),
                                )
                                hasPrecedingRow = true
                            }
                            if (settings.changeable.effort == Changeable.Pick && settings.efforts.isNotEmpty()) {
                                if (hasPrecedingRow) AmuxInsetHairline()
                                val currentEffort = settings.efforts.firstOrNull { it.current }?.value ?: unnamed
                                NewAgentFieldRow(
                                    label = stringResource(R.string.new_agent_effort_label),
                                    value = currentEffort,
                                    monoValue = false,
                                    isLargeText = isLargeText,
                                    showChevron = true,
                                    enabled = true,
                                    onClick = { activePicker = ActiveNewAgentPicker.Effort },
                                    modifier = Modifier.testTag(NewAgentTags.EFFORT_ROW),
                                )
                                hasPrecedingRow = true
                            }
                            if (settings.changeable.permission == Changeable.Pick && settings.permissions.isNotEmpty()) {
                                if (hasPrecedingRow) AmuxInsetHairline()
                                val currentPerm = settings.permissions.firstOrNull { it.current }
                                val permLabel = currentPerm?.let(ChatWords::permission) ?: unnamed
                                NewAgentFieldRow(
                                    label = stringResource(R.string.new_agent_permission_label),
                                    value = permLabel,
                                    monoValue = false,
                                    isLargeText = isLargeText,
                                    showChevron = true,
                                    enabled = true,
                                    warnValue = currentPerm?.neverAsks == true,
                                    onClick = { activePicker = ActiveNewAgentPicker.Permission },
                                    modifier = Modifier.testTag(NewAgentTags.PERMISSION_ROW),
                                )
                                hasPrecedingRow = true
                            }
                            if (settings.changeable.mode == Changeable.Pick && settings.modes.isNotEmpty()) {
                                if (hasPrecedingRow) AmuxInsetHairline()
                                val currentMode = settings.modes.firstOrNull { it.current }
                                    ?.let(ChatWords::mode) ?: unnamed
                                NewAgentFieldRow(
                                    label = stringResource(R.string.new_agent_mode_label),
                                    value = currentMode,
                                    monoValue = false,
                                    isLargeText = isLargeText,
                                    showChevron = true,
                                    enabled = true,
                                    onClick = { activePicker = ActiveNewAgentPicker.Mode },
                                    modifier = Modifier.testTag(NewAgentTags.MODE_ROW),
                                )
                                hasPrecedingRow = true
                            }
                        }

                        if (hasPrecedingRow) AmuxInsetHairline()
                        WorktreeToggleRow(
                            checked = state.newWorktree,
                            isLargeText = isLargeText,
                            onCheckedChange = { onAction(NewAgentAction.SetNewWorktree(it)) },
                        )
                    }

                    if (state.providerSignedIn == false) {
                        Text(
                            text = stringResource(
                                R.string.new_agent_provider_signed_out,
                                providerTitle,
                                hostName,
                            ),
                            style = AmuxTheme.typography.detail,
                            color = colors.accent,
                            modifier = Modifier
                                .padding(horizontal = Space.margin)
                                .testTag(NewAgentTags.SIGNED_OUT_WARNING),
                        )
                    }

                    if (state.selectedHostId != null) {
                        when (state.offer) {
                            NewAgentOfferUi.Offered -> Unit
                            NewAgentOfferUi.Asking -> {
                                Text(
                                    text = stringResource(
                                        R.string.new_agent_offer_asking,
                                        hostName,
                                        providerTitle,
                                    ),
                                    style = AmuxTheme.typography.detail,
                                    color = colors.inkMuted,
                                    modifier = Modifier
                                        .padding(horizontal = Space.margin)
                                        .testTag(NewAgentTags.OFFER_ASKING),
                                )
                            }
                            NewAgentOfferUi.Unavailable -> {
                                Text(
                                    text = stringResource(
                                        R.string.new_agent_offer_unavailable,
                                        hostName,
                                        providerTitle,
                                    ),
                                    style = AmuxTheme.typography.detail,
                                    color = colors.inkMuted,
                                    modifier = Modifier
                                        .padding(horizontal = Space.margin)
                                        .testTag(NewAgentTags.OFFER_UNAVAILABLE),
                                )
                            }
                        }
                    }
                }

                // Name section with live validation error
                Column(
                    modifier = Modifier.fillMaxWidth(),
                    verticalArrangement = Arrangement.spacedBy(Space.s),
                ) {
                    AmuxSectionHeader(title = stringResource(R.string.new_agent_section_name))
                    val nameLabel = stringResource(R.string.new_agent_name_label)
                    OutlinedTextField(
                        value = state.name,
                        onValueChange = { onAction(NewAgentAction.UpdateName(it)) },
                        singleLine = true,
                        isError = state.nameProblem != null,
                        placeholder = {
                            Text(
                                text = stringResource(R.string.new_agent_name_placeholder),
                                style = AmuxTheme.typography.mono,
                                color = colors.inkFaint,
                            )
                        },
                        textStyle = AmuxTheme.typography.mono.copy(color = colors.ink),
                        shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                        colors = OutlinedTextFieldDefaults.colors(
                            focusedContainerColor = colors.raised,
                            unfocusedContainerColor = colors.raised,
                            errorContainerColor = colors.raised,
                            focusedBorderColor = colors.ink,
                            unfocusedBorderColor = colors.hairline,
                            errorBorderColor = colors.removed,
                            focusedTextColor = colors.ink,
                            unfocusedTextColor = colors.ink,
                            errorTextColor = colors.ink,
                            cursorColor = colors.ink,
                        ),
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = Space.margin)
                            .semantics { contentDescription = nameLabel }
                            .testTag(NewAgentTags.NAME_FIELD),
                    )
                    val problem = state.nameProblem
                    if (problem != null) {
                        Text(
                            text = ChatWords.nameProblem(problem),
                            style = AmuxTheme.typography.detail,
                            color = colors.removed,
                            modifier = Modifier
                                .padding(horizontal = Space.margin)
                                .testTag(NewAgentTags.NAME_ERROR),
                        )
                    }
                }
            }
        }
    }

    // Folder browsing modal sheet
    if (state.browsing) {
        val browseSheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(
            onDismissRequest = { onAction(NewAgentAction.SetBrowsing(false)) },
            sheetState = browseSheetState,
            containerColor = colors.ground,
            contentColor = colors.ink,
        ) {
            NewAgentDirectoriesContent(
                state = state,
                onAction = onAction,
            )
        }
    }

    // Host / Model / Effort / Permission / Mode picker sheets
    val picker = activePicker
    if (picker != null) {
        val pickerSheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(
            onDismissRequest = { activePicker = null },
            sheetState = pickerSheetState,
            containerColor = colors.ground,
            contentColor = colors.ink,
        ) {
            when (picker) {
                ActiveNewAgentPicker.Host -> {
                    HostPickerSheetContent(
                        hosts = state.hosts,
                        selectedHostId = state.selectedHostId,
                        connectingHostIds = state.connectingHostIds,
                        onSelect = { hostId ->
                            activePicker = null
                            onAction(NewAgentAction.SelectHost(hostId))
                        },
                        onDismiss = { activePicker = null },
                    )
                }
                ActiveNewAgentPicker.Model -> {
                    val models = state.settings?.models.orEmpty()
                    ChoicePickerSheetContent(
                        title = stringResource(R.string.new_agent_model_label),
                        options = models.map { model ->
                            PickerOptionUi(
                                value = model.value,
                                title = model.displayName,
                                detail = model.description,
                                current = model.current,
                                warn = false,
                                enabled = true,
                            )
                        },
                        onSelect = { value ->
                            activePicker = null
                            onAction(NewAgentAction.PickSetting(NewAgentPick.Model(value)))
                        },
                        onDismiss = { activePicker = null },
                    )
                }
                ActiveNewAgentPicker.Effort -> {
                    val efforts = state.settings?.efforts.orEmpty()
                    val defaultLabel = stringResource(R.string.new_agent_effort_default)
                    ChoicePickerSheetContent(
                        title = stringResource(R.string.new_agent_effort_label),
                        options = efforts.map { effort ->
                            PickerOptionUi(
                                value = effort.value,
                                title = effort.value,
                                detail = if (effort.default) defaultLabel else "",
                                current = effort.current,
                                warn = false,
                                enabled = true,
                            )
                        },
                        onSelect = { value ->
                            activePicker = null
                            onAction(NewAgentAction.PickSetting(NewAgentPick.Effort(value)))
                        },
                        onDismiss = { activePicker = null },
                    )
                }
                ActiveNewAgentPicker.Permission -> {
                    val permissions = state.settings?.permissions.orEmpty().filter { it.settable || it.current }
                    ChoicePickerSheetContent(
                        title = stringResource(R.string.new_agent_permission_label),
                        options = permissions.map { perm ->
                            PickerOptionUi(
                                value = perm.value,
                                title = ChatWords.permission(perm),
                                detail = ChatWords.permissionDetail(perm),
                                current = perm.current,
                                warn = perm.neverAsks,
                                enabled = perm.settable,
                            )
                        },
                        onSelect = { value ->
                            activePicker = null
                            onAction(NewAgentAction.PickSetting(NewAgentPick.Permission(value)))
                        },
                        onDismiss = { activePicker = null },
                    )
                }
                ActiveNewAgentPicker.Mode -> {
                    val modes = state.settings?.modes.orEmpty().filter { it.settable || it.current }
                    ChoicePickerSheetContent(
                        title = stringResource(R.string.new_agent_mode_label),
                        options = modes.map { mode ->
                            PickerOptionUi(
                                value = mode.value,
                                title = ChatWords.mode(mode),
                                detail = "",
                                current = mode.current,
                                warn = false,
                                enabled = mode.settable,
                            )
                        },
                        onSelect = { value ->
                            activePicker = null
                            onAction(NewAgentAction.PickSetting(NewAgentPick.Mode(value)))
                        },
                        onDismiss = { activePicker = null },
                    )
                }
            }
        }
    }
}

@Composable
private fun NewAgentStartBar(
    hostName: String?,
    starting: Boolean,
    ready: Boolean,
    failure: String?,
    onStart: () -> Unit,
) {
    val colors = AmuxTheme.colors
    val label = when {
        starting -> stringResource(R.string.new_agent_starting)
        !hostName.isNullOrEmpty() -> stringResource(R.string.new_agent_start_on, hostName)
        else -> stringResource(R.string.new_agent_start_button)
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(colors.ground)
            .windowInsetsPadding(WindowInsets.navigationBars.union(WindowInsets.ime)),
    ) {
        AmuxHairline(color = colors.hairline.copy(alpha = 0.6f))
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin, vertical = Space.m),
            verticalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            if (!failure.isNullOrEmpty()) {
                Text(
                    text = failure,
                    style = AmuxTheme.typography.detail,
                    color = colors.removed,
                    modifier = Modifier.testTag(NewAgentTags.FAILURE),
                )
            }
            AmuxActionButton(
                label = label,
                kind = ActionButtonKind.Primary,
                fillWidth = true,
                enabled = ready,
                onClick = onStart,
                modifier = Modifier.testTag(NewAgentTags.START_BUTTON),
            )
        }
    }
}

@Composable
private fun ProviderChipBar(
    providers: List<Provider>,
    selected: Provider,
    onSelect: (Provider) -> Unit,
) {
    val colors = AmuxTheme.colors
    val chipColors = FilterChipDefaults.filterChipColors(
        containerColor = colors.raised,
        labelColor = colors.inkMuted,
        selectedContainerColor = colors.ink,
        selectedLabelColor = colors.ground,
    )
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = Space.margin)
            .testTag(NewAgentTags.PROVIDER_ROW),
        horizontalArrangement = Arrangement.spacedBy(Space.s),
    ) {
        for (item in providers.filter { it != Provider.Unspecified }) {
            val isSelected = item == selected
            FilterChip(
                selected = isSelected,
                onClick = { onSelect(item) },
                label = {
                    Text(
                        text = providerLabel(item),
                        style = AmuxTheme.typography.bodyEmphasis,
                    )
                },
                colors = chipColors,
                border = FilterChipDefaults.filterChipBorder(
                    enabled = true,
                    selected = isSelected,
                    borderColor = colors.hairline,
                    selectedBorderColor = colors.ink,
                ),
                modifier = Modifier
                    .thumbTarget()
                    .testTag(NewAgentTags.providerChip(item)),
            )
        }
    }
}

@Composable
private fun NewAgentFieldRow(
    label: String,
    value: String,
    monoValue: Boolean,
    isLargeText: Boolean,
    showChevron: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    warnValue: Boolean = false,
) {
    val colors = AmuxTheme.colors
    val valueStyle = if (monoValue) AmuxTheme.typography.monoSmall else AmuxTheme.typography.body
    val valueColor = when {
        warnValue -> colors.removed
        !enabled -> colors.inkFaint
        else -> colors.inkMuted
    }
    val rowModifier = modifier
        .fillMaxWidth()
        .thumbTarget()
        .let {
            if (enabled) it.clickable(role = Role.Button, onClick = onClick) else it
        }
        .padding(horizontal = Space.l, vertical = Space.m)

    if (isLargeText) {
        Row(
            modifier = rowModifier,
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Text(
                    text = label,
                    style = AmuxTheme.typography.body,
                    color = if (enabled) colors.ink else colors.inkMuted,
                )
                Text(
                    text = value,
                    style = valueStyle,
                    color = valueColor,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            if (showChevron) {
                Icon(
                    imageVector = Icons.Outlined.ChevronRight,
                    contentDescription = null,
                    tint = colors.inkFaint,
                    modifier = Modifier.size(18.dp),
                )
            }
        }
    } else {
        Row(
            modifier = rowModifier,
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Text(
                text = label,
                style = AmuxTheme.typography.body,
                color = if (enabled) colors.ink else colors.inkMuted,
                maxLines = 1,
            )
            Spacer(modifier = Modifier.weight(1f))
            Text(
                text = value,
                style = valueStyle,
                color = valueColor,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            if (showChevron) {
                Icon(
                    imageVector = Icons.Outlined.ChevronRight,
                    contentDescription = null,
                    tint = colors.inkFaint,
                    modifier = Modifier.size(18.dp),
                )
            }
        }
    }
}

@Composable
private fun WorktreeToggleRow(
    checked: Boolean,
    isLargeText: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    val colors = AmuxTheme.colors
    val title = stringResource(R.string.new_agent_worktree_title)
    val detail = stringResource(R.string.new_agent_worktree_detail)
    val switchColors = SwitchDefaults.colors(
        checkedThumbColor = colors.ground,
        checkedTrackColor = colors.ink,
        checkedBorderColor = colors.ink,
        uncheckedThumbColor = colors.inkMuted,
        uncheckedTrackColor = colors.sunken,
        uncheckedBorderColor = colors.hairline,
    )

    if (isLargeText) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clickable(role = Role.Switch) { onCheckedChange(!checked) }
                .padding(horizontal = Space.l, vertical = Space.m)
                .testTag(NewAgentTags.WORKTREE_ROW),
            verticalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            Text(
                text = title,
                style = AmuxTheme.typography.body,
                color = colors.ink,
            )
            Text(
                text = detail,
                style = AmuxTheme.typography.detail,
                color = colors.inkMuted,
            )
            Switch(
                checked = checked,
                onCheckedChange = onCheckedChange,
                colors = switchColors,
                modifier = Modifier.testTag(NewAgentTags.WORKTREE_SWITCH),
            )
        }
    } else {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .thumbTarget()
                .clickable(role = Role.Switch) { onCheckedChange(!checked) }
                .padding(horizontal = Space.l, vertical = Space.m)
                .testTag(NewAgentTags.WORKTREE_ROW),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Space.m),
        ) {
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(Space.xxs),
            ) {
                Text(
                    text = title,
                    style = AmuxTheme.typography.body,
                    color = colors.ink,
                )
                Text(
                    text = detail,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                )
            }
            Switch(
                checked = checked,
                onCheckedChange = onCheckedChange,
                colors = switchColors,
                modifier = Modifier.testTag(NewAgentTags.WORKTREE_SWITCH),
            )
        }
    }
}

/**
 * Pure Compose body of the folder browser sheet. Exposed directly for headless
 * storybook and Robolectric tests alongside [NewAgentScreen].
 */
@Composable
fun NewAgentDirectoriesContent(
    state: NewAgentScreenState,
    onAction: (NewAgentAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(colors.ground)
            .verticalScroll(rememberScrollState())
            .padding(bottom = Space.xl)
            .testTag(NewAgentTags.BROWSE_SHEET),
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
                text = stringResource(R.string.new_agent_browse_title),
                style = AmuxTheme.typography.screenTitle,
                color = colors.ink,
                modifier = Modifier.weight(1f),
            )
            TextButton(
                onClick = { onAction(NewAgentAction.SetBrowsing(false)) },
                modifier = Modifier
                    .thumbTarget()
                    .testTag(NewAgentTags.BROWSE_DONE),
            ) {
                Text(
                    text = stringResource(R.string.new_agent_browse_done),
                    style = AmuxTheme.typography.bodyEmphasis,
                    color = colors.ink,
                )
            }
        }

        // Search bar
        if (state.listing != NewAgentListingUi.Unavailable) {
            OutlinedTextField(
                value = state.query,
                onValueChange = { onAction(NewAgentAction.UpdateQuery(it)) },
                singleLine = true,
                placeholder = {
                    Text(
                        text = stringResource(R.string.new_agent_browse_search_placeholder),
                        style = AmuxTheme.typography.detail,
                        color = colors.inkFaint,
                    )
                },
                leadingIcon = {
                    Icon(
                        imageVector = Icons.Outlined.Search,
                        contentDescription = null,
                        tint = colors.inkMuted,
                        modifier = Modifier.size(18.dp),
                    )
                },
                trailingIcon = {
                    if (state.query.isNotEmpty()) {
                        val clearLabel = stringResource(R.string.new_agent_browse_search_clear)
                        IconButton(
                            onClick = { onAction(NewAgentAction.UpdateQuery("")) },
                            modifier = Modifier
                                .thumbTarget()
                                .testTag(NewAgentTags.BROWSE_SEARCH_CLEAR),
                        ) {
                            Icon(
                                imageVector = Icons.Outlined.Close,
                                contentDescription = clearLabel,
                                tint = colors.inkMuted,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                },
                textStyle = AmuxTheme.typography.body.copy(color = colors.ink),
                shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedContainerColor = colors.raised,
                    unfocusedContainerColor = colors.raised,
                    focusedBorderColor = colors.ink,
                    unfocusedBorderColor = colors.hairline,
                    focusedTextColor = colors.ink,
                    unfocusedTextColor = colors.ink,
                    cursorColor = colors.ink,
                ),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin)
                    .testTag(NewAgentTags.BROWSE_SEARCH),
            )
        }

        // Directory sections
        if (state.searching) {
            if (state.found.isNotEmpty()) {
                DirectorySection(
                    title = stringResource(R.string.new_agent_browse_section_found),
                    directories = state.found,
                    selectedPath = state.directory,
                    isRecent = false,
                    isPinned = state::isPinned,
                    onChoose = { dir ->
                        onAction(NewAgentAction.ChooseDirectory(dir.path))
                        onAction(NewAgentAction.SetBrowsing(false))
                    },
                    onTogglePin = { dir, pin -> onAction(NewAgentAction.PinFolder(dir, pin)) },
                )
            } else if (state.listing != NewAgentListingUi.Asking) {
                Text(
                    text = stringResource(R.string.new_agent_browse_no_matches),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    modifier = Modifier
                        .padding(horizontal = Space.margin)
                        .testTag(NewAgentTags.BROWSE_EMPTY),
                )
            }
        } else {
            if (state.repositories.isNotEmpty()) {
                DirectorySection(
                    title = stringResource(R.string.new_agent_browse_section_repositories),
                    directories = state.repositories,
                    selectedPath = state.directory,
                    isRecent = false,
                    isPinned = state::isPinned,
                    onChoose = { dir ->
                        onAction(NewAgentAction.ChooseDirectory(dir.path))
                        onAction(NewAgentAction.SetBrowsing(false))
                    },
                    onTogglePin = { dir, pin -> onAction(NewAgentAction.PinFolder(dir, pin)) },
                )
            }
            if (state.recent.isNotEmpty()) {
                DirectorySection(
                    title = stringResource(R.string.new_agent_browse_section_recent),
                    directories = state.recent,
                    selectedPath = state.directory,
                    isRecent = true,
                    isPinned = state::isPinned,
                    onChoose = { dir ->
                        onAction(NewAgentAction.ChooseDirectory(dir.path))
                        onAction(NewAgentAction.SetBrowsing(false))
                    },
                    onTogglePin = { dir, pin -> onAction(NewAgentAction.PinFolder(dir, pin)) },
                )
            }
            if (state.history.isNotEmpty()) {
                DirectorySection(
                    title = stringResource(R.string.new_agent_browse_section_history),
                    directories = state.history,
                    selectedPath = state.directory,
                    isRecent = true,
                    isPinned = state::isPinned,
                    onChoose = { dir ->
                        onAction(NewAgentAction.ChooseDirectory(dir.path))
                        onAction(NewAgentAction.SetBrowsing(false))
                    },
                    onTogglePin = { dir, pin -> onAction(NewAgentAction.PinFolder(dir, pin)) },
                )
            }
            if (state.listing == NewAgentListingUi.Ready &&
                state.repositories.isEmpty() &&
                state.recent.isEmpty() &&
                state.history.isEmpty()
            ) {
                Text(
                    text = stringResource(R.string.new_agent_browse_empty),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    modifier = Modifier
                        .padding(horizontal = Space.margin)
                        .testTag(NewAgentTags.BROWSE_EMPTY),
                )
            }
        }

        when (state.listing) {
            NewAgentListingUi.Asking -> {
                Text(
                    text = stringResource(R.string.new_agent_browse_reading),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    modifier = Modifier
                        .padding(horizontal = Space.margin)
                        .testTag(NewAgentTags.BROWSE_READING),
                )
            }
            NewAgentListingUi.Unavailable -> {
                Text(
                    text = stringResource(R.string.new_agent_browse_unavailable),
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    modifier = Modifier
                        .padding(horizontal = Space.margin)
                        .testTag(NewAgentTags.BROWSE_UNAVAILABLE),
                )
            }
            NewAgentListingUi.None, NewAgentListingUi.Ready -> Unit
        }

        // Typed custom path entry
        Column(
            modifier = Modifier.fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(Space.s),
        ) {
            AmuxSectionHeader(title = stringResource(R.string.new_agent_browse_section_typed))
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Space.margin),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Space.s),
            ) {
                OutlinedTextField(
                    value = state.typedPath,
                    onValueChange = { onAction(NewAgentAction.UpdateTypedPath(it)) },
                    singleLine = true,
                    placeholder = {
                        Text(
                            text = stringResource(R.string.new_agent_browse_typed_placeholder),
                            style = AmuxTheme.typography.monoSmall,
                            color = colors.inkFaint,
                        )
                    },
                    textStyle = AmuxTheme.typography.mono.copy(color = colors.ink),
                    shape = RoundedCornerShape(AmuxTheme.metrics.controlRadius),
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedContainerColor = colors.raised,
                        unfocusedContainerColor = colors.raised,
                        focusedBorderColor = colors.ink,
                        unfocusedBorderColor = colors.hairline,
                        focusedTextColor = colors.ink,
                        unfocusedTextColor = colors.ink,
                        cursorColor = colors.ink,
                    ),
                    modifier = Modifier
                        .weight(1f)
                        .testTag(NewAgentTags.BROWSE_TYPED),
                )
                val trimmedTyped = state.typedPath.trim()
                val useSpoken = stringResource(R.string.new_agent_browse_typed_use_spoken)
                AmuxActionButton(
                    label = stringResource(R.string.new_agent_browse_typed_use),
                    kind = ActionButtonKind.Outline,
                    enabled = trimmedTyped.isNotEmpty(),
                    onClick = {
                        if (trimmedTyped.isNotEmpty()) {
                            onAction(NewAgentAction.ChooseDirectory(trimmedTyped))
                            onAction(NewAgentAction.SetBrowsing(false))
                        }
                    },
                    modifier = Modifier
                        .semantics { contentDescription = useSpoken }
                        .testTag(NewAgentTags.BROWSE_TYPED_USE),
                )
            }
        }
    }
}

@Composable
private fun DirectorySection(
    title: String,
    directories: List<Directory>,
    selectedPath: String,
    isRecent: Boolean,
    isPinned: (String) -> Boolean,
    onChoose: (Directory) -> Unit,
    onTogglePin: (Directory, Boolean) -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(modifier = Modifier.fillMaxWidth()) {
        AmuxSectionHeader(title = title)
        AmuxRowCard(modifier = Modifier.padding(horizontal = Space.margin)) {
            directories.forEachIndexed { index, dir ->
                if (index > 0) AmuxInsetHairline()
                val pinned = isPinned(dir.path)
                val chosen = selectedPath == dir.path
                val pinDesc = stringResource(
                    if (pinned) R.string.new_agent_browse_pin_remove else R.string.new_agent_browse_pin_add,
                )
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Row(
                        modifier = Modifier
                            .weight(1f)
                            .thumbTarget()
                            .clickable(role = Role.Button) { onChoose(dir) }
                            .padding(start = Space.l, end = Space.s, top = Space.s, bottom = Space.s)
                            .semantics {
                                contentDescription = dir.path
                                selected = chosen
                            }
                            .testTag(NewAgentTags.browseDirectory(dir.name)),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Space.m),
                    ) {
                        Icon(
                            imageVector = if (isRecent) Icons.Outlined.Schedule else Icons.Outlined.Folder,
                            contentDescription = null,
                            tint = colors.inkFaint,
                            modifier = Modifier.size(18.dp),
                        )
                        Column(
                            modifier = Modifier.weight(1f),
                            verticalArrangement = Arrangement.spacedBy(Space.xxs),
                        ) {
                            Text(
                                text = dir.name,
                                style = AmuxTheme.typography.identifier,
                                color = colors.ink,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                text = PlaceNames.path(dir.path),
                                style = AmuxTheme.typography.monoSmall,
                                color = colors.inkFaint,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                        if (chosen) {
                            Icon(
                                imageVector = Icons.Outlined.Check,
                                contentDescription = null,
                                tint = colors.ink,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                    IconButton(
                        onClick = { onTogglePin(dir, !pinned) },
                        modifier = Modifier
                            .thumbTarget()
                            .testTag(NewAgentTags.browsePin(dir.name)),
                    ) {
                        Icon(
                            imageVector = if (pinned) Icons.Filled.PushPin else Icons.Outlined.PushPin,
                            contentDescription = pinDesc,
                            tint = if (pinned) colors.ink else colors.inkFaint,
                            modifier = Modifier.size(18.dp),
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun HostPickerSheetContent(
    hosts: List<HostView>,
    selectedHostId: List<Int>?,
    connectingHostIds: Set<List<Int>>,
    onSelect: (List<Int>) -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(colors.ground)
            .verticalScroll(rememberScrollState())
            .padding(bottom = Space.xl)
            .testTag(NewAgentTags.HOST_SHEET),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        PickerSheetHeader(
            title = stringResource(R.string.new_agent_host_sheet_title),
            onDismiss = onDismiss,
        )
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
        ) {
            Column(modifier = Modifier.fillMaxWidth()) {
                hosts.forEachIndexed { index, host ->
                    if (index > 0) AmuxHairline()
                    val selected = host.hostId == selectedHostId
                    val subtitle = hostPickerSubtitle(host, host.hostId in connectingHostIds)
                    PickerRadioRow(
                        title = host.name,
                        detail = subtitle,
                        current = selected,
                        warn = false,
                        enabled = host.isOnline,
                        onClick = { if (host.isOnline) onSelect(host.hostId) },
                        modifier = Modifier.testTag(NewAgentTags.hostOption(host.name)),
                    )
                }
            }
        }
    }
}

@Immutable
private data class PickerOptionUi(
    val value: String,
    val title: String,
    val detail: String,
    val current: Boolean,
    val warn: Boolean,
    val enabled: Boolean,
)

@Composable
private fun ChoicePickerSheetContent(
    title: String,
    options: List<PickerOptionUi>,
    onSelect: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(colors.ground)
            .verticalScroll(rememberScrollState())
            .padding(bottom = Space.xl)
            .testTag(NewAgentTags.PICKER_SHEET),
        verticalArrangement = Arrangement.spacedBy(Space.m),
    ) {
        PickerSheetHeader(title = title, onDismiss = onDismiss)
        AmuxPanel(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Space.margin),
        ) {
            Column(modifier = Modifier.fillMaxWidth()) {
                options.forEachIndexed { index, option ->
                    if (index > 0) AmuxHairline()
                    PickerRadioRow(
                        title = option.title,
                        detail = option.detail,
                        current = option.current,
                        warn = option.warn,
                        enabled = option.enabled,
                        onClick = { if (option.enabled) onSelect(option.value) },
                        modifier = Modifier.testTag(NewAgentTags.pickerOption(option.value)),
                    )
                }
            }
        }
    }
}

@Composable
private fun PickerSheetHeader(
    title: String,
    onDismiss: () -> Unit,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Space.margin),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title,
            style = AmuxTheme.typography.screenTitle,
            color = colors.ink,
            modifier = Modifier.weight(1f),
        )
        val closeLabel = stringResource(R.string.action_close)
        IconButton(
            onClick = onDismiss,
            modifier = Modifier.thumbTarget(),
        ) {
            Icon(
                imageVector = Icons.Outlined.Close,
                contentDescription = closeLabel,
                tint = colors.inkMuted,
                modifier = Modifier.size(20.dp),
            )
        }
    }
}

@Composable
private fun PickerRadioRow(
    title: String,
    detail: String,
    current: Boolean,
    warn: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = AmuxTheme.colors
    Row(
        modifier = modifier
            .fillMaxWidth()
            .clickable(enabled = enabled || current, role = Role.RadioButton, onClick = onClick)
            .thumbTarget()
            .padding(horizontal = Space.s, vertical = Space.s)
            .semantics { selected = current },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Space.xs),
    ) {
        RadioButton(
            selected = current,
            onClick = if (enabled || current) onClick else null,
            enabled = enabled || current,
            colors = RadioButtonDefaults.colors(
                selectedColor = colors.ink,
                unselectedColor = colors.inkMuted,
                disabledSelectedColor = colors.inkMuted,
                disabledUnselectedColor = colors.inkFaint,
            ),
        )
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(Space.xxs),
        ) {
            Text(
                text = title,
                style = if (current) AmuxTheme.typography.bodyEmphasis else AmuxTheme.typography.body,
                color = when {
                    warn -> colors.removed
                    !enabled && !current -> colors.inkFaint
                    else -> colors.ink
                },
            )
            if (detail.isNotEmpty() && detail != title) {
                Text(
                    text = detail,
                    style = AmuxTheme.typography.detail,
                    color = colors.inkMuted,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

@Composable
private fun providerLabel(provider: Provider): String = when (provider) {
    Provider.Codex -> stringResource(R.string.new_agent_provider_codex)
    Provider.Claude, Provider.Unspecified -> stringResource(R.string.new_agent_provider_claude)
}

@Composable
private fun hostPickerSubtitle(host: HostView, connecting: Boolean): String = when (host.reachGroup()) {
    HostReachGroup.Away -> stringResource(R.string.new_agent_host_away)
    HostReachGroup.Offline -> stringResource(
        if (connecting) R.string.new_agent_host_connecting else R.string.new_agent_host_offline,
    )
    HostReachGroup.OnThisNetwork, HostReachGroup.ThroughTheRelay -> {
        val route = stringResource(
            if (host.reachGroup() == HostReachGroup.OnThisNetwork) {
                R.string.new_agent_host_on_network
            } else {
                R.string.new_agent_host_over_internet
            },
        )
        listOfNotNull(host.platform?.label, route).joinToString(" · ")
    }
}
