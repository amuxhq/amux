package sh.amux.testsupport

import sh.amux.design.Backdrop
import sh.amux.design.ProvideAmuxBackdrop
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.BottomSheetDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import java.io.ByteArrayOutputStream
import sh.amux.design.AmuxScrim
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.features.NetworkSight
import sh.amux.features.AgentsHome
import sh.amux.features.AgentsHomeState
import sh.amux.features.AskReviewTags
import sh.amux.features.ElapsedClock
import sh.amux.features.ChatOverviewSheetContent
import sh.amux.features.ChatOverviewState
import sh.amux.features.StopJobCard
import sh.amux.features.ChatScreen
import sh.amux.features.ChatScreenState
import sh.amux.features.ChatSettingsSheetContent
import sh.amux.features.ChatSettingsState
import sh.amux.features.ChatSubject
import sh.amux.features.ChatPlaceholderScreen
import sh.amux.features.DeleteAgentCard
import sh.amux.features.DeleteAccountPhaseUi
import sh.amux.features.DeleteAccountSheetContent
import sh.amux.features.DeleteAccountSheetState
import sh.amux.features.HostPage
import sh.amux.features.HostPageState
import sh.amux.features.HomeScope
import sh.amux.features.HostsTab
import sh.amux.features.HostsTabState
import sh.amux.features.NewAgentDirectoriesContent
import sh.amux.features.NewAgentListingUi
import sh.amux.features.NewAgentOfferUi
import sh.amux.features.NewAgentScreen
import sh.amux.features.NewAgentScreenState
import sh.amux.features.PairingPhaseUi
import sh.amux.features.PairingSheetContent
import sh.amux.features.PairingSheetState
import sh.amux.features.PaywallPhaseUi
import sh.amux.features.PaywallPlanPeriod
import sh.amux.features.PaywallPlanUi
import sh.amux.features.PaywallScreen
import sh.amux.features.PaywallScreenState
import sh.amux.features.QuestionDraft
import sh.amux.features.RenameAgentCard
import sh.amux.features.ReportDeliveryUi
import sh.amux.features.ReportMarkUi
import sh.amux.features.ReportOverlayContent
import sh.amux.features.ReportUiState
import sh.amux.features.SettingsAccountItem
import sh.amux.features.SettingsAppearance
import sh.amux.features.SettingsEntitlementBadge
import sh.amux.features.SettingsNotificationStatus
import sh.amux.features.SettingsScreen
import sh.amux.features.SettingsScreenState
import sh.amux.features.StaticChatRowSource
import sh.amux.features.StoreFailureScreen
import sh.amux.values.ActivityKind
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.CardState
import sh.amux.values.ChatFrame
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ExitCause
import sh.amux.values.FamilyHeader
import sh.amux.values.FileStatus
import sh.amux.values.Filing
import sh.amux.values.GroupState
import sh.amux.values.JobStop
import sh.amux.values.Kind
import sh.amux.values.LineKind
import sh.amux.values.OpenedAgent
import sh.amux.values.PhaseView
import sh.amux.values.Provider
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.Scope
import sh.amux.values.SettingsView
import sh.amux.values.Waiting
import sh.amux.features.ReviewScreen
import sh.amux.features.ReviewScreenState
import sh.amux.features.ReviewCommentSheetContent
import sh.amux.features.ReviewRoundContent
import sh.amux.features.ReviewPlaceholderScreen
import sh.amux.core.runtime.ReviewSession
import sh.amux.values.ReviewBanner
import sh.amux.values.ReviewChip
import sh.amux.values.ReviewPage
import sh.amux.words.ChatWords
import sh.amux.words.EMPTY_GROUP_STATE

/**
 * One named storybook screen fixture rendered by both headless Roborazzi
 * captures (`just android storybook`) and the debug-only `StorybookActivity`.
 *
 * [taps] are test tags tapped in order once the story is drawn and before it
 * is pictured, for a state only a person's taps reach, such as a denial's
 * note being written; the interactive storybook leaves them to the person.
 */
data class Story(
    val id: String,
    val area: String,
    val title: String,
    val description: String,
    val fontScale: Float = 1.0f,
    val taps: List<String> = emptyList(),
    val content: @Composable () -> Unit,
)

/**
 * Central registry of Android storybook stories.
 *
 * Every phone story in `tests/stories.json` is here under its name. The
 * others are the iOS catalogue's examples under the iOS ids (an ask card in
 * each of its states, say), so each Android picture has an iOS picture of the
 * same name to be compared with. Both draw the iOS catalogue's own data:
 * [CatalogFixtures] and [StoryFleet].
 */
object Stories {
    private const val NOW_MS: Long = CatalogFixtures.NOW_MS

    /** Every chat story is drawn at the catalogue's one moment. */
    private val STORY_CLOCK = ElapsedClock { NOW_MS }

    /**
     * Android's largest text size. Large-text stories draw at it, as iOS's
     * draw at an accessibility size.
     */
    const val LARGEST_FONT_SCALE: Float = 2.0f

    /**
     * Stories listed under `"clients": [..., "phone", ...]` in `tests/stories.json`
     * that Android does not draw yet, along with why.
     */
    val IOS_ONLY_STORIES: Map<String, String> = mapOf(
        "hosts.network-refused" to "Android asks no permission to look on the network; a phone that cannot look says so on the first run (fleet.first-run-refused).",
    )

    private val F = CatalogFixtures

    val all: List<Story> = listOf(
        // --- Fleet ---------------------------------------------------------
        Story(
            id = "fleet.busy",
            area = "Fleet",
            title = "Busy fleet",
            description = "A day's agents across machines, one away: pinned, needs you, working, idle, a family, and settled.",
        ) {
            AgentsHome(state = StoryFleet.busy(away = true), onAction = {})
        },
        Story(
            id = "fleet.notifications-offer",
            area = "Fleet",
            title = "Busy fleet offering notifications",
            description = "The busy list with the offer to turn on notifications above it.",
        ) {
            AgentsHome(state = StoryFleet.busy(nearby = false).copy(offerNotifications = true), onAction = {})
        },
        Story(
            id = "fleet.subagents",
            area = "Fleet",
            title = "Expanded subagent family",
            description = "A family opened to its Claude subagents, listed under their parent by the titles Claude gave them, one waiting on you.",
        ) {
            AgentsHome(state = StoryFleet.busy(subagents = true), onAction = {})
        },
        Story(
            id = "fleet.signed-out",
            area = "Fleet",
            title = "Busy fleet (signed out)",
            description = "Busy agent list on a phone with no account signed in.",
        ) {
            AgentsHome(state = StoryFleet.busy(accounts = 0), onAction = {})
        },
        Story(
            id = "fleet.settled",
            area = "Fleet",
            title = "Settled agents",
            description = "The busy fleet's settled agents on a page of their own, with the way back.",
        ) {
            AgentsHome(state = StoryFleet.busy().copy(scope = HomeScope.Closed), onAction = {})
        },
        Story(
            id = "fleet.no-agents",
            area = "Fleet",
            title = "No agents yet",
            description = "Paired with a computer that has no agents running yet.",
        ) {
            AgentsHome(state = StoryFleet.noAgents(), onAction = {})
        },
        Story(
            id = "fleet.first-run",
            area = "Fleet",
            title = "First run (computer found)",
            description = "No computers paired yet, with one nearby computer discovered on the network.",
        ) {
            AgentsHome(state = StoryFleet.firstRun(), onAction = {})
        },
        Story(
            id = "fleet.first-run-two-found",
            area = "Fleet",
            title = "First run (two found)",
            description = "No computers paired yet, with two computers discovered on this network.",
        ) {
            AgentsHome(state = StoryFleet.firstRun(found = listOf(StoryFleet.laptop, StoryFleet.studioFound)), onAction = {})
        },
        Story(
            id = "fleet.first-run-window-open",
            area = "Fleet",
            title = "First run (pairing window open)",
            description = "Nearby computer advertising an open pairing window ready for one-tap pairing.",
        ) {
            AgentsHome(state = StoryFleet.firstRun(found = listOf(StoryFleet.laptopOpen)), onAction = {})
        },
        Story(
            id = "fleet.first-run-looking",
            area = "Fleet",
            title = "First run (looking)",
            description = "Nothing paired and nothing found yet: looking on this network with two setup steps.",
        ) {
            AgentsHome(state = StoryFleet.firstRun(found = emptyList()), onAction = {})
        },
        Story(
            id = "fleet.first-run-refused",
            area = "Fleet",
            title = "First run (can't see this network)",
            description = "Nothing paired on a phone with no Wi-Fi: it says it cannot look, and how to put that right.",
        ) {
            AgentsHome(
                state = StoryFleet.firstRun(found = emptyList()).copy(networkSight = NetworkSight.NoLocalNetwork),
                onAction = {},
            )
        },
        Story(
            id = "fleet.first-run-signed-out",
            area = "Fleet",
            title = "First run (signed out)",
            description = "Nothing paired on a phone with no account, offering Sign In below pairing steps.",
        ) {
            AgentsHome(state = StoryFleet.firstRun(found = emptyList(), accounts = 0), onAction = {})
        },
        Story(
            id = "fleet.first-run-signed-out-found",
            area = "Fleet",
            title = "First run (signed out, computer found)",
            description = "Nothing paired on a phone with no account, with one nearby computer discovered.",
        ) {
            AgentsHome(state = StoryFleet.firstRun(accounts = 0), onAction = {})
        },
        Story(
            id = "fleet.first-run-on-the-account-free",
            area = "Fleet",
            title = "First run (relay host on free plan)",
            description = "Nothing paired on the free plan, with a computer on the account reachable only through the relay.",
        ) {
            AgentsHome(state = StoryFleet.firstRun(found = listOf(StoryFleet.onTheAccount), pro = false), onAction = {})
        },
        Story(
            id = "fleet.filter",
            area = "Fleet",
            title = "Filtered fleet",
            description = "Search bar expanded and list narrowed to the agents whose step runs the tests.",
        ) {
            AgentsHome(state = filteredFleet(), searchExpanded = true, onAction = {})
        },
        Story(
            id = "fleet.no-matches",
            area = "Fleet",
            title = "No matching agents",
            description = "Search narrowed to zero matches with Clear action.",
        ) {
            val base = StoryFleet.busy()
            AgentsHome(
                state = base.copy(
                    rows = emptyList(),
                    sections = emptyList(),
                    filter = sh.amux.values.FleetFilter(words = "kubernetes", only = null),
                ),
                searchExpanded = true,
                onAction = {},
            )
        },
        Story(
            id = "fleet.large-text",
            area = "Fleet",
            title = "Busy fleet (large text)",
            description = "Busy agent list at Android's largest text size.",
            fontScale = LARGEST_FONT_SCALE,
        ) {
            AgentsHome(state = StoryFleet.busy(), onAction = {})
        },
        Story(
            id = "fleet.two-accounts",
            area = "Fleet",
            title = "Two accounts",
            description = "Busy fleet on a phone signed into two accounts.",
        ) {
            AgentsHome(state = StoryFleet.busy(accounts = 2), onAction = {})
        },

        // --- New Agent -----------------------------------------------------
        Story(
            id = "new-agent.claude",
            area = "New Agent",
            title = "New Claude agent",
            description = "Starting a Claude agent: host, folder, name, model, effort, and permission.",
        ) {
            NewAgentScreen(state = newAgentState(), onAction = {})
        },
        Story(
            id = "new-agent.browsing",
            area = "New Agent",
            title = "New Agent folder browser",
            description = "Choosing the folder a new agent starts in: recent folders and repositories, over the form.",
        ) {
            val state = newAgentState()
            SheetStoryFrame(background = { NewAgentScreen(state = state, onAction = {}) }) {
                NewAgentDirectoriesContent(state = state.copy(browsing = true), onAction = {})
            }
        },
        Story(
            id = "new-agent.large-text",
            area = "New Agent",
            title = "New Agent (large text)",
            description = "New Agent form at Android's largest text size with stacked values and sticky Start bar.",
            fontScale = LARGEST_FONT_SCALE,
        ) {
            NewAgentScreen(state = newAgentState(), onAction = {})
        },

        // --- Chat ----------------------------------------------------------
        Story(
            id = "chat.conversation",
            area = "Chat",
            title = "Conversation",
            description = "A finished turn: prompt, one run of steps (a failed check fixed, a lint left failing, a refused command) and the answer.",
        ) {
            StoryChatScreen(conversationChat())
        },
        Story(
            id = "chat.working",
            area = "Chat",
            title = "Working turn",
            description = "The finished turn, then a second one five steps in with its newest step running.",
        ) {
            StoryChatScreen(workingChat())
        },
        Story(
            id = "chat.asking",
            area = "Chat",
            title = "Agent asking",
            description = "The conversation with its newest command asking to run, and the ask docked in the composer's place.",
        ) {
            StoryChatScreen(
                chat(
                    rows = F.conversation + F.askedCommand,
                    frame = ScriptedChat.frame(phase = PhaseView.NeedsYou),
                    card = F.card(Kind.ClaudeSdk, F.specSuite, F.sdkPermissionChoices),
                ),
            )
        },
        Story(
            id = "chat.subagents",
            area = "Chat",
            title = "Subagents group",
            description = "A turn that started four subagents at once: one row saying 2 running · 1 waiting on you · 1 done.",
        ) {
            StoryChatScreen(delegatingChat(open = false))
        },
        Story(
            id = "chat.subagents-open",
            area = "Chat",
            title = "Subagents group (open)",
            description = "The subagents group opened to its four calls, side by side at one level.",
        ) {
            StoryChatScreen(delegatingChat(open = true))
        },
        Story(
            id = "chat.subagent",
            area = "Chat",
            title = "Provider subagent",
            description = "A Claude subagent's own chat: its task, its steps and answer so far, and a read-only composer with Stop.",
        ) {
            StoryChatScreen(
                chat(
                    rows = F.subagentWork,
                    frame = F.nativeFrame(JobStop.Offered("toolu_02")),
                    subject = F.nativeSubject,
                    family = F.nativeFamily,
                ),
            )
        },
        Story(
            id = "chat.plan",
            area = "Chat",
            title = "Plan approval",
            description = "A Codex plan read whole in the transcript and waiting for approval in place of the composer.",
        ) {
            StoryChatScreen(
                chat(
                    rows = F.planned,
                    frame = ScriptedChat.frame(kind = Kind.Codex, phase = PhaseView.NeedsYou),
                    card = F.card(Kind.Codex, F.plan, F.codexPlanChoices),
                ),
            )
        },
        Story(
            id = "chat.exited",
            area = "Chat",
            title = "Exited agent",
            description = "The first agent's chat after it exited while amux was stopped, ready to resume.",
        ) {
            StoryChatScreen(exitedChat())
        },
        Story(
            id = "chat.away",
            area = "Chat",
            title = "Host away with kept draft",
            description = "Away host showing out-of-reach notice, kept draft text, and disabled Send.",
        ) {
            StoryChatScreen(
                chat(
                    rows = emptyList(),
                    frame = ScriptedChat.frame(mode = Composer.Waiting(Waiting.Detached), caughtUp = false),
                    subject = F.awaySubject,
                    draft = "Now run the Windows check.",
                ),
            )
        },
        Story(
            id = "chat.overview",
            area = "Chat",
            title = "Chat overview sheet",
            description = "Overview sheet over the chat: two background jobs, a tool server to sign in to, three usage windows and the changed files.",
        ) {
            val chat = overviewChat()
            SheetStoryFrame(background = { StoryChatScreen(chat) }) {
                ChatOverviewSheetContent(state = overviewOf(chat.state), onAction = {})
            }
        },
        Story(
            id = "chat.overview-stop",
            area = "Chat",
            title = "Stop background job dialog",
            description = "Confirming stopping a background job from overview while keeping the agent running.",
        ) {
            val chat = overviewChat()
            val job = F.surroundings.overview.jobs.first()
            DialogStoryFrame(
                background = {
                    SheetStoryFrame(background = { StoryChatScreen(chat) }) {
                        ChatOverviewSheetContent(state = overviewOf(chat.state), onAction = {})
                    }
                },
            ) {
                StopJobCard(job = job, onConfirm = {}, onDismiss = {})
            }
        },
        Story(
            id = "chat.settings",
            area = "Chat",
            title = "Chat settings sheet",
            description = "Terminal Claude's settings over its chat: the model and effort changed by typing, the permission by cycling.",
        ) {
            val chat = chat(
                rows = F.conversation,
                frame = ScriptedChat.frame(kind = Kind.ClaudePty),
                strip = ScriptedChat.strip(model = "claude-sonnet-5", effort = "high", mode = "acceptEdits"),
                settings = F.settingsClaudePty,
            )
            SheetStoryFrame(background = { StoryChatScreen(chat) }) {
                ChatSettingsSheetContent(
                    state = ChatSettingsState(
                        settings = F.settingsClaudePty,
                        kind = chat.state.frame?.kind,
                        context = chat.state.frame?.context,
                    ),
                    onChange = {},
                    onDismiss = {},
                    onSelectCommand = {},
                )
            }
        },
        Story(
            id = "chat.loading",
            area = "Chat",
            title = "Loading conversation",
            description = "Opening conversation before initial transcript rows have arrived.",
        ) {
            StoryChatScreen(
                chat(
                    rows = emptyList(),
                    frame = ScriptedChat.frame(mode = Composer.Waiting(Waiting.CatchingUp), caughtUp = false),
                    loadingHint = true,
                ),
            )
        },
        Story(
            id = "chat.not-opened",
            area = "Chat",
            title = "Chat could not be opened",
            description = "Unavailable conversation screen explaining why the chat could not be opened.",
        ) {
            ChatPlaceholderScreen(title = F.subject.name, failure = "desk did not answer", onBack = {})
        },
        Story(
            id = "chat.opening",
            area = "Chat",
            title = "Opening a chat",
            description = "The chat's page while its conversation is still opening.",
        ) {
            ChatPlaceholderScreen(title = F.subject.name, failure = null, onBack = {})
        },
        Story(
            id = "chat.queued",
            area = "Chat",
            title = "Queued prompts",
            description = "Working turn with a withdrawable prompt, a steering one, and one from another agent queued above the composer.",
        ) {
            StoryChatScreen(workingChat(queue = true))
        },
        Story(
            id = "chat.dock",
            area = "Chat",
            title = "Dock above the composer",
            description = "A working parent's dock folded: its task list's head, the agents it started, two jobs in the background and a queued prompt.",
        ) {
            StoryChatScreen(dockChat(open = false))
        },
        Story(
            id = "chat.dock-open",
            area = "Chat",
            title = "Dock opened",
            description = "The dock opened in place: the task list, the agents this one started, then the head and a queued prompt.",
        ) {
            StoryChatScreen(dockChat(open = true))
        },
        Story(
            id = "chat.signed-out",
            area = "Chat",
            title = "Provider signed out",
            description = "The conversation with the provider's refused sign-in in the composer's place.",
        ) {
            StoryChatScreen(
                chat(
                    rows = F.conversation,
                    frame = ScriptedChat.frame(phase = PhaseView.Idle),
                    strip = ScriptedChat.strip(signIn = F.failedSignIn),
                ),
            )
        },
        Story(
            id = "chat.delete",
            area = "Chat",
            title = "Delete agent confirmation",
            description = "Confirming deletion of a settled agent from its chat.",
        ) {
            val chat = chat(
                rows = F.conversation,
                frame = ScriptedChat.frame(phase = PhaseView.Exited(ExitCause.Ended), mode = Composer.Resume),
                subject = F.closedSubject,
                canResume = true,
            )
            DialogStoryFrame(background = { StoryChatScreen(chat) }) {
                DeleteAgentCard(
                    agentName = F.closedSubject.name,
                    descendants = 0,
                    warning = null,
                    onDismiss = {},
                    onConfirm = {},
                )
            }
        },
        Story(
            id = "chat.rename",
            area = "Chat",
            title = "Rename agent dialog",
            description = "Renaming an agent from its chat with live handle editing.",
        ) {
            val chat = chat(rows = F.conversation, frame = ScriptedChat.frame())
            DialogStoryFrame(background = { StoryChatScreen(chat) }) {
                RenameAgentCard(currentName = F.subject.name, onDismiss = {}, onConfirm = {})
            }
        },
        Story(
            id = "chat.large-text",
            area = "Chat",
            title = "Active conversation (large text)",
            description = "The conversation at Android's largest text size.",
            fontScale = LARGEST_FONT_SCALE,
        ) {
            StoryChatScreen(conversationChat())
        },

        // --- Asks ----------------------------------------------------------
        // The iOS catalogue's ask examples, by its ids. iOS draws each card on
        // its own; here each is docked over a chat, where the app draws it.
        askStory("permission-command", "Command permission", F.card(Kind.ClaudeSdk, F.specSuite, F.sdkPermissionChoices)),
        askStory(
            "permission-deny-note",
            "Denying with a note",
            F.card(
                Kind.ClaudeSdk,
                AskBody.Command(command = "rm -rf target", cwd = "~/src/amux", reason = "", description = "Clean the build"),
                F.sdkPermissionChoices,
            ),
            taps = listOf(AskReviewTags.ASK_DENY),
        ),
        askStory(
            "permission-edit",
            "Edit permission",
            F.card(
                Kind.ClaudeSdk,
                AskBody.Edit(
                    path = "crates/amux-ui/src/pairing.rs",
                    files = 1,
                    added = 1,
                    removed = 2,
                    patch = F.diff,
                    reason = "",
                    created = false,
                    line = 13,
                ),
                listOf(
                    Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
                    Choice(ChoiceOutcome.AllowAlways(emptyList(), emptyList(), "acceptEdits", "Accept edits", Scope.Session), primary = false, takesNote = false),
                    Choice(ChoiceOutcome.Deny, primary = false, takesNote = true),
                ),
            ),
        ),
        askStory(
            "permission-create",
            "New file permission",
            F.card(
                Kind.ClaudeSdk,
                AskBody.Edit(
                    path = "crates/amux-ui/src/pairing/refusals/every_reason_a_pairing_is_refused.rs",
                    files = 1,
                    added = 2,
                    removed = 0,
                    patch = F.created,
                    reason = "",
                    created = true,
                    line = 1,
                ),
                F.allowOrDeny,
            ),
        ),
        askStory(
            "permission-tool",
            "Tool permission",
            F.card(
                Kind.ClaudeSdk,
                AskBody.Tool(
                    server = "github",
                    tool = "create_issue",
                    arguments = "{\n  \"repo\": \"jlw/amux\",\n  \"title\": \"Pairing copy\"\n}",
                ),
                F.allowOrDeny,
            ),
        ),
        askStory(
            "permission-terminal",
            "Terminal Claude command permission",
            F.card(
                Kind.ClaudePty,
                F.bareSpecSuite,
                listOf(
                    Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
                    Choice(ChoiceOutcome.AllowAlways(listOf("cargo test"), emptyList(), "", "", Scope.Project), primary = false, takesNote = false),
                    Choice(ChoiceOutcome.Deny, primary = false, takesNote = false),
                ),
            ),
        ),
        askStory("codex-command", "Codex command permission", F.card(Kind.Codex, F.curlHealth, F.codexCommandChoices, count = 2L)),
        askStory("question-single", "Question", F.card(Kind.ClaudeSdk, AskBody.Question(listOf(F.redactionQuestion)), emptyList())),
        askStory(
            "question-multi",
            "Multi-select question",
            F.card(Kind.ClaudeSdk, AskBody.Question(listOf(F.platformQuestion)), emptyList()),
            draft = questionDraft(1, highlighted = listOf(0)),
        ),
        askStory("questions-step", "Stepped questions", F.card(Kind.Codex, AskBody.Question(F.threeQuestions), emptyList())),
        askStory(
            "questions-review",
            "Reviewing answers",
            F.card(Kind.Codex, AskBody.Question(F.threeQuestions), emptyList()),
            draft = questionDraft(
                3,
                picks = listOf(setOf(0, 1), setOf(1), emptySet()),
                others = listOf(null, null, "Only failures, with the host id"),
                reviewing = true,
            ),
        ),
        askStory(
            "questions-skipped",
            "Reviewing with a question skipped",
            F.card(Kind.Codex, AskBody.Question(F.threeQuestions), emptyList()),
            draft = questionDraft(
                3,
                picks = listOf(setOf(0, 1), emptySet(), emptySet()),
                others = listOf(null, null, "Only failures, with the host id"),
                skipped = listOf(false, true, false),
                reviewing = true,
            ),
        ),
        askStory(
            "question-note",
            "Question with a note",
            F.card(Kind.ClaudeSdk, AskBody.Question(listOf(F.redactionQuestion)), emptyList()),
            draft = questionDraft(1, notes = listOf("Keep the old strings for one release"), noting = listOf(true)),
        ),
        askStory(
            "question-reply",
            "Replying instead",
            F.card(Kind.Codex, AskBody.Question(F.threeQuestions), emptyList()),
            draft = questionDraft(3, replying = true, replyText = "Hold off; I want to read the diff first."),
        ),
        askStory("question-terminal", "Terminal Claude question", F.card(Kind.ClaudePty, AskBody.Question(listOf(F.redactionQuestion)), emptyList())),
        askStory(
            "question-previews",
            "Question with previews",
            F.card(Kind.ClaudeSdk, AskBody.Question(listOf(F.previewQuestion)), emptyList()),
            draft = questionDraft(1, highlighted = listOf(0)),
        ),
        askStory(
            "question-other",
            "Something else typed",
            F.card(Kind.ClaudeSdk, AskBody.Question(listOf(F.redactionQuestion)), emptyList()),
            draft = questionDraft(1, others = listOf("Put it in amux-wire next to the codes")),
        ),
        askStory(
            "question-secret",
            "Secret answer",
            F.card(Kind.Codex, AskBody.Question(listOf(F.secretQuestion)), emptyList()),
            draft = questionDraft(1, others = listOf("hunter2")),
        ),
        askStory("question-code", "Question naming code", F.card(Kind.Codex, AskBody.Question(listOf(F.codeQuestion)), emptyList())),
        askStory(
            "plan",
            "Plan approval",
            F.card(Kind.ClaudeSdk, F.plan, F.claudePlanChoices),
            taps = listOf(AskReviewTags.ASK_PLAN_SWITCH),
        ),
        askStory("plan-codex", "Codex plan approval", F.card(Kind.Codex, F.plan, F.codexPlanChoices)),
        askStory(
            "form",
            "Tool-server form",
            F.card(
                Kind.ClaudeSdk,
                AskBody.Form(server = "github", message = "Create the issue in which repository?", fields = F.formFields),
                listOf(
                    Choice(ChoiceOutcome.Submit, primary = true, takesNote = false),
                    Choice(ChoiceOutcome.Decline, primary = false, takesNote = false),
                ),
            ),
        ),
        askStory(
            "link",
            "Sign-in link",
            F.card(
                Kind.Codex,
                AskBody.Link(server = "linear", message = "Sign in to Linear to continue.", url = "https://linear.app/login", localPage = false),
                listOf(
                    Choice(ChoiceOutcome.OpenLink, primary = true, takesNote = false),
                    Choice(ChoiceOutcome.Decline, primary = false, takesNote = false),
                ),
            ),
        ),
        askStory(
            "access",
            "Access beyond the sandbox",
            F.card(
                Kind.Codex,
                AskBody.Access(
                    reason = "To run the integration tests against a local server.",
                    read = emptyList(),
                    write = listOf("~/src/amux/target"),
                    network = true,
                    hosts = emptyList(),
                ),
                listOf(
                    Choice(ChoiceOutcome.GrantForTurn, primary = true, takesNote = false),
                    Choice(ChoiceOutcome.GrantForSession, primary = false, takesNote = false),
                    Choice(ChoiceOutcome.Decline, primary = false, takesNote = false),
                ),
            ),
        ),
        askStory(
            "sending",
            "Answer sending",
            F.card(Kind.ClaudeSdk, AskBody.Command(command = "cargo test", cwd = "", reason = "", description = ""), emptyList(), state = CardState.Sending),
        ),
        askStory(
            "rejected",
            "Answer refused",
            F.card(Kind.ClaudeSdk, F.specSuite.copy(description = ""), F.sdkPermissionChoices, state = F.closedAsk),
        ),
        askStory("not-confirmed", "Answer not confirmed", F.card(Kind.ClaudeSdk, F.bareSpecSuite, F.sdkPermissionChoices, state = CardState.NotConfirmed)),
        askStory("dismissed", "Ask dismissed", F.card(Kind.ClaudeSdk, F.bareSpecSuite, F.sdkPermissionChoices, state = CardState.Dismissed)),
        askStory("unanswerable", "Unanswerable ask", F.card(Kind.ClaudePty, AskBody.Unanswerable(reason = ""), emptyList())),
        askStory(
            "permission-command-large-text",
            "Command permission (large text)",
            F.card(Kind.ClaudeSdk, F.specSuite, F.sdkPermissionChoices),
            fontScale = LARGEST_FONT_SCALE,
        ),
        askStory(
            "codex-command-large-text",
            "Codex command permission (large text)",
            F.card(Kind.Codex, F.curlHealth, F.codexCommandLargeTextChoices),
            fontScale = LARGEST_FONT_SCALE,
        ),
        askStory(
            "question-large-text",
            "Question (large text)",
            F.card(Kind.ClaudeSdk, AskBody.Question(listOf(F.redactionQuestion)), emptyList()),
            fontScale = LARGEST_FONT_SCALE,
            description = "A question at Android's largest text size: the question, every option and Something else clear of each other.",
        ),
        askStory(
            "plan-large-text",
            "Plan approval (large text)",
            F.card(Kind.ClaudeSdk, F.plan, F.claudePlanChoices),
            fontScale = LARGEST_FONT_SCALE,
        ),
        askStory(
            "not-confirmed-large-text",
            "Answer not confirmed (large text)",
            F.card(Kind.ClaudeSdk, F.bareSpecSuite, F.sdkPermissionChoices, state = CardState.NotConfirmed),
            fontScale = LARGEST_FONT_SCALE,
        ),

        // --- Hosts ---------------------------------------------------------
        Story(
            id = "hosts.list",
            area = "Hosts",
            title = "Paired computers",
            description = "Every paired computer by how it is reached (direct, relay, away, offline, unpaired this phone) with one found nearby.",
        ) {
            HostsTab(state = hostsTabState(F.machines), onAction = {})
        },
        Story(
            id = "hosts.empty",
            area = "Hosts",
            title = "No computers yet",
            description = "Computers tab when nothing is paired or discovered yet.",
        ) {
            HostsTab(state = HostsTabState(nowMs = NOW_MS), onAction = {})
        },
        Story(
            id = "hosts.on-the-account-free",
            area = "Hosts",
            title = "Hosts (relay host on free plan)",
            description = "Computers tab on the free plan showing a computer on the account reachable only through the relay.",
        ) {
            HostsTab(state = hostsTabState(F.machines + F.accountServer, pro = false), onAction = {})
        },
        Story(
            id = "hosts.large-text",
            area = "Hosts",
            title = "Paired computers (large text)",
            description = "Computers list at Android's largest text size, on the free plan so the away computer's card offers Subscribe beside its words.",
            fontScale = LARGEST_FONT_SCALE,
        ) {
            HostsTab(state = hostsTabState(F.machines, pro = false), onAction = {})
        },
        Story(
            id = "host.page",
            area = "Hosts",
            title = "Computer details",
            description = "One computer's page: reach, platform, version, providers, copyable addresses, and key fingerprint.",
        ) {
            HostPage(
                state = HostPageState(
                    host = F.deskHost,
                    fingerprint = F.fingerprint(1),
                    nowMs = NOW_MS,
                ),
                onAction = {},
            )
        },

        // --- Pairing -------------------------------------------------------
        pairingStory(
            "pair.code-empty",
            "Pairing code entry",
            "Six-digit code entry for a computer found on this network, saying where to get the code.",
            PairingSheetState(machine = F.nearbyLaptop, nowMs = NOW_MS),
        ),
        pairingStory(
            "pair.code-offered",
            "Pairing code entry (window open)",
            "Six-digit code entry for a computer whose pairing window is open.",
            PairingSheetState(machine = F.openLaptop, nowMs = NOW_MS),
        ),
        pairingStory(
            "pair.code-typing",
            "Pairing code typing",
            "Three of the six digits typed.",
            PairingSheetState(digits = "418", machine = F.nearbyLaptop, nowMs = NOW_MS),
        ),
        pairingStory(
            "pair.code-checking",
            "Pairing code checking",
            "All six digits typed and the pairing code being checked.",
            PairingSheetState(digits = "418207", phase = PairingPhaseUi.Checking(viaLink = false), machine = F.nearbyLaptop, nowMs = NOW_MS),
        ),
        pairingStory(
            "pair.code-refused",
            "Pairing code refused",
            "Uniform refusal state after an invalid or expired code, ready for another entry.",
            PairingSheetState(phase = PairingPhaseUi.Refused(message = null, viaLink = false), machine = F.nearbyLaptop, nowMs = NOW_MS),
        ),
        pairingStory(
            "pair.code-subscribe",
            "Pairing needs Pro subscription",
            "A pairing code that reached a computer only the relay can carry, on the free plan.",
            PairingSheetState(digits = "418207", phase = PairingPhaseUi.NeedsSubscription, machine = F.relayLaptop, nowMs = NOW_MS),
        ),
        pairingStory(
            "pair.needs-account",
            "Pairing needs sign-in",
            "An invitation naming a computer only the relay can reach on a phone with no account.",
            PairingSheetState(phase = PairingPhaseUi.NeedsAccount(signingIn = false), machine = null, nowMs = NOW_MS),
        ),
        pairingStory(
            "pair.confirm",
            "Pairing confirmation",
            "Authenticated computer name, route, and full key fingerprint awaiting confirmation.",
            PairingSheetState(
                phase = PairingPhaseUi.Confirming(pending = F.pendingLaptop, platformName = "macOS"),
                machine = F.nearbyLaptop,
                nowMs = NOW_MS,
            ),
        ),
        pairingStory(
            "pair.trusted",
            "Pairing completed",
            "Confirmation state once the computer has been paired and trusted.",
            PairingSheetState(phase = PairingPhaseUi.Trusted(name = "laptop"), machine = F.nearbyLaptop, nowMs = NOW_MS),
        ),

        // --- Settings ------------------------------------------------------
        Story(
            id = "settings.signed-in",
            area = "Settings",
            title = "Settings (signed in)",
            description = "Settings with signed-in account, appearance, notifications, privacy, diagnostics, and phone identity.",
        ) {
            SettingsScreen(state = signedInSettings(), onAction = {})
        },
        Story(
            id = "settings.signed-out",
            area = "Settings",
            title = "Settings (signed out)",
            description = "Settings on a phone with no account signed in.",
        ) {
            SettingsScreen(
                state = signedInSettings().copy(
                    accounts = emptyList(),
                    notificationStatus = SettingsNotificationStatus.NotAsked,
                ),
                onAction = {},
            )
        },
        Story(
            id = "settings.paywall",
            area = "Settings",
            title = "Pro subscription",
            description = "Subscribing as this build offers it: Google Play has no plans to sell yet, so the page points to amux.sh.",
        ) {
            PaywallScreen(
                state = PaywallScreenState(
                    phase = PaywallPhaseUi.StoreUnavailable(),
                    accountEmail = "ada@example.com",
                ),
                onAction = {},
            )
        },
        Story(
            id = "settings.paywall-plans",
            area = "Settings",
            title = "Pro subscription (plans)",
            description = "The plans page once Google Play sells them: monthly and yearly plans, the yearly chosen, and restoring a purchase.",
        ) {
            PaywallScreen(
                state = PaywallScreenState(
                    plans = listOf(
                        PaywallPlanUi(id = "pro.monthly", period = PaywallPlanPeriod.Monthly, price = "$4.99"),
                        PaywallPlanUi(id = "pro.yearly", period = PaywallPlanPeriod.Yearly, price = "$49.99", saving = "Save 17%"),
                    ),
                    chosen = PaywallPlanPeriod.Yearly,
                    phase = PaywallPhaseUi.Ready,
                    accountEmail = "ada@example.com",
                ),
                onAction = {},
            )
        },
        Story(
            id = "settings.delete",
            area = "Settings",
            title = "Delete account confirmation",
            description = "Asking before an account is permanently deleted, with the address typed to confirm, over Settings.",
        ) {
            SheetStoryFrame(background = { SettingsScreen(state = signedInSettings(), onAction = {}) }) {
                DeleteAccountSheetContent(
                    state = DeleteAccountSheetState(
                        accountId = "ada",
                        email = "ada@example.com",
                        typedEmail = "ada@example.com",
                        billingWarning = null,
                        phase = DeleteAccountPhaseUi.Asking,
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "settings.large-text",
            area = "Settings",
            title = "Settings (large text)",
            description = "Settings at Android's largest text size.",
            fontScale = LARGEST_FONT_SCALE,
        ) {
            SettingsScreen(state = signedInSettings(), onAction = {})
        },

        // --- Report --------------------------------------------------------
        Story(
            id = "report.ready",
            area = "Report",
            title = "Report a problem (ready)",
            description = "Reporting a problem over a frozen copy of the screen before writing a note.",
        ) {
            val frameBytes = remember { sampleReportPngBytes() }
            ReportOverlayContent(
                state = ReportUiState(
                    imageBytes = frameBytes,
                    note = "",
                    marks = emptyList(),
                    signedIn = true,
                    dumpReady = true,
                    delivery = ReportDeliveryUi.Ready,
                ),
                onAction = {},
            )
        },
        Story(
            id = "report.sent",
            area = "Report",
            title = "Report sent",
            description = "A sent problem report showing the reference ID to quote.",
        ) {
            val frameBytes = remember { sampleReportPngBytes() }
            ReportOverlayContent(
                state = ReportUiState(
                    imageBytes = frameBytes,
                    note = "Scroll jumped past the last prompt after reconnect.",
                    marks = listOf(
                        ReportMarkUi(
                            left = 0.12f,
                            top = 0.24f,
                            right = 0.88f,
                            bottom = 0.44f,
                            note = "Jump happened here",
                        ),
                    ),
                    signedIn = true,
                    dumpReady = true,
                    delivery = ReportDeliveryUi.Sent(referenceId = "rep_4KX2M9"),
                ),
                onAction = {},
            )
        },

        // --- Review --------------------------------------------------------
        // The iOS catalogue's review examples, by its ids, drawn from the
        // pages the shared runtime would hand over for one invented review.
        reviewStory("page", "Review page", "All changes on a first look: three files, the lock file folded, two drafts.") {
            ReviewStoryScreen(R.page())
        },
        reviewStory("selection", "Lines selected", "Two lines of a file selected, with the comment they take at the foot.") {
            ReviewStoryScreen(R.page(), selectTwoLines = true)
        },
        reviewStory("comment", "Writing a comment", "The comment sheet raised over the lines it is about.") {
            val page = R.page()
            SheetStoryFrame(background = { ReviewStoryScreen(page, selectTwoLines = true) }) {
                ReviewCommentSheetContent(
                    title = ChatWords.commentOn(3),
                    note = TextFieldValue(),
                    onNoteChange = {},
                    editing = false,
                    onDismiss = {},
                    onAct = {},
                    focusOnOpen = false,
                )
            }
        },
        reviewStory("editing", "Editing a comment", "A draft opened to be reworded or deleted.") {
            val page = R.page()
            val comment = R.firstComment(page)
            SheetStoryFrame(background = { ReviewStoryScreen(page) }) {
                ReviewCommentSheetContent(
                    title = comment?.let { ChatWords.reviewCommentState(it.state) }.orEmpty(),
                    note = comment?.text.orEmpty().let { TextFieldValue(it, selection = TextRange(it.length)) },
                    onNoteChange = {},
                    editing = true,
                    onDismiss = {},
                    onAct = {},
                    focusOnOpen = false,
                )
            }
        },
        reviewStory("folded", "Files folded", "Every file folded to its head.") {
            ReviewStoryScreen(R.page(folded = true))
        },
        reviewStory(
            "since-last",
            "Since your last review",
            "The next look: the two views, comments sent and outdated or near changed code, and a draft on a new line.",
        ) {
            ReviewStoryScreen(R.sinceLast())
        },
        reviewStory("drafts-stale", "Drafts on an older snapshot", "The page held on its drafts' snapshot after the agent changed code, offering to refresh.") {
            ReviewStoryScreen(R.page(banners = listOf(ReviewBanner.ChangedSinceYouStarted)))
        },
        reviewStory("moved-drafts", "Drafts moved", "A refresh moved two drafts onto the new snapshot.") {
            ReviewStoryScreen(R.page(banners = listOf(ReviewBanner.MovedDrafts(count = 2))))
        },
        reviewStory("empty", "Nothing to review", "The review page with nothing left to review.") {
            ReviewStoryScreen(R.empty())
        },
        reviewStory("nothing-since", "Nothing since the last round", "Nothing changed since the last round, which still lists the comment sent in it.") {
            ReviewStoryScreen(R.nothingSince())
        },
        reviewStory("not-offered", "Reviews not offered", "An agent whose computer runs no reviews: the page says so.") {
            ReviewStoryScreen(R.notOffered())
        },
        reviewStory("opening", "Review opening", "The page before the review's summary arrives.") {
            ReviewStoryScreen(ReviewSession.OPENING)
        },
        reviewStory("large-text", "Review page (large text)", "The review page at Android's largest text size.", fontScale = LARGEST_FONT_SCALE) {
            ReviewStoryScreen(R.page())
        },
        reviewStory("round", "A sent round", "What a sent review opens: the round it closed, its whole diff with each comment on its lines.") {
            SheetStoryFrame(background = { StoryChatScreen(conversationChat()) }) {
                ReviewRoundContent(sent = R.sent, text = R.sentText, asked = true, round = R.round(), onOpenReview = {}, onClose = {})
            }
        },
        reviewStory("round-text", "A sent round no longer held", "A round the computer no longer holds: the text the agent read.") {
            SheetStoryFrame(background = { StoryChatScreen(conversationChat()) }) {
                ReviewRoundContent(sent = R.sent, text = R.sentText, asked = true, round = null, onOpenReview = {}, onClose = {})
            }
        },
        reviewStory("reading", "Reading the changes", "The review page while its chat is still opening.") {
            ReviewPlaceholderScreen(failure = null, onBack = {})
        },
        Story(
            id = "composer.review-chip",
            area = "Review",
            title = "Review chip in the composer",
            description = "The review's drafts as a chip after the draft's words, which the next send carries.",
        ) {
            StoryChatScreen(
                chat(
                    rows = F.conversation,
                    frame = ScriptedChat.frame(phase = PhaseView.Idle, reviewChip = ReviewChip(comments = 3, moved = false)),
                    strip = opus,
                    draft = "These three, then run the suite.",
                    canSend = true,
                ),
            )
        },
        Story(
            id = "composer.review-chip-working",
            area = "Review",
            title = "Review chip while the agent works",
            description = "The chip alone while the agent works, after the code moved: the round button still sends.",
        ) {
            StoryChatScreen(
                chat(
                    rows = F.working,
                    frame = ScriptedChat.frame(
                        phase = PhaseView.Working,
                        activity = F.workingActivity,
                        reviewChip = ReviewChip(comments = 1, moved = true),
                    ),
                    strip = opus,
                    canSend = true,
                ),
            )
        },

        // --- System --------------------------------------------------------
        Story(
            id = "store.failure",
            area = "System",
            title = "Store unavailable",
            description = "Full-screen recovery view when the installation store fails to open.",
        ) {
            StoreFailureScreen(
                message = "sqlite error 11: database disk image is malformed (profile default)",
                onRelaunch = {},
            )
        },
    )

    fun matching(filter: String?): List<Story> {
        val raw = filter?.trim().orEmpty()
        if (raw.isEmpty()) return all
        val tokens = raw.split(',').map { it.trim() }.filter { it.isNotEmpty() }
        return all.filter { story ->
            tokens.any { token ->
                story.id.equals(token, ignoreCase = true) ||
                    story.id.startsWith(token, ignoreCase = true) ||
                    story.area.equals(token, ignoreCase = true)
            }
        }
    }

    // --- Frames ---------------------------------------------------------------

    @Composable
    private fun DialogStoryFrame(
        background: @Composable () -> Unit,
        content: @Composable () -> Unit,
    ) {
        Box(modifier = Modifier.fillMaxSize()) {
            // The real dialog is a window of its own that hides the screen
            // behind it from TalkBack; the sweep should see what TalkBack does.
            Box(modifier = Modifier.fillMaxSize().clearAndSetSemantics {}) {
                background()
            }
            AmuxScrim()
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(horizontal = Space.xl),
                contentAlignment = Alignment.Center,
            ) {
                content()
            }
        }
    }

    /**
     * A sheet raised over the screen it was raised from, drawn with the chrome
     * the app's `ModalBottomSheet`s draw: Material's expanded shape, scrim and
     * drag handle, and the raised container `AmuxSheet` gives every one of
 * them, with the raised backdrop its content reads.
     *
     * The real sheet is a window of its own, which a single-root capture and
     * the accessibility sweep cannot see; this draws the same pieces in one.
     * The screen behind is hidden from semantics, as the sheet's own window
     * hides it from TalkBack.
     */
    @OptIn(ExperimentalMaterial3Api::class)
    @Composable
    private fun SheetStoryFrame(
        background: @Composable () -> Unit,
        content: @Composable () -> Unit,
    ) {
        val colors = AmuxTheme.colors
        Box(modifier = Modifier.fillMaxSize()) {
            Box(modifier = Modifier.fillMaxSize().clearAndSetSemantics {}) {
                background()
            }
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(BottomSheetDefaults.ScrimColor),
            )
            Surface(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .widthIn(max = BottomSheetDefaults.SheetMaxWidth)
                    .fillMaxWidth(),
                shape = BottomSheetDefaults.ExpandedShape,
                color = colors.raised,
                contentColor = colors.ink,
                tonalElevation = 0.dp,
            ) {
                ProvideAmuxBackdrop(Backdrop.Raised) {
                    Column(modifier = Modifier.fillMaxWidth()) {
                        BottomSheetDefaults.DragHandle(modifier = Modifier.align(Alignment.CenterHorizontally))
                        content()
                    }
                }
            }
        }
    }

    private fun pairingStory(id: String, title: String, description: String, state: PairingSheetState): Story =
        Story(id = id, area = "Pairing", title = title, description = description) {
            SheetStoryFrame(background = { HostsTab(state = hostsTabState(F.machines), onAction = {}) }) {
                PairingSheetContent(state = state, onAction = {})
            }
        }

    // --- Review ---------------------------------------------------------------

    private val R = ReviewStory

    private fun reviewStory(
        id: String,
        title: String,
        description: String,
        fontScale: Float = 1.0f,
        content: @Composable () -> Unit,
    ): Story = Story(
        id = "review.$id",
        area = "Review",
        title = title,
        description = description,
        fontScale = fontScale,
        content = content,
    )

    /** The review page as the app draws it for [page], with the store's two lines selected when asked. */
    @Composable
    private fun ReviewStoryScreen(page: ReviewPage, selectTwoLines: Boolean = false) {
        val (from, to) = R.twoLines(page)
        val state = ReviewScreenState(
            page = page,
            selected = if (selectTwoLines) minOf(from, to)..maxOf(from, to) else null,
            anchor = if (selectTwoLines) R.anchor else null,
        )
        ReviewScreen(state = state, onAction = {})
    }

    // --- Chats ----------------------------------------------------------------

    private class StoryChat(val state: ChatScreenState, val source: StaticChatRowSource)

    @Composable
    private fun StoryChatScreen(chat: StoryChat) {
        ChatScreen(state = chat.state, rowSource = chat.source, clock = STORY_CLOCK)
    }

    /**
     * A chat as the app's chat state would show [rows] under [frame]: runs
     * and groups folded as the shared view folds them, the surroundings
     * applied to the frame, and the comparisons and change totals the chat
     * model derives from its git facts. Dictation is on, as it is on a phone
     * that offers it, so the composer draws its microphone.
     */
    private fun chat(
        rows: List<Row>,
        frame: ChatFrame,
        strip: ScriptedChat.Surroundings = ScriptedChat.strip(),
        subject: ChatSubject = F.subject,
        card: AskCard? = null,
        family: FamilyHeader? = null,
        settings: SettingsView? = null,
        openGroups: Set<String> = emptySet(),
        groupStates: Map<String, GroupState> = emptyMap(),
        openedAgents: Map<List<Int>, OpenedAgent> = emptyMap(),
        liveSubagents: GroupState = EMPTY_GROUP_STATE,
        draft: String = "",
        questionDraft: QuestionDraft? = null,
        canResume: Boolean = false,
        canSend: Boolean = false,
        loadingHint: Boolean = false,
    ): StoryChat {
        val folded = rows.map { ScriptedChat.folding(it, open = emptySet(), groups = openGroups) }
        val shown = strip.appliedTo(frame)
        val git = shown.git
        val running = (shown.composer.activity?.kind as? ActivityKind.Running)?.key
        val state = ChatScreenState(
            subject = subject,
            family = family,
            keys = folded.map { it.id },
            frame = shown,
            ask = card,
            overview = strip.overview,
            settings = settings,
            openedAgents = openedAgents,
            liveSubagents = liveSubagents,
            groupStates = groupStates,
            loadingHint = loadingHint,
            draft = draft,
            canResume = canResume,
            canSend = canSend,
            comparisons = if (git?.baseBranch == null) {
                listOf(Comparison.Uncommitted)
            } else {
                listOf(Comparison.Uncommitted, Comparison.OnBranch)
            },
            changes = git?.uncommitted?.takeIf { it.files > 0 || it.added + it.removed > 0 },
            activitySubject = running?.let { key -> folded.firstOrNull { it.id == key } }?.let(ChatWords::subject),
            keptQuestionDraft = questionDraft,
            dictationEnabled = true,
            reviewChip = shown.reviewChip,
        )
        val source = StaticChatRowSource(
            rows = folded,
            openedAgents = openedAgents,
            groupStates = groupStates,
        )
        return StoryChat(state, source)
    }

    private val opus = ScriptedChat.strip(model = "opus 4.6")

    private fun conversationChat(): StoryChat =
        chat(rows = F.conversation, frame = ScriptedChat.frame(phase = PhaseView.Idle), strip = opus)

    private fun workingChat(queue: Boolean = false): StoryChat = chat(
        rows = F.working,
        frame = ScriptedChat.frame(
            phase = PhaseView.Working,
            activity = F.workingActivity,
            queue = if (queue) F.queue else emptyList(),
        ),
        strip = opus,
    )

    private fun dockChat(open: Boolean): StoryChat {
        val chat = chat(
            rows = F.working,
            frame = ScriptedChat.frame(
                phase = PhaseView.Working,
                activity = F.workingActivity,
                queue = F.queue.take(1),
            ),
            strip = ScriptedChat.strip(
                tasks = F.pairingTasks,
                model = "opus 4.6",
                effort = "high",
                background = if (open) null else 2,
            ),
            family = F.family.copy(children = F.family.children + F.docsSweep),
        )
        return StoryChat(chat.state.copy(dockOpen = open), chat.source)
    }

    private fun delegatingChat(open: Boolean): StoryChat = chat(
        rows = F.delegating,
        frame = ScriptedChat.frame(phase = PhaseView.Working, activity = F.delegatingActivity),
        strip = opus,
        openGroups = if (open) setOf(F.DELEGATED_GROUP) else emptySet(),
        groupStates = mapOf(F.DELEGATED_GROUP to F.delegatedState),
        openedAgents = F.delegatedAgents,
        liveSubagents = F.delegatedLive,
    )

    /**
     * The whole app's first agent opened after it exited, as iOS's story of
     * the app draws it: the busy fleet's pinned nightly-triage on desk.
     */
    private fun exitedChat(): StoryChat {
        val first = StoryFleet.busy().sections.first().rows.first().card
        return chat(
            rows = F.conversation,
            frame = ScriptedChat.frame(
                name = first.name,
                phase = PhaseView.Exited(ExitCause.Failed("while amux was stopped")),
                mode = Composer.Resume,
            ),
            strip = ScriptedChat.strip(model = "opus 4.6", git = F.git),
            subject = ChatSubject(
                name = first.name,
                host = first.host,
                directory = first.cwd,
                reach = first.hostReach,
                filing = first.filing,
                key = first.agent,
            ),
            canResume = true,
        )
    }

    private fun overviewChat(): StoryChat =
        chat(rows = F.conversation, frame = ScriptedChat.frame(phase = PhaseView.Working), strip = F.surroundings)

    /** The overview state the chat screen builds for its sheet. */
    private fun overviewOf(state: ChatScreenState): ChatOverviewState = ChatOverviewState(
        subject = state.subject.toOverviewUi(),
        overview = state.overview,
        context = state.frame?.context,
        comparisons = state.comparisons,
        comparison = state.comparison,
        baseBranch = state.frame?.git?.baseBranch,
        changeTotals = state.changes,
        nowMs = NOW_MS,
    )

    /** An ask docked over a chat that has only just been asked something. */
    private fun askStory(
        id: String,
        title: String,
        card: AskCard,
        draft: QuestionDraft? = null,
        taps: List<String> = emptyList(),
        fontScale: Float = 1.0f,
        description: String = "The iOS catalogue's ask.$id, docked in place of the composer.",
    ): Story = Story(
        id = "ask.$id",
        area = "Asks",
        title = title,
        description = description,
        fontScale = fontScale,
        taps = taps,
    ) {
        StoryChatScreen(
            chat(
                rows = F.conversation.take(1),
                frame = ScriptedChat.frame(kind = card.kind, phase = PhaseView.NeedsYou),
                card = card,
                questionDraft = draft,
            ),
        )
    }

    /** A question ask's progress, as a person who answered this far left it. */
    private fun questionDraft(
        count: Int,
        picks: List<Set<Int>> = List(count) { emptySet() },
        others: List<String?> = List(count) { null },
        highlighted: List<Int?> = List(count) { null },
        skipped: List<Boolean> = List(count) { false },
        notes: List<String> = List(count) { "" },
        noting: List<Boolean> = List(count) { false },
        reviewing: Boolean = false,
        replying: Boolean = false,
        replyText: String = "",
    ): QuestionDraft = QuestionDraft(
        step = if (reviewing) count - 1 else 0,
        picks = picks,
        others = others,
        highlighted = highlighted,
        skipped = skipped,
        notes = notes,
        noting = noting,
        reviewing = reviewing,
        replying = replying,
        replyText = replyText,
    )

    // --- Other screens --------------------------------------------------------

    /** The busy list as the runtime filters it: the two whose step runs the tests. */
    private fun filteredFleet(): AgentsHomeState {
        val base = StoryFleet.busy()
        val matching = setOf("auth-refactor", "flaky-e2e")
        val sections = base.sections.mapNotNull { section ->
            val kept = section.rows.filter { it.card.name in matching }
            if (kept.isEmpty()) null else section.copy(rows = kept, families = kept.size)
        }
        return base.copy(
            rows = sections.flatMap { it.rows },
            sections = sections,
            filter = sh.amux.values.FleetFilter(words = "test", only = null),
        )
    }

    /** The hosts tab over [machines], paired ones listed and the rest offered. */
    private fun hostsTabState(machines: List<sh.amux.values.HostView>, pro: Boolean? = null): HostsTabState {
        val (found, paired) = machines.partition { it.candidate }
        return HostsTabState(
            hosts = paired,
            discovered = found,
            accountPro = pro,
            awayHostName = paired.firstOrNull { it.reach == Reach.Away }?.name,
            nowMs = NOW_MS,
        )
    }

    private fun newAgentState(): NewAgentScreenState = NewAgentScreenState(
        hosts = F.newAgentHosts,
        selectedHostId = F.newAgentHosts.first().hostId,
        provider = Provider.Claude,
        availableProviders = listOf(Provider.Claude, Provider.Codex),
        offer = NewAgentOfferUi.Offered,
        settings = F.claudeCatalogue,
        listing = NewAgentListingUi.Ready,
        repositories = F.repositoryDirectories,
        recent = F.recentDirectories,
        directory = F.repositoryDirectories.first().path,
        newWorktree = true,
    )

    private fun signedInSettings(): SettingsScreenState = SettingsScreenState(
        accounts = listOf(
            SettingsAccountItem(
                id = "ada",
                name = "Ada",
                email = "ada@example.com",
                line = "3 computers",
                signedIn = true,
                selected = true,
                entitlementBadge = SettingsEntitlementBadge.Free,
            ),
        ),
        appearance = SettingsAppearance.System,
        notificationStatus = SettingsNotificationStatus.Off,
        pushDeliveryComingSoon = false,
        shareUsage = true,
        autoReports = false,
        deviceName = "Pixel 9",
        deviceFingerprint = F.fingerprint(9),
        fingerprintCopied = false,
        appVersion = "0.11.0",
        showContactSupport = true,
    )

    private fun sampleReportPngBytes(): ByteArray? = runCatching {
        val width = 180
        val height = 390
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        val bgPaint = Paint().apply { color = 0xFFF6F6F5.toInt() }
        val cardPaint = Paint().apply { color = 0xFFFFFFFF.toInt() }
        val linePaint = Paint().apply { color = 0xFFDCDCD9.toInt() }
        val inkPaint = Paint().apply { color = 0xFF161615.toInt() }
        val mutedPaint = Paint().apply { color = 0xFF8C8C87.toInt() }
        val accentPaint = Paint().apply { color = 0xFFD95D39.toInt() }

        canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), bgPaint)
        canvas.drawRect(0f, 0f, width.toFloat(), 48f, cardPaint)
        canvas.drawRect(0f, 47f, width.toFloat(), 48f, linePaint)
        canvas.drawRoundRect(RectF(14f, 16f, 96f, 30f), 6f, 6f, inkPaint)

        for (row in 0..3) {
            val top = 64f + row * 68f
            canvas.drawRoundRect(RectF(14f, top, width - 14f, top + 56f), 10f, 10f, cardPaint)
            canvas.drawRoundRect(
                RectF(24f, top + 12f, 24f + if (row == 0) 86f else 104f, top + 22f),
                4f,
                4f,
                if (row == 0) accentPaint else inkPaint,
            )
            canvas.drawRoundRect(RectF(24f, top + 30f, width - 32f, top + 38f), 4f, 4f, mutedPaint)
        }

        val out = ByteArrayOutputStream()
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
        bitmap.recycle()
        out.toByteArray()
    }.getOrNull()
}
