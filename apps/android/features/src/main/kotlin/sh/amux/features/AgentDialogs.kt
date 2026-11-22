package sh.amux.features

import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.heading
import sh.amux.design.Backdrop
import sh.amux.design.ProvideAmuxBackdrop
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialogDefaults
import androidx.compose.material3.BasicAlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxTheme
import sh.amux.design.LocalAmuxPhotographed
import sh.amux.design.Space
import sh.amux.values.AgentNameProblem
import sh.amux.values.DeleteWarning
import sh.amux.values.JobRow
import sh.amux.words.ChatWords
import sh.amux.words.DeleteWords

/**
 * The test tags one screen gives its agent dialogs. The chat and the agents
 * list ask the same questions, but each keeps the names its own tests and
 * journeys already drive.
 */
@Immutable
data class AgentDialogTags(
    val renameDialog: String,
    val renameField: String,
    val renameProblem: String,
    val renameConfirm: String,
    val settleDialog: String,
    val settleConfirm: String,
    val deleteDialog: String,
    val deleteConfirm: String,
) {
    companion object {
        val Chat = AgentDialogTags(
            renameDialog = ChatScreenTags.RENAME_DIALOG,
            renameField = ChatScreenTags.RENAME_INPUT,
            renameProblem = ChatScreenTags.RENAME_PROBLEM,
            renameConfirm = ChatScreenTags.RENAME_CONFIRM,
            settleDialog = ChatScreenTags.SETTLE_DIALOG,
            settleConfirm = ChatScreenTags.SETTLE_CONFIRM,
            deleteDialog = ChatScreenTags.DELETE_DIALOG,
            deleteConfirm = ChatScreenTags.DELETE_CONFIRM,
        )

        val Home = AgentDialogTags(
            renameDialog = TestTags.HOME_RENAME_DIALOG,
            renameField = TestTags.HOME_RENAME_INPUT,
            renameProblem = TestTags.HOME_RENAME_PROBLEM,
            renameConfirm = TestTags.HOME_RENAME_CONFIRM,
            settleDialog = TestTags.HOME_SETTLE_DIALOG,
            settleConfirm = TestTags.HOME_SETTLE_CONFIRM,
            deleteDialog = TestTags.HOME_DELETE_DIALOG,
            deleteConfirm = TestTags.HOME_DELETE_CONFIRM,
        )
    }
}

/**
 * Renaming an agent. The name is checked as it is typed, by the same rule
 * the host applies, so a name it would refuse cannot be sent.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RenameAgentDialog(
    currentName: String,
    validateName: (String) -> AgentNameProblem?,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
    tags: AgentDialogTags = AgentDialogTags.Chat,
    initialDraft: String = currentName,
) {
    AgentDialog(onDismiss = onDismiss) {
        RenameAgentCard(
            currentName = currentName,
            validateName = validateName,
            onDismiss = onDismiss,
            onConfirm = onConfirm,
            tags = tags,
            initialDraft = initialDraft,
        )
    }
}

/**
 * Settling an agent that is in the middle of something. Settling is always
 * immediate, so what it cuts short is named first; an agent that is doing
 * nothing settles without asking.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettleAgentDialog(
    agentName: String,
    cuts: String,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
    tags: AgentDialogTags = AgentDialogTags.Chat,
) {
    AgentDialog(onDismiss = onDismiss) {
        SettleAgentCard(
            agentName = agentName,
            cuts = cuts,
            onDismiss = onDismiss,
            onConfirm = onConfirm,
            tags = tags,
        )
    }
}

/**
 * Deleting a settled agent, with what goes and what stays named first.
 *
 * When its workspace goes too, the dialog opens saying what the row said and
 * asks the host through [onPreviewDelete] what is uncommitted there. The
 * dialog keeps what it drew for as long as it is open, so a fleet change
 * meanwhile changes neither what it says nor what confirming agrees to:
 * confirming loses uncommitted changes only when the dialog named them.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeleteAgentDialog(
    agentName: String,
    descendants: Int,
    onPreviewDelete: (suspend () -> Result<DeleteWarning?>)?,
    onDismiss: () -> Unit,
    onConfirm: (discardingChanges: Boolean) -> Unit,
    tags: AgentDialogTags = AgentDialogTags.Chat,
) {
    // Opens checking with the agent's host what the delete takes with it.
    var warning by remember { mutableStateOf<DeleteWarning?>(DeleteWarning.Checking) }
    LaunchedEffect(Unit) {
        if (DeleteWords.awaitsAnswer(warning) && onPreviewDelete != null) {
            warning = DeleteWords.answered(warning, onPreviewDelete())
        }
    }
    AgentDialog(onDismiss = onDismiss) {
        DeleteAgentCard(
            agentName = agentName,
            descendants = descendants,
            warning = warning,
            onDismiss = onDismiss,
            onConfirm = onConfirm,
            tags = tags,
        )
    }
}

/** The rename dialog's card, drawn by [RenameAgentDialog] and by stories over a screen. */
@Composable
fun RenameAgentCard(
    currentName: String,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
    modifier: Modifier = Modifier,
    validateName: (String) -> AgentNameProblem? = ::defaultValidateAgentName,
    tags: AgentDialogTags = AgentDialogTags.Chat,
    initialDraft: String = currentName,
) {
    val colors = AmuxTheme.colors
    var draft by rememberSaveable(currentName) { mutableStateOf(initialDraft) }
    val trimmed = draft.trim()
    val problem = remember(trimmed) { validateName(trimmed) }
    val focus = remember { FocusRequester() }
    val photographed = LocalAmuxPhotographed.current
    val confirm = { if (problem == null) onConfirm(trimmed) }

    LaunchedEffect(focus) {
        // A picture is taken unfocused, without a blinking caret or keyboard.
        if (!photographed) focus.requestFocus()
    }

    AgentDialogCard(
        title = stringResource(R.string.agent_rename_title, currentName),
        modifier = modifier.testTag(tags.renameDialog),
        buttons = {
            DialogTextButton(label = stringResource(R.string.action_cancel), onClick = onDismiss)
            DialogTextButton(
                label = stringResource(R.string.agent_rename_confirm),
                emphasis = true,
                enabled = problem == null,
                onClick = confirm,
                modifier = Modifier.testTag(tags.renameConfirm),
            )
        },
    ) {
        OutlinedTextField(
            value = draft,
            onValueChange = { draft = it },
            singleLine = true,
            label = { Text(text = stringResource(R.string.agent_rename_label)) },
            textStyle = AmuxTheme.typography.mono.copy(color = colors.ink),
            isError = problem != null && trimmed.isNotEmpty(),
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                imeAction = ImeAction.Done,
            ),
            keyboardActions = KeyboardActions(onDone = { confirm() }),
            // A caret blinks forever; a picture or a test of the dialog
            // would never see it settle, so a photographed field draws none.
            colors = if (photographed) {
                amuxTextFieldColors().copy(cursorColor = Color.Unspecified, errorCursorColor = Color.Unspecified)
            } else {
                amuxTextFieldColors()
            },
            modifier = Modifier
                .fillMaxWidth()
                .focusRequester(focus)
                .testTag(tags.renameField),
        )
        if (problem != null && trimmed.isNotEmpty()) {
            Text(
                text = ChatWords.nameProblem(problem),
                style = AmuxTheme.typography.detail,
                color = colors.accent,
                modifier = Modifier.testTag(tags.renameProblem),
            )
        }
    }
}

/** The settle dialog's card, drawn by [SettleAgentDialog] and by stories over a screen. */
@Composable
fun SettleAgentCard(
    agentName: String,
    cuts: String,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
    modifier: Modifier = Modifier,
    tags: AgentDialogTags = AgentDialogTags.Chat,
) {
    val colors = AmuxTheme.colors
    AgentDialogCard(
        title = stringResource(R.string.agent_settle_title, agentName),
        modifier = modifier.testTag(tags.settleDialog),
        buttons = {
            DialogTextButton(label = stringResource(R.string.action_cancel), onClick = onDismiss)
            DialogTextButton(
                label = stringResource(R.string.agent_settle_confirm),
                emphasis = true,
                destructive = true,
                onClick = onConfirm,
                modifier = Modifier.testTag(tags.settleConfirm),
            )
        },
    ) {
        Text(
            text = cuts,
            style = AmuxTheme.typography.detail,
            color = colors.ink,
        )
        ConsequenceRow(kept = false, text = stringResource(R.string.agent_settle_stops_now))
        ConsequenceRow(kept = true, text = stringResource(R.string.agent_settle_keeps))
    }
}

/** The delete dialog's card, drawn by [DeleteAgentDialog] and by stories over a screen. */
@Composable
fun DeleteAgentCard(
    agentName: String,
    onDismiss: () -> Unit,
    onConfirm: (discardingChanges: Boolean) -> Unit,
    modifier: Modifier = Modifier,
    descendants: Int = 0,
    warning: DeleteWarning? = null,
    tags: AgentDialogTags = AgentDialogTags.Chat,
) {
    val lines = DeleteWords.lines(warning)
    AgentDialogCard(
        title = stringResource(R.string.agent_delete_title, agentName),
        modifier = modifier.testTag(tags.deleteDialog),
        buttons = {
            DialogTextButton(label = stringResource(R.string.action_cancel), onClick = onDismiss)
            DialogTextButton(
                label = stringResource(R.string.agent_delete_confirm),
                emphasis = true,
                destructive = true,
                enabled = !DeleteWords.awaitsAnswer(warning),
                onClick = { onConfirm(DeleteWords.discards(warning)) },
                modifier = Modifier.testTag(tags.deleteConfirm),
            )
        },
    ) {
        if (lines == null) {
            ConsequenceRow(kept = true, text = stringResource(R.string.agent_delete_edits_stay))
        }
        ConsequenceRow(kept = false, text = stringResource(R.string.agent_delete_chat_goes))
        when {
            descendants == 1 -> ConsequenceRow(
                kept = false,
                text = stringResource(R.string.agent_delete_subagent_goes),
            )
            descendants > 1 -> ConsequenceRow(
                kept = false,
                text = stringResource(R.string.agent_delete_subagents_go, descendants),
            )
        }
        if (lines != null) {
            // Until the host answers, nothing is said to go but what certainly
            // does, so the workspace line is not yet marked as a loss.
            ConsequenceRow(kept = false, pending = !lines.settled, text = lines.workspace)
            lines.branch?.let { branch ->
                ConsequenceRow(kept = lines.branchKept, text = branch)
            }
        }
    }
}

/**
 * Asks before stopping one of an agent's background jobs, naming it; the
 * agent keeps running either way.
 */
@Composable
fun StopJobDialog(
    job: JobRow,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    AgentDialog(onDismiss = onDismiss) {
        StopJobCard(job = job, onDismiss = onDismiss, onConfirm = onConfirm)
    }
}

/** The stop-job dialog's card, drawn by [StopJobDialog] and by stories over a screen. */
@Composable
fun StopJobCard(
    job: JobRow,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
    modifier: Modifier = Modifier,
) {
    AgentDialogCard(
        title = ChatWords.stopJob(job),
        modifier = modifier.testTag(AskReviewTags.OVERVIEW_STOP_DIALOG),
        buttons = {
            DialogTextButton(
                label = stringResource(R.string.action_cancel),
                onClick = onDismiss,
                modifier = Modifier.testTag(AskReviewTags.OVERVIEW_STOP_CANCEL),
            )
            DialogTextButton(
                label = stringResource(R.string.overview_stop),
                emphasis = true,
                destructive = true,
                onClick = onConfirm,
                modifier = Modifier.testTag(AskReviewTags.OVERVIEW_STOP_CONFIRM),
            )
        },
    ) {
        Text(
            text = ChatWords.STOP_JOB_LEAVES,
            style = AmuxTheme.typography.detail,
            color = AmuxTheme.colors.inkMuted,
        )
    }
}

/**
 * An alert dialog's card: Material's alert shape, padding and button row,
 * on the raised surface. Shown through [BasicAlertDialog], which gives it
 * the window, scrim, width and dismissal of an `AlertDialog`; kept apart
 * from that window so the card can also be drawn inside a single picture.
 */
/**
 * The window an agent dialog's card sits in. The card sets its own width
 * (280 to 560 dp, inset from the screen's edges) rather than the platform's
 * default dialog width: a window sized to its content while the content is
 * sized to the window measures again on every pass and never settles.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AgentDialog(onDismiss: () -> Unit, content: @Composable () -> Unit) {
    BasicAlertDialog(
        onDismissRequest = onDismiss,
        modifier = Modifier.padding(horizontal = Space.xl),
        properties = DialogProperties(usePlatformDefaultWidth = false),
        content = content,
    )
}

@Composable
private fun AgentDialogCard(
    title: String,
    modifier: Modifier = Modifier,
    buttons: @Composable () -> Unit,
    body: @Composable ColumnScope.() -> Unit,
) {
    val colors = AmuxTheme.colors
    Surface(
        shape = AlertDialogDefaults.shape,
        color = colors.raised,
        contentColor = colors.ink,
        tonalElevation = AlertDialogDefaults.TonalElevation,
        modifier = modifier.widthIn(min = 280.dp, max = 560.dp),
    ) {
        ProvideAmuxBackdrop(Backdrop.Raised) {
        Column(modifier = Modifier.padding(Space.xl)) {
            // The title heads the dialog's window, so TalkBack's heading
            // navigation lands on it first.
            Text(
                text = title,
                style = AmuxTheme.typography.identifier,
                color = colors.ink,
                modifier = Modifier.semantics { heading() },
            )
            Spacer(modifier = Modifier.height(Space.l))
            Column(verticalArrangement = Arrangement.spacedBy(Space.s), content = body)
            Spacer(modifier = Modifier.height(Space.xl))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(Space.s, Alignment.End),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                buttons()
            }
        }
        }
    }
}

@Composable
private fun DialogTextButton(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    emphasis: Boolean = false,
    destructive: Boolean = false,
    enabled: Boolean = true,
) {
    val colors = AmuxTheme.colors
    TextButton(
        onClick = onClick,
        enabled = enabled,
        modifier = modifier,
    ) {
        Text(
            text = label,
            style = if (emphasis) AmuxTheme.typography.bodyEmphasis else AmuxTheme.typography.body,
            color = when {
                !enabled -> colors.inkFaint
                destructive -> colors.removed
                emphasis -> colors.ink
                else -> colors.inkMuted
            },
        )
    }
}
