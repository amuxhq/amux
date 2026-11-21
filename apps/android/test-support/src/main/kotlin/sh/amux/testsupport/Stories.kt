package sh.amux.testsupport

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import java.io.ByteArrayOutputStream
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxScrim
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.features.AccountUi
import sh.amux.features.AgentsHome
import sh.amux.features.AgentsHomeState
import sh.amux.features.ChatDeleteDialogContent
import sh.amux.features.ChatOverviewSheetContent
import sh.amux.features.ChatOverviewState
import sh.amux.features.ChatOverviewStopDialogContent
import sh.amux.features.ChatRenameDialogContent
import sh.amux.features.ChatScreen
import sh.amux.features.ChatScreenState
import sh.amux.features.ChatSettingsSheetContent
import sh.amux.features.ChatSettingsState
import sh.amux.features.ChatSubject
import sh.amux.features.ChatSubjectUi
import sh.amux.features.ChatUnavailableScreen
import sh.amux.features.DeleteAccountPhaseUi
import sh.amux.features.DeleteAccountSheetContent
import sh.amux.features.DeleteAccountSheetState
import sh.amux.features.HostPage
import sh.amux.features.HostPageState
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
import sh.amux.features.ReportDeliveryUi
import sh.amux.features.ReportMarkUi
import sh.amux.features.ReportOverlayContent
import sh.amux.features.ReportUiState
import sh.amux.features.ReviewPage
import sh.amux.features.ReviewPageState
import sh.amux.features.SettledScreen
import sh.amux.features.SettledScreenState
import sh.amux.features.SettingsAccountItem
import sh.amux.features.SettingsAppearance
import sh.amux.features.SettingsEntitlementBadge
import sh.amux.features.SettingsNotificationStatus
import sh.amux.features.SettingsScreen
import sh.amux.features.SettingsScreenState
import sh.amux.features.StaticChatRowSource
import sh.amux.features.StoreFailureScreen
import sh.amux.push.Said
import sh.amux.push.Wants
import sh.amux.values.Activity
import sh.amux.values.ActivityKind
import sh.amux.values.ActivityLine
import sh.amux.values.AgentKey
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.Attention
import sh.amux.values.BackgroundKind
import sh.amux.values.CallPhase
import sh.amux.values.CardState
import sh.amux.values.ChangeTotals
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.ChangedFile
import sh.amux.values.Changes
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.CommandView
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Connection
import sh.amux.values.ContextView
import sh.amux.values.ControlsSummary
import sh.amux.values.DeleteWarning
import sh.amux.values.DiffLine
import sh.amux.values.Directory
import sh.amux.values.EditLine
import sh.amux.values.EffortChoice
import sh.amux.values.ExitCause
import sh.amux.values.ExploreVerb
import sh.amux.values.FamilyHeader
import sh.amux.values.FileChangeView
import sh.amux.values.FileRow
import sh.amux.values.FileStatus
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetFilter
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.Folder
import sh.amux.values.FormField
import sh.amux.values.FormFieldKind
import sh.amux.values.FormValue
import sh.amux.values.Git
import sh.amux.values.GroupState
import sh.amux.values.HostView
import sh.amux.values.Hunk
import sh.amux.values.JobRow
import sh.amux.values.JobStop
import sh.amux.values.Kind
import sh.amux.values.LineKind
import sh.amux.values.ModeChoice
import sh.amux.values.ModelChoice
import sh.amux.values.Offline
import sh.amux.values.OpenedAgent
import sh.amux.values.OptionView
import sh.amux.values.Overview
import sh.amux.values.PatchLine
import sh.amux.values.PendingPair
import sh.amux.values.PermissionChoice
import sh.amux.values.PhaseView
import sh.amux.values.Platform
import sh.amux.values.Provider
import sh.amux.values.ProviderSignIn
import sh.amux.values.QuestionView
import sh.amux.values.QueuedRow
import sh.amux.values.Reach
import sh.amux.values.Repository
import sh.amux.values.ReviewComment
import sh.amux.values.ReviewDoc
import sh.amux.values.ReviewFile
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.Scope
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind
import sh.amux.values.Segment
import sh.amux.values.ServerView
import sh.amux.values.SettingsView
import sh.amux.values.SignInState
import sh.amux.values.SignInView
import sh.amux.values.SubagentGroup
import sh.amux.values.TaskLine
import sh.amux.values.TaskMark
import sh.amux.values.TasksView
import sh.amux.values.UsageLabel
import sh.amux.values.UsageState
import sh.amux.values.UsageView
import sh.amux.values.UsageWindowView
import sh.amux.values.Waiting
import sh.amux.values.WorkspaceGoes

/**
 * One named storybook screen fixture rendered by both headless Roborazzi
 * captures (`just android storybook`) and the debug-only `StorybookActivity`.
 */
data class Story(
    val id: String,
    val area: String,
    val title: String,
    val description: String,
    val fontScale: Float = 1.0f,
    val content: @Composable () -> Unit,
)

/**
 * Central registry of Android storybook stories aligned with `tests/stories.json`.
 */
object Stories {
    private const val NOW_MS: Long = 1_750_000_000_000L

    /**
     * Stories listed under `"clients": [..., "phone", ...]` in `tests/stories.json`
     * that are intentionally excluded on Android, along with why.
     */
    val IOS_ONLY_STORIES: Map<String, String> = mapOf(
        "fleet.first-run-refused" to "Android has no iOS Local Network permission dialog.",
        "hosts.network-refused" to "Android has no iOS Local Network permission dialog.",
    )

    private val desk = host(1, "desk", Platform.Macos)
    private val studio = host(2, "studio", Platform.Macos, addrs = listOf("192.168.1.2:4433", "fe80::2:4433"))
    private val studioAway = host(2, "studio", Platform.Macos, reach = Reach.Away)
    private val garagePi = host(3, "garage-pi", Platform.Linux, reach = Reach.Online(RouteKind.Relay))
    private val oldMini = host(7, "old-mini", Platform.Macos, reach = Reach.Offline(Offline.Plain))
    private val laptopFound = host(4, "laptop", Platform.Macos, trusted = false, candidate = true, pairingOpen = false)
    private val laptopOpen = host(4, "laptop", Platform.Macos, trusted = false, candidate = true, pairingOpen = true)
    private val studioFound = host(5, "studio", Platform.Macos, trusted = false, candidate = true, pairingOpen = false)
    private val studioRelay = host(
        index = 6,
        name = "studio",
        platform = Platform.Macos,
        trusted = false,
        candidate = true,
        reach = Reach.Offline(Offline.Plain),
        relayOnly = true,
        pairingOpen = false,
    )

    private const val SAMPLE_HOST_FINGERPRINT =
        "9f4b2c81d7e03a5612489bcdf013579a6c8e0241b5d7f93a1e2c4b6d8f0a3c5e"
    private const val SAMPLE_PHONE_FINGERPRINT =
        "4c8e19a0d2f5b73168e4a209c7d3f18b5e60a4c2d8f1b309e7a5c1d4f2b8063e"

    private val adaAccount = AccountUi(
        id = "ada",
        name = "Ada",
        email = "ada@example.com",
        line = "3 hosts",
        signedIn = true,
        selected = true,
    )

    private val workAccount = AccountUi(
        id = "ada-work",
        name = "Lovelace Labs",
        email = "ada@lovelace.dev",
        line = "2 hosts",
        signedIn = true,
        selected = false,
    )

    val all: List<Story> = listOf(
        // --- Fleet ---------------------------------------------------------
        Story(
            id = "fleet.busy",
            area = "Fleet",
            title = "Busy fleet",
            description = "A day's agents across machines, one away: pinned, needs you, working, idle, a family, and settled.",
        ) {
            AgentsHome(state = busyFleetState(away = true), onAction = {})
        },
        Story(
            id = "fleet.subagents",
            area = "Fleet",
            title = "Expanded subagent family",
            description = "A family opened to its Claude subagents listed under their parent, one waiting on you.",
        ) {
            AgentsHome(state = subagentsFleetState(), onAction = {})
        },
        Story(
            id = "fleet.signed-out",
            area = "Fleet",
            title = "Busy fleet (signed out)",
            description = "Busy agent list on a phone with no account signed in.",
        ) {
            AgentsHome(
                state = busyFleetState(
                    away = false,
                    accounts = emptyList(),
                    includeNearby = false,
                ).copy(accountPro = null),
                onAction = {},
            )
        },
        Story(
            id = "fleet.settled",
            area = "Fleet",
            title = "Settled agents",
            description = "Settled archive list with search, restore, and delete actions.",
        ) {
            SettledScreen(
                state = sampleSettledScreenState(),
                onAction = {},
            )
        },
        Story(
            id = "fleet.no-agents",
            area = "Fleet",
            title = "No agents yet",
            description = "Paired with a computer that has no agents running yet.",
        ) {
            AgentsHome(
                state = AgentsHomeState(
                    loaded = true,
                    machines = listOf(desk),
                    orderedAtMs = NOW_MS,
                    accounts = listOf(adaAccount),
                    selectedAccount = adaAccount,
                    accountPro = true,
                ),
                onAction = {},
            )
        },
        Story(
            id = "fleet.first-run",
            area = "Fleet",
            title = "First run (computer found)",
            description = "No computers paired yet, with one nearby computer discovered on the network.",
        ) {
            AgentsHome(
                state = AgentsHomeState(
                    loaded = true,
                    machines = emptyList(),
                    discoveredHosts = listOf(laptopFound),
                    orderedAtMs = NOW_MS,
                    accounts = listOf(adaAccount),
                    selectedAccount = adaAccount,
                    accountPro = true,
                ),
                onAction = {},
            )
        },
        Story(
            id = "fleet.first-run-two-found",
            area = "Fleet",
            title = "First run (two found)",
            description = "No computers paired yet, with two computers discovered on this network.",
        ) {
            AgentsHome(
                state = AgentsHomeState(
                    loaded = true,
                    machines = emptyList(),
                    discoveredHosts = listOf(laptopFound, studioFound),
                    orderedAtMs = NOW_MS,
                    accounts = listOf(adaAccount),
                    selectedAccount = adaAccount,
                    accountPro = true,
                ),
                onAction = {},
            )
        },
        Story(
            id = "fleet.first-run-window-open",
            area = "Fleet",
            title = "First run (pairing window open)",
            description = "Nearby computer advertising an open pairing window ready for one-tap pairing.",
        ) {
            AgentsHome(
                state = AgentsHomeState(
                    loaded = true,
                    machines = emptyList(),
                    discoveredHosts = listOf(laptopOpen),
                    orderedAtMs = NOW_MS,
                    accounts = listOf(adaAccount),
                    selectedAccount = adaAccount,
                    accountPro = true,
                ),
                onAction = {},
            )
        },
        Story(
            id = "fleet.first-run-looking",
            area = "Fleet",
            title = "First run (looking)",
            description = "Nothing paired and nothing found yet: looking on this network with two setup steps.",
        ) {
            AgentsHome(
                state = AgentsHomeState(
                    loaded = true,
                    machines = emptyList(),
                    discoveredHosts = emptyList(),
                    orderedAtMs = NOW_MS,
                    accounts = listOf(adaAccount),
                    selectedAccount = adaAccount,
                    accountPro = true,
                ),
                onAction = {},
            )
        },
        Story(
            id = "fleet.first-run-signed-out",
            area = "Fleet",
            title = "First run (signed out)",
            description = "Nothing paired on a phone with no account, offering Sign In below pairing steps.",
        ) {
            AgentsHome(
                state = AgentsHomeState(
                    loaded = true,
                    machines = emptyList(),
                    discoveredHosts = emptyList(),
                    orderedAtMs = NOW_MS,
                    accounts = emptyList(),
                    selectedAccount = null,
                ),
                onAction = {},
            )
        },
        Story(
            id = "fleet.first-run-on-the-account-free",
            area = "Fleet",
            title = "First run (relay host on free plan)",
            description = "Nothing paired on the free plan, with a computer on the account reachable only through the relay.",
        ) {
            AgentsHome(
                state = AgentsHomeState(
                    loaded = true,
                    machines = emptyList(),
                    discoveredHosts = listOf(studioRelay),
                    orderedAtMs = NOW_MS,
                    accounts = listOf(adaAccount),
                    selectedAccount = adaAccount,
                    accountPro = false,
                ),
                onAction = {},
            )
        },
        Story(
            id = "fleet.filter",
            area = "Fleet",
            title = "Filtered fleet",
            description = "Search bar expanded and list narrowed to matching agents.",
        ) {
            val base = busyFleetState(away = false, includeNearby = false)
            val matching = setOf("auth-refactor", "flaky-e2e")
            val filteredSections = base.sections.mapNotNull { section ->
                val kept = section.rows.filter { it.card.name in matching }
                if (kept.isEmpty()) null else section.copy(rows = kept, families = kept.size)
            }
            AgentsHome(
                state = base.copy(
                    rows = filteredSections.flatMap { it.rows },
                    sections = filteredSections,
                    filter = FleetFilter(words = "test", only = null),
                ),
                searchExpanded = true,
                onAction = {},
            )
        },
        Story(
            id = "fleet.no-matches",
            area = "Fleet",
            title = "No matching agents",
            description = "Search narrowed to zero matches with Clear action.",
        ) {
            val base = busyFleetState(away = false, includeNearby = false)
            AgentsHome(
                state = base.copy(
                    rows = emptyList(),
                    sections = emptyList(),
                    filter = FleetFilter(words = "kubernetes", only = null),
                ),
                searchExpanded = true,
                onAction = {},
            )
        },
        Story(
            id = "fleet.large-text",
            area = "Fleet",
            title = "Busy fleet (large text)",
            description = "Busy agent list at 1.5x accessibility font scaling.",
            fontScale = 1.5f,
        ) {
            AgentsHome(state = busyFleetState(away = false, includeNearby = false), onAction = {})
        },
        Story(
            id = "fleet.two-accounts",
            area = "Fleet",
            title = "Two accounts",
            description = "Busy fleet on a phone signed into two accounts.",
        ) {
            AgentsHome(
                state = busyFleetState(
                    away = false,
                    accounts = listOf(adaAccount, workAccount),
                ),
                onAction = {},
            )
        },

        // --- New Agent -----------------------------------------------------
        Story(
            id = "new-agent.claude",
            area = "New Agent",
            title = "New Claude agent",
            description = "Starting a Claude agent: host, folder, name, model, effort, and permission.",
        ) {
            NewAgentScreen(
                state = sampleNewAgentState(),
                onAction = {},
            )
        },
        Story(
            id = "new-agent.browsing",
            area = "New Agent",
            title = "New Agent folder browser",
            description = "Choosing the folder a new agent starts in: pinned repositories, recent folders, and a typed path.",
        ) {
            SheetStoryFrame {
                NewAgentDirectoriesContent(
                    state = sampleNewAgentState().copy(
                        browsing = true,
                        typedPath = "~/source/experiments/relay-probe",
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "new-agent.large-text",
            area = "New Agent",
            title = "New Agent (large text)",
            description = "New Agent form at 1.5x accessibility font scaling with stacked values and sticky Start bar.",
            fontScale = 1.5f,
        ) {
            NewAgentScreen(
                state = sampleNewAgentState(),
                onAction = {},
            )
        },

        // --- Chat ----------------------------------------------------------
        Story(
            id = "chat.conversation",
            area = "Chat",
            title = "Conversation",
            description = "Mixed markdown prose, collapsed explore run, command output, file change, subagent group, and active composer.",
        ) {
            val (state, source) = activeChatFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.working",
            area = "Chat",
            title = "Working turn",
            description = "Turn at work with streaming prose, live activity line, and Interrupt button.",
        ) {
            val (state, source) = streamingChatFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.asking",
            area = "Chat",
            title = "Agent asking",
            description = "A chat whose agent is asking for approval, docked in place of the composer.",
        ) {
            val (state, source) = askChatFixture(commandAskCard())
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.subagents",
            area = "Chat",
            title = "Subagents group",
            description = "Parent chat showing an expanded group of subagent calls: 2 running, 1 waiting on you, 1 done.",
        ) {
            val (state, source) = subagentsGroupChatFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.subagent",
            area = "Chat",
            title = "Provider subagent",
            description = "Claude subagent conversation with parent chip and read-only native composer with Stop.",
        ) {
            val (state, source) = nativeSubagentFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.plan",
            area = "Chat",
            title = "Plan approval",
            description = "Proposed plan read whole and waiting for approval in place of the composer.",
        ) {
            val (state, source) = askChatFixture(planAskCard())
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.exited",
            area = "Chat",
            title = "Exited agent",
            description = "Finished conversation ready to resume with the Resume composer.",
        ) {
            val (state, source) = exitedChatFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.away",
            area = "Chat",
            title = "Host away with kept draft",
            description = "Away host showing out-of-reach notice, kept draft text, and disabled Send.",
        ) {
            val (state, source) = offlineDraftFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.overview",
            area = "Chat",
            title = "Chat overview sheet",
            description = "Overview sheet with tasks, background jobs, failed tool server, usage near limit, and changed files.",
        ) {
            SheetStoryFrame {
                ChatOverviewSheetContent(
                    state = sampleOverviewState(),
                    onAction = {},
                )
            }
        },
        Story(
            id = "chat.overview-stop",
            area = "Chat",
            title = "Stop background job dialog",
            description = "Confirming stopping a background job from overview while keeping the agent running.",
        ) {
            val overviewState = sampleOverviewState()
            val job = requireNotNull(overviewState.overview?.jobs?.firstOrNull())
            DialogStoryFrame(
                background = {
                    SheetStoryFrame {
                        ChatOverviewSheetContent(
                            state = overviewState,
                            onAction = {},
                        )
                    }
                },
            ) {
                ChatOverviewStopDialogContent(
                    job = job,
                    onConfirm = {},
                    onDismiss = {},
                )
            }
        },
        Story(
            id = "chat.settings",
            area = "Chat",
            title = "Chat settings sheet",
            description = "Agent settings sheet with model, effort, permission pickers, context meter, and commands.",
        ) {
            SheetStoryFrame {
                ChatSettingsSheetContent(
                    state = ChatSettingsState(
                        settings = sampleSettingsView(),
                        kind = Kind.ClaudeSdk,
                        context = ContextView(
                            usedTokens = 112_000L,
                            nearFull = false,
                            percent = 56L,
                            windowTokens = 200_000L,
                        ),
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
            val (state, source) = loadingChatFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.not-opened",
            area = "Chat",
            title = "Chat could not be opened",
            description = "Unavailable conversation screen explaining why the chat could not be opened.",
        ) {
            ChatUnavailableScreen(
                title = "spike-quic",
                reason = "This agent is no longer on studio.",
                onBack = {},
            )
        },
        Story(
            id = "chat.queued",
            area = "Chat",
            title = "Queued prompts",
            description = "Working turn with steered and queued follow-up prompts waiting above the composer.",
        ) {
            val (state, source) = queuedChatFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.signed-out",
            area = "Chat",
            title = "Provider signed out",
            description = "Conversation showing the provider sign-in notice in the composer footer.",
        ) {
            val (state, source) = signedOutChatFixture()
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.delete",
            area = "Chat",
            title = "Delete agent confirmation",
            description = "Confirming deletion of a settled agent and its worktree from its chat.",
        ) {
            val (state, source) = exitedChatFixture()
            val warning = DeleteWarning.Goes(
                WorkspaceGoes(
                    branch = "spike-quic",
                    branchCommits = 2,
                    uncommitted = ChangeTotals(files = 2, added = 34, removed = 9),
                ),
            )
            DialogStoryFrame(
                background = { ChatScreen(state = state, rowSource = source) },
            ) {
                ChatDeleteDialogContent(
                    agentName = "spike-quic",
                    descendants = 0,
                    warning = warning,
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
            val (state, source) = activeChatFixture()
            DialogStoryFrame(
                background = { ChatScreen(state = state, rowSource = source) },
            ) {
                ChatRenameDialogContent(
                    currentName = "refactor-auth",
                    initialDraft = "auth-token-expiry",
                    onDismiss = {},
                    onConfirm = {},
                )
            }
        },
        Story(
            id = "chat.large-text",
            area = "Chat",
            title = "Active conversation (large text)",
            description = "Active conversation rendered at 1.5x accessibility font scaling.",
            fontScale = 1.5f,
        ) {
            val (state, source) = activeChatFixture()
            ChatScreen(state = state, rowSource = source)
        },

        // --- Asks ----------------------------------------------------------
        Story(
            id = "chat.ask-command",
            area = "Asks",
            title = "Command permission ask",
            description = "Command permission card docked in place of the composer with scope choices.",
        ) {
            val (state, source) = askChatFixture(commandAskCard())
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.ask-edit",
            area = "Asks",
            title = "Edit permission ask",
            description = "Multi-hunk edit permission ask card showing added and removed diff lines.",
        ) {
            val (state, source) = askChatFixture(editAskCard())
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.ask-question",
            area = "Asks",
            title = "Question ask",
            description = "Structured question ask card with recommended option and Something else.",
        ) {
            val (state, source) = askChatFixture(questionAskCard())
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "ask.question-large-text",
            area = "Asks",
            title = "Question ask (large text)",
            description = "Structured question ask card docked over chat at 1.5x accessibility font scaling.",
            fontScale = 1.5f,
        ) {
            val (state, source) = askChatFixture(questionAskCard())
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.ask-plan",
            area = "Asks",
            title = "Plan approval ask",
            description = "Plan approval ask card with markdown plan body and mode switch toggle.",
        ) {
            val (state, source) = askChatFixture(planAskCard())
            ChatScreen(state = state, rowSource = source)
        },
        Story(
            id = "chat.ask-form",
            area = "Asks",
            title = "Tool-server form ask",
            description = "Schema-driven tool-server form ask with native controls and validation.",
        ) {
            val (state, source) = askChatFixture(formAskCard())
            ChatScreen(state = state, rowSource = source)
        },

        // --- Review --------------------------------------------------------
        Story(
            id = "review.page",
            area = "Review",
            title = "Diff review page",
            description = "Full-screen working-tree diff review with file chips, hunks, and an inline comment.",
        ) {
            ReviewPage(
                state = sampleReviewPageState(),
                onAction = {},
            )
        },
        Story(
            id = "review.no-changes",
            area = "Review",
            title = "Review (no changes)",
            description = "Diff review empty state when the selected comparison has no changes.",
        ) {
            ReviewPage(
                state = ReviewPageState(
                    doc = null,
                    comparison = Comparison.Uncommitted,
                    comparisons = listOf(Comparison.Uncommitted, Comparison.OnBranch),
                    baseBranch = "main",
                ),
                onAction = {},
            )
        },

        // --- Hosts ---------------------------------------------------------
        Story(
            id = "hosts.list",
            area = "Hosts",
            title = "Paired computers",
            description = "Paired computers grouped by reach (direct, relay, away, offline) with one nearby computer found.",
        ) {
            HostsTab(
                state = HostsTabState(
                    hosts = listOf(desk, garagePi, studioAway, oldMini),
                    discovered = listOf(laptopFound),
                    accountPro = false,
                    awayHostName = studioAway.name,
                    wentOfflineAtMs = mapOf(oldMini.hostId to (NOW_MS - 3 * 3_600_000L)),
                    nowMs = NOW_MS,
                ),
                onAction = {},
            )
        },
        Story(
            id = "hosts.empty",
            area = "Hosts",
            title = "No computers yet",
            description = "Computers tab when nothing is paired or discovered yet.",
        ) {
            HostsTab(
                state = HostsTabState(nowMs = NOW_MS),
                onAction = {},
            )
        },
        Story(
            id = "hosts.on-the-account-free",
            area = "Hosts",
            title = "Hosts (relay host on free plan)",
            description = "Computers tab on the free plan showing a computer on the account reachable only through the relay.",
        ) {
            HostsTab(
                state = HostsTabState(
                    hosts = listOf(desk),
                    discovered = listOf(studioRelay),
                    accountPro = false,
                    nowMs = NOW_MS,
                ),
                onAction = {},
            )
        },
        Story(
            id = "hosts.nearby-found",
            area = "Hosts",
            title = "Nearby computers found",
            description = "Unpaired computers discovered nearby shown above paired machines.",
        ) {
            HostsTab(
                state = HostsTabState(
                    hosts = listOf(desk, garagePi),
                    discovered = listOf(laptopOpen, studioFound),
                    accountPro = true,
                    nowMs = NOW_MS,
                ),
                onAction = {},
            )
        },
        Story(
            id = "hosts.large-text",
            area = "Hosts",
            title = "Paired computers (large text)",
            description = "Computers list rendered at 1.5x accessibility font scaling.",
            fontScale = 1.5f,
        ) {
            HostsTab(
                state = HostsTabState(
                    hosts = listOf(desk, garagePi, studioAway, oldMini),
                    discovered = listOf(laptopFound),
                    accountPro = true,
                    awayHostName = studioAway.name,
                    wentOfflineAtMs = mapOf(oldMini.hostId to (NOW_MS - 3 * 3_600_000L)),
                    nowMs = NOW_MS,
                ),
                onAction = {},
            )
        },
        Story(
            id = "host.page",
            area = "Hosts",
            title = "Computer details",
            description = "One computer's page: reach, platform, version, providers, copyable addresses, and key fingerprint.",
        ) {
            HostPage(
                state = HostPageState(
                    host = studio,
                    fingerprint = SAMPLE_HOST_FINGERPRINT,
                    nowMs = NOW_MS,
                    canOfferSignIn = true,
                    accountPro = true,
                ),
                onAction = {},
            )
        },

        // --- Pairing -------------------------------------------------------
        Story(
            id = "pair.code-empty",
            area = "Pairing",
            title = "Pairing PIN entry",
            description = "Six-digit code entry sheet for a nearby computer with an empty keypad.",
        ) {
            PairingStoryFrame {
                PairingSheetContent(
                    state = PairingSheetState(
                        digits = "",
                        phase = PairingPhaseUi.Entering,
                        machine = laptopOpen,
                        nowMs = NOW_MS,
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "pair.code-checking",
            area = "Pairing",
            title = "Pairing code checking",
            description = "All six digits typed and the pairing code being checked.",
        ) {
            PairingStoryFrame {
                PairingSheetContent(
                    state = PairingSheetState(
                        digits = "418920",
                        phase = PairingPhaseUi.Checking(viaLink = false),
                        machine = laptopOpen,
                        nowMs = NOW_MS,
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "pair.code-refused",
            area = "Pairing",
            title = "Pairing code refused",
            description = "Uniform refusal state after an invalid or expired code, ready for another entry.",
        ) {
            PairingStoryFrame {
                PairingSheetContent(
                    state = PairingSheetState(
                        digits = "",
                        phase = PairingPhaseUi.Refused(message = null, viaLink = false),
                        machine = laptopOpen,
                        nowMs = NOW_MS,
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "pair.code-subscribe",
            area = "Pairing",
            title = "Pairing needs Pro subscription",
            description = "A pairing code that reached a computer only the relay can carry, on the free plan.",
        ) {
            PairingStoryFrame {
                PairingSheetContent(
                    state = PairingSheetState(
                        digits = "418920",
                        phase = PairingPhaseUi.NeedsSubscription,
                        machine = studioRelay,
                        nowMs = NOW_MS,
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "pair.needs-account",
            area = "Pairing",
            title = "Pairing needs sign-in",
            description = "An invitation naming a computer only the relay can reach on a phone with no account.",
        ) {
            PairingStoryFrame {
                PairingSheetContent(
                    state = PairingSheetState(
                        phase = PairingPhaseUi.NeedsAccount(signingIn = false),
                        machine = studioRelay,
                        nowMs = NOW_MS,
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "pair.confirm",
            area = "Pairing",
            title = "Pairing confirmation",
            description = "Authenticated computer name, route, and full key fingerprint awaiting confirmation.",
        ) {
            val pending = PendingPair(
                token = List(16) { 9 },
                hostId = laptopOpen.hostId,
                name = "laptop",
                fingerprint = SAMPLE_HOST_FINGERPRINT,
                expiresAtMs = NOW_MS + 240_000L,
                via = RouteKind.Direct,
            )
            PairingStoryFrame {
                PairingSheetContent(
                    state = PairingSheetState(
                        phase = PairingPhaseUi.Confirming(pending = pending, platformName = "macOS"),
                        machine = laptopOpen,
                        nowMs = NOW_MS,
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "pair.trusted",
            area = "Pairing",
            title = "Pairing completed",
            description = "Confirmation state once the computer has been paired and trusted.",
        ) {
            PairingStoryFrame {
                PairingSheetContent(
                    state = PairingSheetState(
                        phase = PairingPhaseUi.Trusted(name = "laptop"),
                        machine = laptopOpen,
                        nowMs = NOW_MS,
                    ),
                    onAction = {},
                )
            }
        },

        // --- Settings ------------------------------------------------------
        Story(
            id = "settings.signed-in",
            area = "Settings",
            title = "Settings (signed in)",
            description = "Settings with signed-in account, appearance, notifications, privacy, diagnostics, and phone identity.",
        ) {
            SettingsScreen(
                state = sampleSignedInSettingsState(),
                onAction = {},
            )
        },
        Story(
            id = "settings.signed-out",
            area = "Settings",
            title = "Settings (signed out)",
            description = "Settings on a phone with no account signed in.",
        ) {
            SettingsScreen(
                state = sampleSignedInSettingsState().copy(
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
            description = "Subscribing to amux Pro: selectable monthly and yearly plans and purchase restoration.",
        ) {
            PaywallScreen(
                state = PaywallScreenState(
                    plans = listOf(
                        PaywallPlanUi(
                            id = "pro.monthly",
                            period = PaywallPlanPeriod.Monthly,
                            price = "$9 / month",
                        ),
                        PaywallPlanUi(
                            id = "pro.yearly",
                            period = PaywallPlanPeriod.Yearly,
                            price = "$84 / year",
                            saving = "Save 22%",
                        ),
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
            description = "Asking before an account is permanently deleted, with the address typed to confirm.",
        ) {
            SheetStoryFrame {
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
            description = "Settings rendered at 1.5x accessibility font scaling.",
            fontScale = 1.5f,
        ) {
            SettingsScreen(
                state = sampleSignedInSettingsState(),
                onAction = {},
            )
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
                    diagnosticDumpAttached = true,
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
                    diagnosticDumpAttached = true,
                    delivery = ReportDeliveryUi.Sent(referenceId = "RPT-4829-AF"),
                ),
                onAction = {},
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

    @Composable
    private fun PairingStoryFrame(content: @Composable () -> Unit) {
        SheetStoryFrame(content = content)
    }

    @Composable
    private fun DialogStoryFrame(
        background: @Composable () -> Unit,
        content: @Composable () -> Unit,
    ) {
        Box(modifier = Modifier.fillMaxSize()) {
            background()
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

    @Composable
    private fun SheetStoryFrame(content: @Composable () -> Unit) {
        val sheetShape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp)
        AmuxGround {
            AmuxScrim()
            Box(
                modifier = Modifier.fillMaxSize(),
                contentAlignment = Alignment.BottomCenter,
            ) {
                AmuxPanel(
                    shape = sheetShape,
                    color = AmuxTheme.colors.ground,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Box(
                            modifier = Modifier
                                .padding(top = Space.m, bottom = Space.xs)
                                .width(36.dp)
                                .height(4.dp)
                                .clip(CircleShape)
                                .background(AmuxTheme.colors.hairline),
                        )
                        content()
                    }
                }
            }
        }
    }

    private fun sampleChatSubject(
        name: String = "refactor-auth",
        hostName: String = "studio",
        reach: Reach = Reach.Online(RouteKind.Direct),
        native: Boolean = false,
        filing: Filing = Filing.Open,
    ): ChatSubject = ChatSubject(
        name = name,
        host = hostName,
        directory = "/Users/ada/source/amux",
        branch = "refactor-auth",
        reach = reach,
        filing = filing,
        topLevel = !native,
        native = native,
    )

    private fun sampleControls(): ControlsSummary = ControlsSummary(
        effort = "high",
        mode = null,
        model = "Opus 4.6",
        permission = null,
    )

    private fun activeChatFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val exploreRun = ScriptedChat.run(
            listOf(
                ScriptedChat.row(
                    id = "r-read-1",
                    order = 2L,
                    kind = RowKind.Explore(ExploreVerb.Read, "crates/auth/src/token.rs", CallPhase.Succeeded, ""),
                ),
                ScriptedChat.row(
                    id = "r-search-1",
                    order = 3L,
                    kind = RowKind.Explore(ExploreVerb.Search, "verify_signature", CallPhase.Succeeded, ""),
                ),
            ),
        ).map { ScriptedChat.folding(it, emptySet()) }

        val subGroup = SubagentGroup(
            first = "r-sub-1",
            agents = listOf(listOf(10), listOf(11), listOf(12), listOf(13)),
            open = false,
        )
        val groupState = GroupState(
            agents = 4,
            running = 2,
            needsYou = 1,
            idle = 0,
            finished = 1,
            ended = 0,
            failed = 0,
            unknown = 0,
        )

        val rows = listOf(
            ScriptedChat.row(
                id = "r-prompt-1",
                order = 1L,
                kind = RowKind.Prompt(
                    text = listOf(Segment.Text("Clean up token verification and add tests for expired keys.")),
                    steered = false,
                ),
            ),
        ) + exploreRun + listOf(
            ScriptedChat.row(
                id = "r-cmd-1",
                order = 4L,
                kind = RowKind.Command(
                    command = "cargo test -p amux-auth",
                    description = "Run auth unit suite",
                    state = CallPhase.Succeeded,
                    exitCode = 0,
                    durationMs = 4_200L,
                    outputHead = listOf(
                        "running 14 tests",
                        "test token::expired_key_rejected ... ok",
                        "test result: ok. 14 passed; 0 failed",
                    ),
                    outputTail = emptyList(),
                    outputTrimmed = false,
                    moreLines = 0L,
                ),
            ),
            ScriptedChat.row(
                id = "r-file-1",
                order = 5L,
                kind = RowKind.FileChange(
                    files = listOf(
                        FileRow(
                            path = "crates/auth/src/token.rs",
                            change = FileChangeView.Edited,
                            added = 28,
                            removed = 7,
                        ),
                    ),
                    state = CallPhase.Succeeded,
                ),
            ),
            ScriptedChat.row(
                id = "r-sub-1",
                order = 6L,
                kind = RowKind.Subagent(
                    description = "Audit token callers across mobile clients",
                    continued = false,
                    state = CallPhase.Running,
                    answer = "",
                ),
                opens = listOf(10),
                group = subGroup,
            ),
            ScriptedChat.row(
                id = "r-prose-1",
                order = 7L,
                kind = RowKind.Prose(
                    text = listOf(
                        Segment.Text(
                            "### Token verification updated\n\n" +
                                "Split expiry parsing out of `verify_signature` so callers get a typed reason:\n\n" +
                                "- Rejected keys past `expires_at` before signature checks\n" +
                                "- Added regression coverage in `token::tests`\n\n" +
                                "```rust\nif now > claims.expires_at {\n    return Err(TokenError::Expired);\n}\n```",
                        ),
                    ),
                    streaming = false,
                    workingNote = false,
                ),
            ),
            ScriptedChat.row(
                id = "r-turn-1",
                order = 8L,
                kind = RowKind.TurnEnd(durationMs = 42_000L, failed = false),
            ),
        )

        val changes = ChangeTotals(files = 2, added = 42, removed = 7)
        val frame = ScriptedChat.frame(
            name = "refactor-auth",
            phase = PhaseView.Idle,
            mode = Composer.Send,
        ).copy(
            controls = sampleControls(),
            git = Git(baseBranch = "main", uncommitted = changes, onBranch = changes),
        )
        val groupMap = mapOf("r-sub-1" to groupState)
        val state = ChatScreenState(
            subject = sampleChatSubject(),
            keys = rows.map { it.id },
            frame = frame,
            overview = Overview(
                jobs = emptyList(),
                failedServers = emptyList(),
                changes = null,
                tasks = TasksView(
                    done = 2,
                    total = 3,
                    current = "Verify mobile client callers",
                    entries = listOf(
                        TaskLine("Split expiry check", TaskMark.Done),
                        TaskLine("Add expired key tests", TaskMark.Done),
                        TaskLine("Verify mobile client callers", TaskMark.Current),
                    ),
                ),
                usageNearLimit = null,
            ),
            settings = sampleSettingsView(),
            groupStates = groupMap,
            changes = changes,
            comparisons = listOf(Comparison.Uncommitted, Comparison.OnBranch),
            canSend = false,
            nowMs = NOW_MS,
        )
        val source = StaticChatRowSource(rows = rows, groupStates = groupMap)
        return state to source
    }

    private fun streamingChatFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val rows = listOf(
            ScriptedChat.row(
                id = "s-1",
                order = 1L,
                kind = RowKind.Prompt(
                    text = listOf(Segment.Text("Diagnose why the reconnect test flaked on CI.")),
                    steered = false,
                ),
            ),
            ScriptedChat.row(
                id = "s-2",
                order = 2L,
                kind = RowKind.Command(
                    command = "cargo test -p e2e reconnect -- --nocapture",
                    description = "",
                    state = CallPhase.Running,
                    exitCode = null,
                    durationMs = null,
                    outputHead = listOf("spawning relay on 127.0.0.1:48211"),
                    outputTail = emptyList(),
                    outputTrimmed = false,
                    moreLines = 0L,
                ),
            ),
            ScriptedChat.row(
                id = "s-3",
                order = 3L,
                kind = RowKind.Prose(
                    text = listOf(
                        Segment.Text("Reproducing the timeout now. The second dial races the session ticket refresh when the relay restarts…"),
                    ),
                    streaming = true,
                    workingNote = false,
                ),
            ),
        )
        val frame = ScriptedChat.frame(
            name = "flaky-e2e",
            phase = PhaseView.Working,
            mode = Composer.Send,
            activity = Activity(
                kind = ActivityKind.Running("s-2"),
                sinceMs = NOW_MS - 18_000L,
                elapsedMs = 18_000L,
                turnElapsedMs = 18_000L,
            ),
        ).copy(controls = sampleControls())

        val state = ChatScreenState(
            subject = sampleChatSubject(name = "flaky-e2e", hostName = "desk"),
            keys = rows.map { it.id },
            frame = frame,
            activitySubject = "cargo test -p e2e reconnect -- --nocapture",
            settings = sampleSettingsView(),
            nowMs = NOW_MS,
        )
        return state to StaticChatRowSource(rows = rows)
    }

    private fun nativeSubagentFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val parentCard = fleetRow(
            index = 20,
            name = "migration",
            host = desk,
            attention = Attention.Working,
            minutesAgo = 2,
        ).card
        val rows = listOf(
            ScriptedChat.row(
                id = "n-1",
                order = 1L,
                kind = RowKind.Prompt(
                    text = listOf(Segment.Text("Scan every call site of `open_store` and list those missing WAL checkpointing.")),
                    steered = false,
                ),
            ),
            ScriptedChat.row(
                id = "n-2",
                order = 2L,
                kind = RowKind.Explore(ExploreVerb.Search, "open_store", CallPhase.Succeeded, ""),
            ),
            ScriptedChat.row(
                id = "n-3",
                order = 3L,
                kind = RowKind.Prose(
                    text = listOf(
                        Segment.Text("Found 3 call sites. Checking `crates/app-embedded/src/lib.rs` next."),
                    ),
                    streaming = true,
                    workingNote = false,
                ),
            ),
        )
        val frame = ScriptedChat.frame(
            name = "audit-store-calls",
            kind = Kind.ClaudeSdk,
            phase = PhaseView.Working,
            mode = Composer.Native(JobStop.Offered("job-sub-1")),
            activity = Activity(
                kind = ActivityKind.Working,
                sinceMs = NOW_MS - 14_000L,
                elapsedMs = 14_000L,
                turnElapsedMs = 14_000L,
            ),
        )
        val state = ChatScreenState(
            subject = sampleChatSubject(name = "audit-store-calls", hostName = "desk", native = true),
            family = FamilyHeader(
                children = emptyList(),
                attention = Attention.Working,
                needYou = 0,
                parent = parentCard,
            ),
            keys = rows.map { it.id },
            frame = frame,
            nowMs = NOW_MS,
        )
        return state to StaticChatRowSource(rows = rows)
    }

    private fun exitedChatFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val rows = listOf(
            ScriptedChat.row(
                id = "e-1",
                order = 1L,
                kind = RowKind.Prompt(
                    text = listOf(Segment.Text("Check whether QUIC holds up behind symmetric NATs.")),
                    steered = false,
                ),
            ),
            ScriptedChat.row(
                id = "e-2",
                order = 2L,
                kind = RowKind.Prose(
                    text = listOf(
                        Segment.Text("Verified relay fallback and hole-punching across both NAT profiles. Notes are in `docs/NETWORK.md`."),
                    ),
                    streaming = false,
                    workingNote = false,
                ),
            ),
            ScriptedChat.row(
                id = "e-3",
                order = 3L,
                kind = RowKind.TurnEnd(durationMs = 95_000L, failed = false),
            ),
        )
        val frame = ScriptedChat.frame(
            name = "spike-quic",
            phase = PhaseView.Exited(ExitCause.Finished),
            mode = Composer.Resume,
        ).copy(controls = sampleControls())
        val state = ChatScreenState(
            subject = sampleChatSubject(name = "spike-quic", hostName = "studio", filing = Filing.Closed),
            keys = rows.map { it.id },
            frame = frame,
            canResume = true,
            canSend = false,
            nowMs = NOW_MS,
        )
        return state to StaticChatRowSource(rows = rows)
    }

    private fun offlineDraftFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val rows = listOf(
            ScriptedChat.row(
                id = "o-1",
                order = 1L,
                kind = RowKind.Prompt(
                    text = listOf(Segment.Text("Trace the reconnect backoff when Wi-Fi drops.")),
                    steered = false,
                ),
            ),
            ScriptedChat.row(
                id = "o-2",
                order = 2L,
                kind = RowKind.Prose(
                    text = listOf(
                        Segment.Text("Added jittered exponential backoff capped at 30 seconds in `quic_link.rs`."),
                    ),
                    streaming = false,
                    workingNote = false,
                ),
            ),
        )
        val frame = ScriptedChat.frame(
            name = "reconnect-backoff",
            phase = PhaseView.Idle,
            mode = Composer.Waiting(Waiting.Detached),
            caughtUp = false,
        ).copy(
            connection = Connection.Reconnecting,
            controls = sampleControls(),
        )
        val state = ChatScreenState(
            subject = sampleChatSubject(
                name = "reconnect-backoff",
                hostName = "studio",
                reach = Reach.Away,
            ),
            keys = rows.map { it.id },
            frame = frame,
            draft = "Check whether a network change resets the backoff timer immediately.",
            canSend = false,
            nowMs = NOW_MS,
        )
        return state to StaticChatRowSource(rows = rows)
    }

    private fun subagentsGroupChatFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val group = SubagentGroup(
            first = "sg-1",
            agents = listOf(listOf(21), listOf(22), listOf(23), listOf(24)),
            open = true,
        )
        val groupState = GroupState(
            agents = 4,
            running = 2,
            needsYou = 1,
            idle = 0,
            finished = 1,
            ended = 0,
            failed = 0,
            unknown = 0,
        )
        val rows = listOf(
            ScriptedChat.row(
                id = "sg-prompt",
                order = 1L,
                kind = RowKind.Prompt(
                    text = listOf(Segment.Text("Audit every platform client for expired-token handling in parallel.")),
                    steered = false,
                ),
            ),
            ScriptedChat.row(
                id = "sg-1",
                order = 2L,
                kind = RowKind.Subagent(
                    description = "Check Android OAuth refresh retry path",
                    continued = false,
                    state = CallPhase.Running,
                    answer = "",
                ),
                opens = listOf(21),
                group = group,
            ),
            ScriptedChat.row(
                id = "sg-2",
                order = 3L,
                kind = RowKind.Subagent(
                    description = "Check iOS keychain token expiry guard",
                    continued = false,
                    state = CallPhase.Running,
                    answer = "",
                ),
                opens = listOf(22),
                group = group,
                parent = "sg-1",
            ),
            ScriptedChat.row(
                id = "sg-3",
                order = 4L,
                kind = RowKind.Subagent(
                    description = "Run relay token expiry integration test",
                    continued = false,
                    state = CallPhase.Running,
                    answer = "",
                ),
                opens = listOf(23),
                group = group,
                parent = "sg-1",
                attention = true,
            ),
            ScriptedChat.row(
                id = "sg-4",
                order = 5L,
                kind = RowKind.Subagent(
                    description = "Verify CLI credential reload on 401",
                    continued = false,
                    state = CallPhase.Succeeded,
                    answer = "CLI refreshes and retries once when the server returns 401.",
                    durationMs = 14_000L,
                ),
                opens = listOf(24),
                group = group,
                parent = "sg-1",
            ),
        )
        val openedAgents = mapOf(
            listOf(21) to OpenedAgent(
                agent = AgentKey(host = desk.hostId, agent = List(16) { 21 }),
                name = "android-oauth",
                kind = Kind.ClaudeSdk,
                phase = PhaseView.Working,
            ),
            listOf(22) to OpenedAgent(
                agent = AgentKey(host = desk.hostId, agent = List(16) { 22 }),
                name = "ios-keychain",
                kind = Kind.ClaudeSdk,
                phase = PhaseView.Working,
            ),
            listOf(23) to OpenedAgent(
                agent = AgentKey(host = desk.hostId, agent = List(16) { 23 }),
                name = "relay-expiry",
                kind = Kind.ClaudeSdk,
                phase = PhaseView.NeedsYou,
            ),
            listOf(24) to OpenedAgent(
                agent = AgentKey(host = desk.hostId, agent = List(16) { 24 }),
                name = "cli-reload",
                kind = Kind.ClaudeSdk,
                phase = PhaseView.Idle,
            ),
        )
        val groupMap = mapOf("sg-1" to groupState)
        val frame = ScriptedChat.frame(
            name = "migration",
            phase = PhaseView.Working,
            mode = Composer.Send,
            activity = Activity(
                kind = ActivityKind.Subagents(3),
                sinceMs = NOW_MS - 48_000L,
                elapsedMs = 48_000L,
                turnElapsedMs = 48_000L,
            ),
        ).copy(controls = sampleControls())
        val state = ChatScreenState(
            subject = sampleChatSubject(name = "migration", hostName = "desk"),
            keys = rows.map { it.id },
            frame = frame,
            groupStates = groupMap,
            settings = sampleSettingsView(),
            nowMs = NOW_MS,
        )
        val source = StaticChatRowSource(
            rows = rows,
            expanded = setOf("sg-1"),
            openedAgents = openedAgents,
            groupStates = groupMap,
        )
        return state to source
    }

    private fun subagentsFleetState(): AgentsHomeState {
        val parent = fleetRow(
            index = 4,
            name = "migration",
            host = desk,
            attention = Attention.NeedsYou,
            minutesAgo = 2,
            children = 2,
            secondLine = SecondLine.Step(
                ActivityLine(
                    activity = Activity(
                        kind = ActivityKind.Subagents(2),
                        sinceMs = NOW_MS - 90_000L,
                        elapsedMs = 90_000L,
                        turnElapsedMs = 90_000L,
                    ),
                    step = null,
                ),
            ),
        ).copy(expanded = true)
        val childWaiting = fleetRow(
            index = 41,
            name = "relay-expiry",
            host = desk,
            attention = Attention.NeedsYou,
            minutesAgo = 1,
            secondLine = SecondLine.Said(
                Said.Ask(
                    key = "",
                    wants = Wants.Run("cargo test -p relay token_expiry"),
                    why = "",
                    more = 0,
                ),
            ),
        ).let { row ->
            row.copy(
                card = row.card.copy(topLevel = false, native = true),
                depth = 1,
            )
        }
        val childWorking = fleetRow(
            index = 42,
            name = "android-oauth",
            host = desk,
            attention = Attention.Working,
            minutesAgo = 2,
            secondLine = SecondLine.Step(
                ActivityLine(
                    activity = Activity(
                        kind = ActivityKind.Running("read-1"),
                        sinceMs = NOW_MS - 22_000L,
                        elapsedMs = 22_000L,
                        turnElapsedMs = 22_000L,
                    ),
                    step = "AmuxCloudService.kt",
                ),
            ),
        ).let { row ->
            row.copy(
                card = row.card.copy(topLevel = false, native = true),
                depth = 1,
            )
        }
        val sibling = fleetRow(
            index = 6,
            name = "licensing",
            host = studio,
            attention = Attention.Idle,
            minutesAgo = 18,
            secondLine = SecondLine.LastSaid("Drafted the licensing page, ready for you to read."),
        )
        val needsYouRows = listOf(parent, childWaiting, childWorking)
        val liveRows = listOf(sibling)
        val sections = listOf(
            FleetSection(SectionKind.NeedsYou, families = 1, foldedByDefault = false, rows = needsYouRows),
            FleetSection(SectionKind.Live, families = 1, foldedByDefault = false, rows = liveRows),
        )
        return AgentsHomeState(
            loaded = true,
            rows = sections.flatMap { it.rows },
            sections = sections,
            needYou = 1,
            working = 1,
            machines = listOf(desk, studio),
            orderedAtMs = NOW_MS,
            accounts = listOf(adaAccount),
            selectedAccount = adaAccount,
            accountPro = true,
        )
    }

    private fun sampleSettledScreenState(): SettledScreenState = SettledScreenState(
        rows = listOf(
            fleetRow(
                index = 8,
                name = "spike-quic",
                host = studio,
                attention = Attention.Exited,
                minutesAgo = 180,
                filing = Filing.Closed,
                exitCause = ExitCause.Finished,
                secondLine = SecondLine.LastSaid("QUIC holds up behind both NATs."),
            ),
            fleetRow(
                index = 9,
                name = "bisect-hang",
                host = desk,
                attention = Attention.Exited,
                minutesAgo = 300,
                filing = Filing.Closed,
                exitCause = ExitCause.Failed("the provider exited: exit code 1"),
                secondLine = SecondLine.Exited(ExitCause.Failed("the provider exited: exit code 1")),
            ),
            fleetRow(
                index = 14,
                name = "release-0-10",
                host = desk,
                attention = Attention.Exited,
                minutesAgo = 1_440,
                filing = Filing.Closed,
                exitCause = ExitCause.Finished,
                secondLine = SecondLine.LastSaid("Published release notes and tag."),
            ),
        ),
        query = "",
        nowMs = NOW_MS,
    )

    private fun sampleNewAgentState(): NewAgentScreenState = NewAgentScreenState(
        hosts = listOf(desk, studio, garagePi),
        selectedHostId = desk.hostId,
        provider = Provider.Claude,
        availableProviders = listOf(Provider.Claude, Provider.Codex),
        offer = NewAgentOfferUi.Offered,
        settings = sampleSettingsView(),
        listing = NewAgentListingUi.Ready,
        repositories = listOf(
            Directory(path = "/Users/ada/source/amux", name = "amux"),
            Directory(path = "/Users/ada/source/relay", name = "relay"),
        ),
        recent = listOf(
            Directory(path = "/Users/ada/source/docs", name = "docs"),
            Directory(path = "/Users/ada/scratch/bench", name = "bench"),
        ),
        directory = "/Users/ada/source/amux",
        newWorktree = true,
        name = "auth-pass",
    )

    private fun sampleSignedInSettingsState(): SettingsScreenState = SettingsScreenState(
        accounts = listOf(
            SettingsAccountItem(
                id = "ada",
                name = "Ada",
                email = "ada@example.com",
                line = "3 hosts",
                signedIn = true,
                selected = true,
                entitlementBadge = SettingsEntitlementBadge.Pro,
                entitlementSummary = "Pro plan",
            ),
        ),
        appearance = SettingsAppearance.System,
        notificationStatus = SettingsNotificationStatus.Enabled,
        pushDeliveryComingSoon = true,
        shareUsage = true,
        autoReports = false,
        deviceName = "Pixel 9",
        deviceFingerprint = SAMPLE_PHONE_FINGERPRINT,
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

    private fun loadingChatFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val frame = ScriptedChat.frame(
            name = "auth-migration",
            phase = PhaseView.Starting,
            mode = Composer.Waiting(Waiting.CatchingUp),
            caughtUp = false,
        ).copy(
            connection = Connection.Connecting,
            controls = sampleControls(),
        )
        val state = ChatScreenState(
            subject = sampleChatSubject(
                name = "auth-migration",
                hostName = "desk",
            ).copy(connecting = true),
            keys = emptyList(),
            empty = true,
            frame = frame,
            loadingHint = true,
            canSend = false,
            nowMs = NOW_MS,
        )
        return state to StaticChatRowSource(rows = emptyList())
    }

    private fun queuedChatFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val (base, source) = streamingChatFixture()
        val queued = listOf(
            QueuedRow(
                inputId = listOf(11),
                text = listOf(Segment.Text("Capture the relay trace logs if the second attempt times out.")),
                steered = true,
                canWithdraw = true,
                canSendNow = true,
            ),
            QueuedRow(
                inputId = listOf(12),
                text = listOf(Segment.Text("Then run the full reconnect suite twice to check for flake.")),
                steered = false,
                canWithdraw = true,
                canSendNow = false,
            ),
        )
        val frame = requireNotNull(base.frame).copy(queue = queued)
        return base.copy(frame = frame) to source
    }

    private fun signedOutChatFixture(): Pair<ChatScreenState, StaticChatRowSource> {
        val (base, source) = activeChatFixture()
        val frame = requireNotNull(base.frame).copy(
            signIn = SignInView(
                state = SignInState.SignedOut,
                account = "ada@example.com",
                message = "Claude session expired on studio.",
            ),
        )
        return base.copy(frame = frame) to source
    }

    private fun askChatFixture(card: AskCard): Pair<ChatScreenState, StaticChatRowSource> {
        val rows = listOf(
            ScriptedChat.row(
                id = "a-1",
                order = 1L,
                kind = RowKind.Prompt(
                    text = listOf(Segment.Text("Update the store schema and verify every test passes.")),
                    steered = false,
                ),
            ),
            ScriptedChat.row(
                id = "a-2",
                order = 2L,
                kind = RowKind.Prose(
                    text = listOf(Segment.Text("I’ve prepared the change and need your confirmation before continuing.")),
                    streaming = false,
                    workingNote = false,
                ),
            ),
        )
        val frame = ScriptedChat.frame(
            name = "refactor-auth",
            phase = PhaseView.NeedsYou,
            mode = Composer.Send,
        ).copy(controls = sampleControls())
        val state = ChatScreenState(
            subject = sampleChatSubject(),
            keys = rows.map { it.id },
            frame = frame,
            ask = card,
            nowMs = NOW_MS,
        )
        return state to StaticChatRowSource(rows = rows)
    }

    private fun makeAskCard(
        key: String,
        body: AskBody,
        choices: List<Choice>,
        position: Long = 1L,
        count: Long = 1L,
    ): AskCard = AskCard(
        kind = Kind.ClaudeSdk,
        key = key,
        itemKey = "item-$key",
        position = position,
        count = count,
        body = body,
        choices = choices,
        questionNote = false,
        questionSkip = false,
        questionReply = false,
        stopsTurn = true,
        defaultToNo = false,
        state = CardState.Open,
    )

    private fun commandAskCard(): AskCard = makeAskCard(
        key = "ask-cmd",
        body = AskBody.Command(
            command = "cargo test -p store --locked",
            cwd = "~/source/amux",
            reason = "Run migration and WAL recovery tests",
            description = "",
        ),
        choices = listOf(
            Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
            Choice(
                ChoiceOutcome.AllowAlways(
                    subjects = listOf("cargo test"),
                    directories = emptyList(),
                    mode = "",
                    modeName = "",
                    scope = Scope.Project,
                ),
                primary = false,
                takesNote = false,
            ),
            Choice(ChoiceOutcome.Deny, primary = false, takesNote = true),
        ),
        position = 1L,
        count = 2L,
    )

    private fun editAskCard(): AskCard = makeAskCard(
        key = "ask-edit",
        body = AskBody.Edit(
            path = "crates/auth/src/token.rs",
            files = 1,
            added = 4,
            removed = 2,
            lines = listOf(
                EditLine.Hunk(oldStart = 42, newStart = 42),
                EditLine.Line(PatchLine(LineKind.Context, "pub fn verify(tok: &Token) -> Result<(), Error> {", number = 42)),
                EditLine.Line(PatchLine(LineKind.Removed, "    if tok.sig.is_empty() { return Ok(()); }", number = 43)),
                EditLine.Line(PatchLine(LineKind.Added, "    if tok.expires_at < now() {", number = 43)),
                EditLine.Line(PatchLine(LineKind.Added, "        return Err(Error::Expired);", number = 44)),
                EditLine.Line(PatchLine(LineKind.Added, "    }", number = 45)),
                EditLine.Hunk(oldStart = 88, newStart = 90),
                EditLine.Line(PatchLine(LineKind.Removed, "    check_hmac(tok)", number = 88)),
                EditLine.Line(PatchLine(LineKind.Added, "    check_ed25519(tok)", number = 90)),
            ),
            reason = "Enforce expiration and Ed25519 signature verification",
        ),
        choices = listOf(
            Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
            Choice(ChoiceOutcome.Deny, primary = false, takesNote = true),
        ),
    )

    private fun questionAskCard(): AskCard = makeAskCard(
        key = "ask-question",
        body = AskBody.Question(
            listOf(
                QuestionView(
                    header = "Retention",
                    question = "How long should closed chat sessions stay cached in memory?",
                    multiSelect = false,
                    options = listOf(
                        OptionView("5 minutes", "Matches iOS and keeps back-navigation instant", "", recommended = true),
                        OptionView("1 minute", "Lower memory footprint on busy fleets", "", recommended = false),
                        OptionView("Do not cache", "Re-open from the store on every visit", "", recommended = false),
                    ),
                    allowOther = true,
                    secret = false,
                ),
            ),
        ),
        choices = emptyList(),
    )

    private fun planAskCard(): AskCard = makeAskCard(
        key = "ask-plan",
        body = AskBody.Plan(
            title = "Migrate attachment uploads to streaming chunks",
            body = "1. Check `OpenableColumns.SIZE` before reading bytes.\n" +
                "2. Reject files above the 64 MB wire limit immediately.\n" +
                "3. Stream validated bytes into `sessionPutBlob`.",
        ),
        choices = listOf(
            Choice(ChoiceOutcome.ApprovePlan(mode = "", modeName = ""), primary = true, takesNote = false),
            Choice(ChoiceOutcome.ApprovePlan(mode = "acceptEdits", modeName = "Accept edits"), primary = false, takesNote = false),
            Choice(ChoiceOutcome.SendBack, primary = false, takesNote = true),
        ),
    )

    private fun formAskCard(): AskCard = makeAskCard(
        key = "ask-form",
        body = AskBody.Form(
            server = "deploy",
            message = "Confirm rollout settings for the staging relay",
            fields = listOf(
                FormField(
                    name = "region",
                    title = "Region",
                    description = "Primary relay region",
                    required = true,
                    kind = FormFieldKind.Choice(listOf("us-east", "eu-central", "ap-northeast")),
                    initial = FormValue.Choice(0),
                ),
                FormField(
                    name = "retries",
                    title = "Max retries",
                    description = "Between 1 and 10",
                    required = true,
                    kind = FormFieldKind.Number(integer = true, minimum = 1.0, maximum = 10.0),
                    initial = FormValue.Text("3"),
                ),
                FormField(
                    name = "dry_run",
                    title = "Dry run",
                    description = "Validate without shifting traffic",
                    required = false,
                    kind = FormFieldKind.Toggle,
                    initial = FormValue.Toggle(true),
                ),
            ),
        ),
        choices = listOf(
            Choice(ChoiceOutcome.Submit, primary = true, takesNote = false),
            Choice(ChoiceOutcome.Decline, primary = false, takesNote = false),
        ),
    )

    private fun sampleOverviewState(): ChatOverviewState = ChatOverviewState(
        subject = ChatSubjectUi(
            name = "refactor-auth",
            host = "studio",
            directory = "/Users/ada/source/amux",
            branch = "refactor-auth",
            address = "refactor-auth/studio",
        ),
        overview = Overview(
            tasks = TasksView(
                done = 2,
                total = 4,
                current = "Add expired key regression tests",
                entries = listOf(
                    TaskLine("Audit token callers", TaskMark.Done),
                    TaskLine("Split expiry validation", TaskMark.Done),
                    TaskLine("Add expired key regression tests", TaskMark.Current),
                    TaskLine("Update user guide", TaskMark.Todo),
                ),
            ),
            jobs = listOf(
                JobRow(
                    command = "cargo test -p amux-auth --watch",
                    startedAtMs = NOW_MS - 75_000L,
                    kind = BackgroundKind.Shell,
                    stop = JobStop.Offered("job-1"),
                ),
            ),
            failedServers = listOf(
                ServerView(name = "github", error = "OAuth token expired", needsAuth = true),
            ),
            usageNearLimit = UsageView(
                blocked = false,
                windows = listOf(
                    UsageWindowView(
                        label = UsageLabel.FiveHour,
                        state = UsageState.NearLimit,
                        resetsAtMs = NOW_MS + 3_600_000L,
                        usedPercent = 86.0,
                    ),
                ),
                credits = null,
            ),
            changes = Changes(
                totals = ChangeTotals(files = 2, added = 42, removed = 7),
                folders = listOf(
                    Folder(
                        path = "crates/auth/src/",
                        files = listOf(
                            ChangedFile("crates/auth/src/token.rs", "token.rs", added = 28, removed = 7, status = FileStatus.Modified, binary = false),
                        ),
                    ),
                    Folder(
                        path = "crates/auth/tests/",
                        files = listOf(
                            ChangedFile("crates/auth/tests/expiry.rs", "expiry.rs", added = 14, removed = 0, status = FileStatus.Added, binary = false),
                        ),
                    ),
                ),
            ),
        ),
        context = ContextView(
            usedTokens = 164_000L,
            nearFull = true,
            percent = 82L,
            windowTokens = 200_000L,
        ),
        comparisons = listOf(Comparison.Uncommitted, Comparison.OnBranch),
        comparison = Comparison.Uncommitted,
        baseBranch = "main",
        nowMs = NOW_MS,
    )

    private fun sampleSettingsView(): SettingsView = SettingsView(
        models = listOf(
            ModelChoice("opus", "Opus 4.6", "Deepest reasoning for complex tasks", listOf("low", "medium", "high"), current = true, unlisted = false, defaultEffort = "high"),
            ModelChoice("sonnet", "Sonnet 4.6", "Fast and balanced for everyday work", listOf("low", "medium", "high"), current = false, unlisted = false, defaultEffort = "medium"),
        ),
        efforts = listOf(
            EffortChoice("low", current = false, default = false, unlisted = false),
            EffortChoice("medium", current = false, default = false, unlisted = false),
            EffortChoice("high", current = true, default = true, unlisted = false),
        ),
        permissions = listOf(
            PermissionChoice("default", "Ask before editing", custom = false, current = true, unlisted = false, normal = true, neverAsks = false, settable = true),
            PermissionChoice("acceptEdits", "Accept edits", custom = false, current = false, unlisted = false, normal = false, neverAsks = false, settable = true),
            PermissionChoice("bypassPermissions", "Bypass permissions", custom = false, current = false, unlisted = false, normal = false, neverAsks = true, settable = true),
        ),
        modes = emptyList(),
        changeable = Changeability(
            model = Changeable.Pick,
            effort = Changeable.Pick,
            permission = Changeable.Pick,
            mode = Changeable.NotOffered,
        ),
        commands = listOf(
            CommandView("compact", "Compact the conversation context", "", "builtin", "/", anywhere = false),
            CommandView("review", "Review current changes", "[branch]", "builtin", "/", anywhere = false),
        ),
    )

    private fun sampleReviewPageState(): ReviewPageState = ReviewPageState(
        doc = ReviewDoc(
            base = "main",
            head = "4f2a9c1",
            added = 4,
            removed = 2,
            files = listOf(
                ReviewFile(
                    path = "crates/auth/src/token.rs",
                    status = FileStatus.Modified,
                    added = 3,
                    removed = 2,
                    binary = false,
                    comments = emptyList(),
                    hunks = listOf(
                        Hunk(
                            skips = true,
                            scope = "pub fn verify(tok: &Token) -> Result<(), Error>",
                            lines = listOf(
                                DiffLine(LineKind.Context, "pub fn verify(tok: &Token) -> Result<(), Error> {", emptyList(), newLine = 42, oldLine = 42),
                                DiffLine(LineKind.Removed, "    if tok.sig.is_empty() { return Ok(()); }", emptyList(), newLine = null, oldLine = 43),
                                DiffLine(LineKind.Added, "    if tok.expires_at < now() {", emptyList(), newLine = 43, oldLine = null),
                                DiffLine(LineKind.Added, "        return Err(Error::Expired);", emptyList(), newLine = 44, oldLine = null),
                                DiffLine(LineKind.Added, "    }", emptyList(), newLine = 45, oldLine = null),
                                DiffLine(LineKind.Removed, "    check_hmac(tok)", emptyList(), newLine = null, oldLine = 44),
                                DiffLine(LineKind.Context, "    check_ed25519(tok)", emptyList(), newLine = 46, oldLine = 45),
                            ),
                        ),
                    ),
                ),
                ReviewFile(
                    path = "crates/auth/tests/expiry.rs",
                    status = FileStatus.Added,
                    added = 1,
                    removed = 0,
                    binary = false,
                    comments = emptyList(),
                    hunks = listOf(
                        Hunk(
                            skips = false,
                            scope = "",
                            lines = listOf(
                                DiffLine(LineKind.Added, "#[test] fn expired_token_is_rejected() {}", emptyList(), newLine = 1, oldLine = null),
                            ),
                        ),
                    ),
                ),
            ),
        ),
        comments = listOf(
            ReviewComment(
                path = "crates/auth/src/token.rs",
                line = 44,
                oldLine = 0,
                text = "Also check clock skew tolerance of 30s so slightly drifted hosts stay connected.",
            ),
        ),
        comparison = Comparison.Uncommitted,
        comparisons = listOf(Comparison.Uncommitted, Comparison.OnBranch),
        baseBranch = "main",
        selectedFileIndex = 0,
    )

    private fun busyFleetState(
        away: Boolean = false,
        accounts: List<AccountUi> = listOf(adaAccount),
        includeNearby: Boolean = true,
    ): AgentsHomeState {
        val studioHost = if (away) studioAway else studio
        val pinned = listOf(
            fleetRow(
                index = 10,
                name = "nightly-triage",
                host = desk,
                attention = Attention.Idle,
                minutesAgo = 95,
                filing = Filing.Pinned,
                secondLine = SecondLine.LastSaid("Nothing new failed overnight."),
            ),
        )
        val needsYou = listOf(
            fleetRow(
                index = 1,
                name = "release-notes",
                host = desk,
                attention = Attention.NeedsYou,
                minutesAgo = 2,
                secondLine = SecondLine.Said(
                    Said.Ask(
                        key = "",
                        wants = Wants.Question("Which changes should the release notes keep?"),
                        why = "",
                        more = 0,
                    ),
                ),
            ),
            fleetRow(
                index = 2,
                name = "auth-refactor",
                host = studioHost,
                attention = Attention.NeedsYou,
                minutesAgo = 6,
                branch = "refactor-auth",
                secondLine = SecondLine.Said(
                    Said.Ask(
                        key = "",
                        wants = Wants.Run("cargo test -p amux-auth"),
                        why = "",
                        more = 0,
                    ),
                ),
            ),
        )
        val live = listOf(
            fleetRow(
                index = 3,
                name = "flaky-e2e",
                host = desk,
                attention = Attention.Working,
                minutesAgo = 1,
                kind = Kind.ClaudePty,
                secondLine = SecondLine.Step(
                    ActivityLine(
                        activity = Activity(
                            kind = ActivityKind.Running("t1"),
                            sinceMs = NOW_MS - 30_000L,
                            elapsedMs = 30_000L,
                            turnElapsedMs = 30_000L,
                        ),
                        step = "cargo test -p e2e reconnect",
                    ),
                ),
            ),
            fleetRow(
                index = 4,
                name = "migration",
                host = desk,
                attention = Attention.Working,
                minutesAgo = 3,
                children = 2,
                secondLine = SecondLine.Step(
                    ActivityLine(
                        activity = Activity(
                            kind = ActivityKind.Subagents(2),
                            sinceMs = NOW_MS - 90_000L,
                            elapsedMs = 90_000L,
                            turnElapsedMs = 90_000L,
                        ),
                        step = null,
                    ),
                ),
            ),
            fleetRow(
                index = 5,
                name = "docs-pass",
                host = studioHost,
                attention = Attention.Working,
                minutesAgo = 4,
                kind = Kind.Codex,
                secondLine = SecondLine.Step(
                    ActivityLine(
                        activity = Activity(
                            kind = ActivityKind.Thinking,
                            sinceMs = NOW_MS - 15_000L,
                            elapsedMs = 15_000L,
                            turnElapsedMs = 15_000L,
                        ),
                        step = null,
                    ),
                ),
            ),
            fleetRow(
                index = 6,
                name = "licensing",
                host = desk,
                attention = Attention.Idle,
                minutesAgo = 18,
                secondLine = SecondLine.LastSaid("Drafted the licensing page, ready for you to read."),
            ),
            fleetRow(
                index = 7,
                name = "sensor-logger",
                host = garagePi,
                attention = Attention.Idle,
                minutesAgo = 52,
                kind = Kind.Codex,
                secondLine = SecondLine.LastSaid("Running under systemd now."),
            ),
        )
        val settled = listOf(
            fleetRow(
                index = 8,
                name = "spike-quic",
                host = studioHost,
                attention = Attention.Exited,
                minutesAgo = 180,
                filing = Filing.Closed,
                exitCause = ExitCause.Finished,
                secondLine = SecondLine.LastSaid("QUIC holds up behind both NATs."),
            ),
            fleetRow(
                index = 9,
                name = "bisect-hang",
                host = desk,
                attention = Attention.Exited,
                minutesAgo = 300,
                filing = Filing.Closed,
                exitCause = ExitCause.Failed("the provider exited: exit code 1"),
                secondLine = SecondLine.Exited(ExitCause.Failed("the provider exited: exit code 1")),
            ),
        )
        val sections = listOf(
            FleetSection(SectionKind.Pinned, families = pinned.size, foldedByDefault = false, rows = pinned),
            FleetSection(SectionKind.NeedsYou, families = needsYou.size, foldedByDefault = false, rows = needsYou),
            FleetSection(SectionKind.Live, families = live.size, foldedByDefault = false, rows = live),
            FleetSection(SectionKind.Closed, families = settled.size, foldedByDefault = true, rows = settled),
        )
        return AgentsHomeState(
            loaded = true,
            rows = sections.flatMap { it.rows },
            sections = sections,
            needYou = 2,
            working = 3,
            machines = listOf(desk, studioHost, garagePi),
            discoveredHosts = if (includeNearby) listOf(laptopFound) else emptyList(),
            orderedAtMs = NOW_MS,
            accounts = accounts,
            selectedAccount = accounts.firstOrNull { it.selected },
            accountPro = !away,
            heldAwayHost = if (away) studioHost.name else null,
        )
    }

    private fun host(
        index: Int,
        name: String,
        platform: Platform,
        trusted: Boolean = true,
        candidate: Boolean = false,
        reach: Reach = Reach.Online(RouteKind.Direct),
        relayOnly: Boolean = false,
        pairingOpen: Boolean = false,
        addrs: List<String>? = null,
        version: String = "0.11.0",
    ): HostView = HostView(
        hostId = List(16) { index },
        name = name,
        local = false,
        trusted = trusted,
        candidate = candidate,
        reach = reach,
        addrs = addrs ?: if (candidate && !relayOnly) listOf("192.168.1.$index:4433") else emptyList(),
        relayOnly = relayOnly,
        pairingOpen = pairingOpen,
        current = true,
        providers = listOf(
            ProviderSignIn(Provider.Claude, signedIn = true),
            ProviderSignIn(Provider.Codex, signedIn = true),
        ),
        platform = platform,
        version = version,
    )

    private fun fleetRow(
        index: Int,
        name: String,
        host: HostView,
        attention: Attention,
        minutesAgo: Long,
        kind: Kind = Kind.ClaudeSdk,
        filing: Filing = Filing.Open,
        children: Int = 0,
        branch: String? = null,
        exitCause: ExitCause? = null,
        secondLine: SecondLine = SecondLine.Blank,
    ): FleetRow = FleetRow(
        card = FleetCard(
            agent = AgentKey(host = host.hostId, agent = List(16) { index }),
            name = name,
            handle = name,
            kind = kind,
            attention = attention,
            cwd = "/Users/ada/source/amux",
            phaseSinceMs = NOW_MS - minutesAgo * 60_000L,
            host = host.name,
            hostReach = host.reach,
            children = children,
            familyAttention = attention,
            members = 1 + children,
            descendants = children,
            backgroundJobs = 0,
            closeHeld = false,
            filing = filing,
            topLevel = true,
            native = false,
            branch = branch,
            exitCause = exitCause,
            repository = Repository(key = "amux", name = "amux", mark = "ax", tint = 0),
        ),
        depth = 0,
        expanded = false,
        secondLine = secondLine,
    )
}
