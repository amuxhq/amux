package sh.amux.shell

import androidx.compose.runtime.Recomposer
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.text.AnnotatedString
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AccountRegistry
import sh.amux.core.runtime.ChatBridge
import sh.amux.core.runtime.DeletionStore
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementSource
import sh.amux.core.runtime.FleetEvent
import sh.amux.core.runtime.FrozenFrame
import sh.amux.core.runtime.Grant
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.NewAgentBridge
import sh.amux.core.runtime.NewAgentSource
import sh.amux.core.runtime.OpenChat
import sh.amux.core.runtime.PairingInvitation
import sh.amux.core.runtime.PaywallStore
import sh.amux.core.runtime.Plan
import sh.amux.core.runtime.RemovalStore
import sh.amux.core.runtime.ReportCapture
import sh.amux.core.runtime.ReportReceipt
import sh.amux.core.runtime.ReportSending
import sh.amux.core.runtime.ReportState
import sh.amux.core.runtime.ReportStore
import sh.amux.core.runtime.RuntimeFailure
import sh.amux.core.runtime.SignInIntent
import sh.amux.core.runtime.SignInStore
import sh.amux.core.runtime.SignedInAccount
import sh.amux.core.runtime.StoreBundle
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.design.AppearanceMode
import sh.amux.features.AccountSheetTags
import sh.amux.features.AskReviewTags
import sh.amux.features.ReviewTags
import sh.amux.features.ChatComposerTags
import sh.amux.features.ChatScreenTags
import sh.amux.features.HostPageTags
import sh.amux.features.NewAgentTags
import sh.amux.features.PaywallTags
import sh.amux.features.ReportTags
import sh.amux.features.SettingsTags
import sh.amux.features.TestTags
import sh.amux.values.Offline
import sh.amux.values.AccountBinding
import sh.amux.values.AccountView
import sh.amux.values.ActOutcome
import sh.amux.values.AgentAct
import sh.amux.values.AgentKey
import sh.amux.values.AgentNameProblem
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.AskMoved
import sh.amux.values.Attention
import sh.amux.values.BlobRef
import sh.amux.values.Catalogue
import sh.amux.values.ChangeTotals
import sh.amux.values.Changeability
import sh.amux.values.Changeable
import sh.amux.values.ChatAgents
import sh.amux.values.ChatDelta
import sh.amux.values.ChatFrame
import sh.amux.values.Choice
import sh.amux.values.ChoiceOutcome
import sh.amux.values.CommandMatch
import sh.amux.values.CommandView
import sh.amux.values.Comparison
import sh.amux.values.Composer
import sh.amux.values.ComposerView
import sh.amux.values.Connection
import sh.amux.values.ControlsSummary
import sh.amux.values.Directories
import sh.amux.values.Directory
import sh.amux.values.Draft
import sh.amux.values.DraftAttachment
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.FleetView
import sh.amux.values.FormField
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.Git
import sh.amux.values.HostView
import sh.amux.values.Identity
import sh.amux.values.InputState
import sh.amux.values.KeysMoved
import sh.amux.values.Kind
import sh.amux.values.ModelChoice
import sh.amux.values.NewAgent
import sh.amux.values.NewAgentChoices
import sh.amux.values.NewAgentPick
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.PairedPeer
import sh.amux.values.PhaseView
import sh.amux.values.Platform
import sh.amux.values.ProfileView
import sh.amux.values.Provider
import sh.amux.values.QuestionResponse
import sh.amux.values.Reach
import sh.amux.values.RelayLink
import sh.amux.values.Roster
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind
import sh.amux.values.Segment
import sh.amux.values.SendOutcome
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView
import sh.amux.values.SignInOfferOutcome
import sh.amux.values.SignInOfferView
import sh.amux.values.ReviewOutcome
import sh.amux.words.EMPTY_GROUP_STATE
import sh.amux.values.ProsePart
import sh.amux.testsupport.plainMarkdown

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ShellTest {
    @get:Rule
    val composeRule = createComposeRule()

    // Stores live on the main thread, as the app's do.
    private val shellScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    @Test
    fun switchesTabsPushesSettingsHidingBottomBarAndPopsBack() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val stores = StoreBundle(account = AccountId("signed-out"), scope = shellScope)
        stores.fleet.show(FleetView(sections = emptyList(), needYou = 0, working = 0), hosts = emptyList())

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        // Starts on Agents with bottom bar visible
        composeRule.onNodeWithTag(TestTags.HOME_SCREEN).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.TAB_AGENTS).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.TAB_HOSTS).assertIsDisplayed()

        // Switch to Devices tab
        composeRule.onNodeWithTag(TestTags.TAB_HOSTS).performClick()
        composeRule.onNodeWithTag(TestTags.HOSTS_SCREEN).assertIsDisplayed()

        // Back to Agents, open account switcher, push Settings
        composeRule.onNodeWithTag(TestTags.TAB_AGENTS).performClick()
        composeRule.onNodeWithTag(TestTags.HOME_ACCOUNT_MENU).performClick()
        composeRule.onNodeWithTag(TestTags.ACCOUNTS_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.ACCOUNTS_SETTINGS).performClick()

        // Settings is full-screen and hides bottom bar
        composeRule.onNodeWithTag(TestTags.SCREEN_SETTINGS).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.TAB_AGENTS).assertDoesNotExist()

        // Back button returns to Agents and restores bottom bar
        composeRule.onNodeWithTag(TestTags.NAV_BACK).performClick()
        composeRule.onNodeWithTag(TestTags.HOME_SCREEN).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.TAB_AGENTS).assertIsDisplayed()
    }

    @Test
    fun autoRaisesPairingForOpenWindowAndDeclinesOnDismiss() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val stores = StoreBundle(account = AccountId("signed-out"), scope = shellScope)
        val laptopId = HostId(UUID.fromString("44444444-4444-4444-4444-444444444444"))
        val laptop = HostView(
            hostId = laptopId.bytes,
            link = 0L,
            sameNetwork = false,
            name = "laptop",
            local = false,
            trusted = false,
            candidate = true,
            reach = Reach.Online(RouteKind.Direct),
            addrs = listOf("192.168.1.4:4433"),
            relayOnly = false,
            pairingOpen = true,
            current = true,
            providers = emptyList(),
            platform = Platform.Macos,
        )
        stores.hosts.show(listOf(laptop))
        stores.fleet.show(FleetView(sections = emptyList(), needYou = 0, working = 0), hosts = listOf(laptop))

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        composeRule.waitForIdle()
        assertEquals(PairingModal.Code(laptopId), router.snapshot.pairing)
        composeRule.onNodeWithTag(TestTags.PAIRING_SHEET).assertIsDisplayed()

        // Closing the pairing sheet records declineWindow so it does not re-raise
        composeRule.onNodeWithTag(TestTags.PAIRING_CLOSE).performClick()
        composeRule.waitForIdle()
        assertNull(router.snapshot.pairing)
        assertTrue(laptopId in stores.hosts.snapshot.declinedWindows)
    }

    @Test
    fun addresslessQrInvitationUsesDiscoveredLanHostWithoutAccount() {
        val hostId = HostId(UUID.fromString("55555555-5555-5555-5555-555555555555"))
        val invitation = sh.amux.core.runtime.PairingInvitation(
            host = hostId,
            addrs = emptyList(),
            link = "https://amux.sh/p#placeholder",
        )
        val modal = PairingModal.Link(invitation)
        val pairing = sh.amux.core.runtime.PairingState()

        // Without discovery, an addressless QR invitation requires signing in.
        val beforeDiscovery = HostsMappers.toPairingSheetState(
            pairing = pairing,
            modal = modal,
            signedIn = false,
            signingIn = false,
            nowMs = 0L,
            discovered = emptyList(),
        )
        assertTrue(beforeDiscovery.phase is sh.amux.features.PairingPhaseUi.NeedsAccount)

        // Once local discovery finds that host (e.g. IPv6-only LAN), no account is needed.
        val discoveredHost = HostView(
            hostId = hostId.bytes,
            link = 0L,
            sameNetwork = false,
            name = "ipv6-desk",
            local = false,
            trusted = false,
            candidate = true,
            reach = Reach.Online(RouteKind.Direct),
            addrs = listOf("[2001:db8::1]:4433"),
            relayOnly = false,
            pairingOpen = true,
            current = true,
            providers = emptyList(),
            platform = Platform.Linux,
        )
        val afterDiscovery = HostsMappers.toPairingSheetState(
            pairing = pairing,
            modal = modal,
            signedIn = false,
            signingIn = false,
            nowMs = 0L,
            discovered = listOf(discoveredHost),
        )
        assertTrue(afterDiscovery.phase is sh.amux.features.PairingPhaseUi.Checking)
    }

    @Test
    fun closingConfirmingPairingSheetAbandonsPendingPair() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val abandoned = java.util.concurrent.CopyOnWriteArrayList<sh.amux.values.PendingPair>()
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            abandonPairOp = { pending ->
                abandoned += pending
                Result.success(Unit)
            },
        )
        stores.fleet.show(FleetView(sections = emptyList(), needYou = 0, working = 0), hosts = emptyList())

        val hostId = HostId(UUID.fromString("66666666-6666-6666-6666-666666666666"))
        val desk = HostView(
            hostId = hostId.bytes,
            link = 0L,
            sameNetwork = false,
            name = "desk",
            local = false,
            trusted = false,
            candidate = true,
            reach = Reach.Online(RouteKind.Direct),
            addrs = listOf("192.168.1.9:4433"),
            relayOnly = false,
            pairingOpen = true,
            current = true,
            providers = emptyList(),
            platform = Platform.Macos,
        )
        val pending = sh.amux.values.PendingPair(
            token = listOf(1, 2, 3, 4),
            hostId = hostId.bytes,
            name = "desk",
            fingerprint = "abcd 1234",
            expiresAtMs = 999_999L,
            via = RouteKind.Direct,
        )
        stores.pairing.open(machine = desk)
        val attempt = stores.pairing.checking()
        stores.pairing.reached(Result.success(pending), attempt = attempt)
        router.presentPairing(PairingModal.Code(hostId))

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.PAIRING_CONFIRM).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.PAIRING_CLOSE).performClick()
        composeRule.waitForIdle()
        composeRule.waitUntil(timeoutMillis = 2_000L) { abandoned.isNotEmpty() }

        assertNull(router.snapshot.pairing)
        assertEquals(listOf(pending), abandoned.toList())
    }

    @Test
    fun opensConversationSendsPromptAnswersAskOpensSheetsAndReviewsDiff() {
        val hostId = HostId(UUID.fromString("77777777-7777-7777-7777-777777777777"))
        val agentKey = AgentKey(host = hostId.bytes, agent = List(16) { 9 })
        val desk = HostView(
            hostId = hostId.bytes,
            link = 0L,
            sameNetwork = false,
            name = "desk",
            local = false,
            trusted = true,
            candidate = false,
            reach = Reach.Online(RouteKind.Direct),
            addrs = listOf("192.168.1.9:4433"),
            relayOnly = false,
            pairingOpen = false,
            current = true,
            providers = emptyList(),
            platform = Platform.Macos,
        )
        val agentRow = FleetRow(
            card = FleetCard(
                agent = agentKey,
                name = "desk-work",
                handle = "desk-work",
                kind = Kind.ClaudeSdk,
                attention = Attention.Idle,
                cwd = "/Users/ada/source/amux",
                phaseSinceMs = 1_000L,
                host = "desk",
                hostReach = Reach.Online(RouteKind.Direct),
                children = 0,
                familyAttention = Attention.Idle,
                members = 1,
                descendants = 0,
                backgroundJobs = 0,
                closeHeld = false,
                filing = Filing.Open,
                topLevel = true,
                native = false,
            ),
            depth = 0,
            expanded = false,
            secondLine = SecondLine.LastSaid("Ready for the next step."),
        )

        val fakeChat = FakeShellChat(agent = agentKey)
        val fakeBridge = FakeShellChatBridge()
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            chatBridge = fakeBridge,
            opener = { fakeChat },
        )
        stores.hosts.show(listOf(desk))
        stores.fleet.show(
            FleetView(
                sections = listOf(
                    FleetSection(
                        kind = SectionKind.Live,
                        families = 1,
                        foldedByDefault = false,
                        rows = listOf(agentRow),
                    ),
                ),
                needYou = 0,
                working = 0,
            ),
            hosts = listOf(desk),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        // Open conversation from AgentsHome
        composeRule.onNodeWithTag(TestTags.homeRow("desk-work")).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()
        composeRule.onNodeWithText("Ready for the next step.").assertIsDisplayed()

        // Type and send a prompt
        composeRule.onNodeWithTag(ChatComposerTags.INPUT_FIELD).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Run the tests"))
        }
        composeRule.onNodeWithTag(ChatComposerTags.SEND_BUTTON).performClick()
        composeRule.waitForIdle()
        assertEquals(1, fakeChat.sent.size)

        // Open Overview sheet from header title, then close
        composeRule.onNodeWithTag(ChatScreenTags.TITLE).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.OVERVIEW_CLOSE).performClick()
        composeRule.waitForIdle()

        // Open Settings sheet from overflow menu, then close
        composeRule.onNodeWithTag(ChatScreenTags.MORE).performClick()
        composeRule.onNodeWithTag(ChatScreenTags.MENU_SETTINGS).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.CHAT_SETTINGS_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.CHAT_SETTINGS_CLOSE).performClick()
        composeRule.waitForIdle()

        // Deliver an AskCard via runtime event stream and answer it
        val ask = AskCard(
            kind = Kind.ClaudeSdk,
            key = "ask-shell-1",
            itemKey = "item-1",
            position = 1L,
            count = 1L,
            body = AskBody.Command("cargo test", "", "Verify build", ""),
            choices = listOf(
                Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false),
                Choice(ChoiceOutcome.Deny, primary = false, takesNote = false),
            ),
            questionNote = false,
            questionSkip = false,
            questionReply = false,
            stopsTurn = true,
            defaultToNo = false,
            state = sh.amux.values.CardState.Open,
        )
        composeRule.runOnIdle {
            stores.moved(
                FleetEvent.Chat(
                    ChatDelta(
                        chat = fakeChat.id,
                        rows = emptyList(),
                        ask = AskMoved(card = ask),
                    ),
                ),
            )
        }
        composeRule.onNodeWithTag(AskReviewTags.ASK_CARD).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.askChoice(0)).performClick()
        composeRule.waitForIdle()
        assertEquals(listOf("ask-shell-1" to 0), fakeChat.answeredChoices)

        // Back returns cleanly to AgentsHome and releases viewer lease
        composeRule.onNodeWithTag(TestTags.NAV_BACK).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(TestTags.HOME_SCREEN).assertIsDisplayed()
    }

    private fun sampleDeskHost(uuid: String): Pair<HostId, HostView> {
        val hostId = HostId(UUID.fromString(uuid))
        val desk = HostView(
            hostId = hostId.bytes,
            link = 0L,
            sameNetwork = false,
            name = "desk",
            local = false,
            trusted = true,
            candidate = false,
            reach = Reach.Online(RouteKind.Direct),
            addrs = listOf("192.168.1.9:4433"),
            relayOnly = false,
            pairingOpen = false,
            current = true,
            providers = emptyList(),
            platform = Platform.Macos,
        )
        return hostId to desk
    }

    private fun sampleFleetRow(agentKey: AgentKey, name: String): FleetRow = FleetRow(
        card = FleetCard(
            agent = agentKey,
            name = name,
            handle = name,
            kind = Kind.ClaudeSdk,
            attention = Attention.Idle,
            cwd = "/Users/ada/source/amux",
            phaseSinceMs = 1_000L,
            host = "desk",
            hostReach = Reach.Online(RouteKind.Direct),
            children = 0,
            familyAttention = Attention.Idle,
            members = 1,
            descendants = 0,
            backgroundJobs = 0,
            closeHeld = false,
            filing = Filing.Open,
            topLevel = true,
            native = false,
        ),
        depth = 0,
        expanded = false,
        secondLine = SecondLine.LastSaid("Ready for the next step."),
    )

    /**
     * The review is pushed over its chat, whose route then leaves the screen:
     * the review holds the chat open, past the time a chat no route shows is
     * kept, so the chat's deltas keep carrying the page.
     */
    @OptIn(ExperimentalCoroutinesApi::class)
    @Test
    fun theChatStaysOpenUnderItsReviewPastRetention() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val (hostId, desk) = sampleDeskHost("88888888-8888-8888-8888-888888888888")
        val agentKey = AgentKey(host = hostId.bytes, agent = List(16) { 8 })
        val agentRow = sampleFleetRow(agentKey, "desk-review")

        var nowMs = 1_000L
        var openCount = 0
        val testScope = TestScope(StandardTestDispatcher())
        val fakeChat = FakeShellChat(agentKey)
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = testScope,
            clock = { nowMs },
            sessionRetentionMs = 5_000L,
            chatBridge = FakeShellChatBridge(),
            opener = {
                openCount += 1
                fakeChat
            },
        )
        stores.hosts.show(listOf(desk))
        stores.fleet.show(
            FleetView(
                sections = listOf(
                    FleetSection(kind = SectionKind.Live, families = 1, foldedByDefault = false, rows = listOf(agentRow)),
                ),
                needYou = 0,
                working = 0,
            ),
            hosts = listOf(desk),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(router = router, accounts = accounts, stores = stores, deviceName = "Pixel 9")
            }
        }

        composeRule.onNodeWithTag(TestTags.homeRow("desk-review")).performClick()
        composeRule.waitForIdle()
        testScope.runCurrent()
        composeRule.waitForIdle()
        assertEquals(1, openCount)

        // The chat's changes capsule pushes its review, which opens at the host.
        composeRule.onNodeWithTag(ChatScreenTags.CHANGES).performClick()
        composeRule.waitForIdle()
        testScope.runCurrent()
        composeRule.onNodeWithTag(ReviewTags.SCREEN).assertIsDisplayed()
        assertEquals(1, fakeChat.reviewOpens)
        nowMs += 10_000L
        testScope.advanceTimeBy(10_000L)
        testScope.runCurrent()
        assertFalse("the chat must not close while its review is shown", fakeChat.closed)
        assertEquals(1, openCount)

        // Back to the chat: the review page closes, the chat stays.
        composeRule.runOnIdle { router.pop() }
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()
        assertEquals(1, fakeChat.reviewCloses)
        nowMs += 10_000L
        testScope.advanceTimeBy(10_000L)
        testScope.runCurrent()
        assertFalse("the chat must not close after returning to it", fakeChat.closed)
        assertEquals(1, openCount)

        // Leaving the chat lets it go once the retention passes.
        composeRule.onNodeWithTag(TestTags.NAV_BACK).performClick()
        composeRule.waitForIdle()
        nowMs += 6_000L
        testScope.advanceTimeBy(6_000L)
        testScope.runCurrent()
        assertTrue("the chat closes after retention once no route shows it", fakeChat.closed)
        assertNull("an evicted chat is not held", stores.leaseCached(agentKey))
    }

    @Test
    fun conversationReopensAutomaticallyWhenProfileVersionAdvances() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val (hostId, _) = sampleDeskHost("99999999-9999-9999-9999-999999999999")
        val agentKey = AgentKey(host = hostId.bytes, agent = List(16) { 9 })
        var ready = false
        val fakeChat = FakeShellChat(agentKey)
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            chatBridge = FakeShellChatBridge(),
            opener = {
                if (!ready) {
                    throw RuntimeFailure("nothing is running for this account")
                }
                fakeChat
            },
        )
        router.push(Route.Conversation(agentKey))

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        composeRule.onNodeWithText("nothing is running for this account").assertIsDisplayed()

        // Once the profile attaches/updates, ConversationRouteContent re-acquires automatically
        composeRule.runOnIdle {
            ready = true
            stores.attach(null)
        }
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()
        composeRule.onNodeWithText("Ready for the next step.").assertIsDisplayed()
    }

    @Test
    fun agentsListKeepsItsScrollAcrossOpeningAChatAndComingBack() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val (hostId, desk) = sampleDeskHost("bbbbbbbb-1111-2222-3333-444444444444")
        val rows = List(40) { index ->
            sampleFleetRow(AgentKey(hostId.bytes, List(16) { index }), "agent-%02d".format(index))
        }
        val fakeChat = FakeShellChat(rows[30].card.agent)
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            chatBridge = FakeShellChatBridge(),
            opener = { fakeChat },
        )
        stores.hosts.show(listOf(desk))
        stores.fleet.show(
            FleetView(
                sections = listOf(FleetSection(SectionKind.Live, rows.size, false, rows)),
                needYou = 0,
                working = 0,
            ),
            hosts = listOf(desk),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(router = router, accounts = accounts, stores = stores, deviceName = "Pixel 9")
            }
        }

        composeRule.onNodeWithTag(TestTags.HOME_LIST).performScrollToNode(hasTestTag(TestTags.homeRow("agent-30")))
        composeRule.onNodeWithTag(TestTags.homeRow("agent-30")).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()

        composeRule.onNodeWithTag(TestTags.NAV_BACK).performClick()
        composeRule.waitForIdle()

        // Back on the list, the row that was opened is still in view and the
        // top of the list is still scrolled away.
        composeRule.onNodeWithTag(TestTags.homeRow("agent-30")).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.homeRow("agent-00")).assertDoesNotExist()
    }

    @Test
    fun aFleetChangeForAnotherAgentLeavesTheOpenChatAlone() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val (hostId, desk) = sampleDeskHost("cccccccc-1111-2222-3333-444444444444")
        val openKey = AgentKey(hostId.bytes, List(16) { 51 })
        val otherKey = AgentKey(hostId.bytes, List(16) { 52 })
        val openRow = sampleFleetRow(openKey, "open-agent")
        val otherRow = sampleFleetRow(otherKey, "other-agent")
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            chatBridge = FakeShellChatBridge(),
            opener = { FakeShellChat(openKey) },
        )
        fun showFleet(open: FleetRow, other: FleetRow) = stores.fleet.show(
            FleetView(
                sections = listOf(FleetSection(SectionKind.Live, 2, false, listOf(open, other))),
                needYou = 0,
                working = 0,
            ),
            hosts = listOf(desk),
        )
        stores.hosts.show(listOf(desk))
        showFleet(openRow, otherRow)
        router.push(Route.Conversation(openKey))

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(router = router, accounts = accounts, stores = stores, deviceName = "Pixel 9")
            }
        }
        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()
        composeRule.waitForIdle()

        val before = composeRule.runOnIdle { recompositions() }
        composeRule.runOnIdle {
            showFleet(openRow, otherRow.copy(secondLine = SecondLine.LastSaid("Something new elsewhere.")))
        }
        composeRule.waitForIdle()
        assertEquals("another agent's turn recomposed the open chat", before, composeRule.runOnIdle { recompositions() })

        // The open agent's own row moving does reach the chat, so the count
        // above is one that would have moved.
        composeRule.runOnIdle {
            showFleet(openRow.copy(card = openRow.card.copy(name = "renamed-agent")), otherRow)
        }
        composeRule.waitForIdle()
        assertTrue(composeRule.runOnIdle { recompositions() } > before)
    }

    /** How many times any composition in the test has recomposed and applied changes. */
    private fun recompositions(): Long = Recomposer.runningRecomposers.value.sumOf { it.changeCount }

    @Test
    fun askActionsBindToRenderedAskKey() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val (hostId, _) = sampleDeskHost("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        val agentKey = AgentKey(host = hostId.bytes, agent = List(16) { 10 })
        val fakeChat = FakeShellChat(agentKey)
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            chatBridge = FakeShellChatBridge(),
            opener = { fakeChat },
        )
        router.push(Route.Conversation(agentKey))

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        val askOne = AskCard(
            kind = Kind.ClaudeSdk,
            key = "ask-one",
            itemKey = "item-1",
            position = 1L,
            count = 1L,
            body = AskBody.Command("echo first", "", "First ask", ""),
            choices = listOf(Choice(ChoiceOutcome.AllowOnce, primary = true, takesNote = false)),
            questionNote = false,
            questionSkip = false,
            questionReply = false,
            stopsTurn = true,
            defaultToNo = false,
            state = sh.amux.values.CardState.Open,
        )
        val askTwo = askOne.copy(
            key = "ask-two",
            itemKey = "item-2",
            body = AskBody.Command("echo second", "", "Second ask", ""),
        )

        composeRule.runOnIdle {
            stores.moved(FleetEvent.Chat(ChatDelta(chat = fakeChat.id, rows = emptyList(), ask = AskMoved(card = askOne))))
        }
        composeRule.onNodeWithText("echo first").assertIsDisplayed()

        // Replace with ask-two before clicking
        composeRule.runOnIdle {
            stores.moved(FleetEvent.Chat(ChatDelta(chat = fakeChat.id, rows = emptyList(), ask = AskMoved(card = askTwo))))
        }
        composeRule.onNodeWithText("echo second").assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.askChoice(0)).performClick()
        composeRule.waitForIdle()
        assertEquals(listOf("ask-two" to 0), fakeChat.answeredChoices)
    }

    @Test
    fun newAgentRouteCreatesAgentPopsAndOpensConversation() {
        val (hostId, desk) = sampleDeskHost("cccccccc-cccc-cccc-cccc-cccccccccccc")
        val createdKey = AgentKey(host = hostId.bytes, agent = List(16) { 21 })
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val createdRequests = mutableListOf<NewAgent>()
        val fakeChat = FakeShellChat(createdKey)
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            chatBridge = FakeShellChatBridge(),
            opener = { fakeChat },
        )
        stores.hosts.show(listOf(desk))
        stores.fleet.show(
            FleetView(
                sections = listOf(
                    FleetSection(
                        kind = SectionKind.Live,
                        families = 1,
                        foldedByDefault = false,
                        rows = listOf(sampleFleetRow(AgentKey(hostId.bytes, List(16) { 3 }), "existing")),
                    ),
                ),
                needYou = 0,
                working = 0,
            ),
            hosts = listOf(desk),
        )
        val fakeNewAgentSource = object : NewAgentSource {
            override suspend fun hostCatalogue(host: HostId, provider: Provider): Result<Catalogue> =
                Result.success(Catalogue(emptyList(), emptyList(), emptyList(), emptyList(), emptyList(), null))

            override suspend fun directories(host: HostId, query: String, limit: Int): Result<Directories> =
                Result.success(
                    Directories(
                        repositories = listOf(Directory("/Users/ada/source/amux", "amux", null)),
                        recent = emptyList(),
                        history = emptyList(),
                    ),
                )

            override suspend fun pinFolder(host: HostId, path: String, pinned: Boolean): Result<Unit> =
                Result.success(Unit)

            override suspend fun createAgent(agent: NewAgent): Result<AgentKey> {
                createdRequests += agent
                return Result.success(createdKey)
            }
        }
        val fakeNewAgentBridge = object : NewAgentBridge {
            override fun newAgentSettings(catalogue: Catalogue, chosen: NewAgentChoices): SettingsView? = null
            override fun newAgentPick(catalogue: Catalogue, chosen: NewAgentChoices, pick: NewAgentPick): NewAgentChoices? = chosen
            override fun agentNameProblem(name: String): AgentNameProblem? = null
        }

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                    newAgentSourceProvider = { fakeNewAgentSource },
                    newAgentBridge = fakeNewAgentBridge,
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.HOME_FAB_NEW_AGENT).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(NewAgentTags.SCREEN).assertIsDisplayed()

        composeRule.onNodeWithTag(NewAgentTags.START_BUTTON).performClick()
        composeRule.waitForIdle()

        assertEquals(1, createdRequests.size)
        assertEquals("/Users/ada/source/amux", createdRequests.single().cwd)
        assertEquals(Route.Conversation(createdKey), router.snapshot.topRoute)
        assertFalse(Route.NewAgent in router.snapshot.currentStack)
        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()
    }

    @Test
    fun hostPageShowsRosterFingerprintPairsAgainAndRevokes() {
        val (hostId, paired) = sampleDeskHost("dddddddd-dddd-dddd-dddd-dddddddddddd")
        // Unpaired from its side: the page then offers to pair again.
        val desk = paired.copy(reach = Reach.Offline(Offline.Revoked))
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val stores = StoreBundle(account = AccountId("signed-out"), scope = shellScope)
        stores.hosts.show(listOf(desk))
        stores.hosts.showRoster(
            Roster(
                identity = Identity(List(16) { 1 }, "Pixel 9", "1111222233334444"),
                peers = listOf(PairedPeer(hostId.bytes, "desk", "abcdef1234567890")),
            ),
        )
        stores.fleet.show(FleetView(emptyList(), 0, 0), hosts = listOf(desk))
        router.selectTab(Tab.Hosts)
        router.push(Route.Host(hostId))

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        composeRule.onNodeWithTag(HostPageTags.SCREEN).assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.FINGERPRINT_ROW).performScrollTo().assertIsDisplayed()

        // Pair Again raises the pairing sheet
        composeRule.onNodeWithTag(HostPageTags.PAIR_AGAIN).performScrollTo().performClick()
        composeRule.waitForIdle()
        assertEquals(PairingModal.Code(hostId), router.snapshot.pairing)
        composeRule.onNodeWithTag(TestTags.PAIRING_CLOSE).performClick()
        composeRule.waitForIdle()

        // Revoke requires confirmation dialog and pops back to HostsTab
        composeRule.onNodeWithTag(HostPageTags.REVOKE).performScrollTo().performClick()
        composeRule.onNodeWithTag(HostPageTags.REVOKE_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(HostPageTags.REVOKE_CONFIRM).performClick()
        composeRule.waitForIdle()
        assertNull(router.snapshot.topRoute)
        composeRule.onNodeWithTag(TestTags.HOSTS_SCREEN).assertIsDisplayed()
    }

    @Test
    fun settledRouteUnsettlesAndOpensSettledAgents() {
        val (hostId, desk) = sampleDeskHost("eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee")
        val settledKey = AgentKey(hostId.bytes, List(16) { 33 })
        val settledRow = sampleFleetRow(settledKey, "old-migration").copy(
            card = sampleFleetRow(settledKey, "old-migration").card.copy(filing = Filing.Closed),
        )
        val liveRow = sampleFleetRow(AgentKey(hostId.bytes, List(16) { 34 }), "active-job")
        val acts = mutableListOf<Pair<AgentAct, AgentKey>>()
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            acting = { act, key ->
                acts += act to key
                Result.success(Unit)
            },
        )
        stores.hosts.show(listOf(desk))
        stores.fleet.show(
            FleetView(
                sections = listOf(
                    FleetSection(SectionKind.Live, 1, false, listOf(liveRow)),
                    FleetSection(SectionKind.Closed, 1, true, listOf(settledRow)),
                ),
                needYou = 0,
                working = 0,
            ),
            hosts = listOf(desk),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        composeRule.onNodeWithTag(TestTags.HOME_SETTLED_ROW).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(TestTags.SCREEN_SETTLED).assertIsDisplayed()
        composeRule.onNodeWithTag(TestTags.settledRow("old-migration")).assertIsDisplayed()

        composeRule.onNodeWithTag(TestTags.settledRow("old-migration")).performTouchInput { longClick() }
        composeRule.onNodeWithTag(TestTags.HOME_ACTION_UNSETTLE).performClick()
        composeRule.waitForIdle()
        assertEquals(listOf(AgentAct.Unfile to settledKey), acts)
    }

    @Test
    fun settingsPaywallAccountSheetsAndReportOverlayEmitActionsAndOverlayRoutes() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        accounts.show(
            listOf(
                ProfileView(
                    id = "prof-1",
                    label = "Ada",
                    subject = "sub-ada",
                    account = AccountView(
                        binding = AccountBinding.SignedIn,
                        email = "ada@example.com",
                        name = "Ada",
                        picture = "",
                        pro = false,
                        relay = RelayLink.Connected,
                    ),
                ),
            ),
        )
        val stores = StoreBundle(account = AccountId("sub-ada"), scope = shellScope)
        stores.fleet.show(FleetView(emptyList(), 0, 0), hosts = emptyList())
        stores.hosts.showRoster(
            Roster(
                identity = Identity(List(16) { 1 }, "Pixel 9", "deadbeefcafebabe"),
                peers = emptyList(),
            ),
        )

        val paywall = PaywallStore(
            plans = listOf(
                Plan(Plan.MONTHLY_ID, Plan.Period.Monthly, "$12"),
                Plan(Plan.YEARLY_ID, Plan.Period.Yearly, "$96", "Save 33%"),
            ),
        )
        val deletion = DeletionStore(
            asking = AccountId("sub-ada"),
            typed = "ada@example.com",
            phase = DeletionStore.Phase.Blocked(
                source = EntitlementSource.Web,
                manageUrl = "https://billing.example.com/portal",
            ),
        )
        val report = ReportStore(redactor = { frame, _ -> frame.png })
        val actions = mutableListOf<ShellAction>()

        router.push(Route.Settings)

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                    appVersion = "0.10.0",
                    appearanceMode = AppearanceMode.System,
                    paywall = paywall,
                    deletion = deletion,
                    report = report,
                    onAction = { actions += it },
                )
            }
        }

        // Delete account 409 blocked sheet renders and handles Try Again
        composeRule.onNodeWithTag(AccountSheetTags.DELETE_BLOCKED).performScrollTo().assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.DELETE_TRY_AGAIN).performScrollTo().performClick()
        assertTrue(ShellAction.RetryDeleteAccount in actions)
        composeRule.runOnIdle { deletion.dismiss() }
        composeRule.waitForIdle()

        // Settings screen emits appearance, privacy, export, and report actions
        composeRule.onNodeWithTag(SettingsTags.SCREEN).assertIsDisplayed()
        composeRule.onNodeWithTag(SettingsTags.APPEARANCE_DARK).performScrollTo().performClick()
        assertTrue(ShellAction.SetAppearance(AppearanceMode.Dark) in actions)

        composeRule.onNodeWithTag(SettingsTags.EXPORT_DIAGNOSTICS).performScrollTo().performClick()
        assertTrue(ShellAction.ExportDiagnostics in actions)

        // Screenshot offer banner opens ReportOverlay above Settings and sends report
        composeRule.runOnIdle {
            report.offer {
                ReportCapture(
                    frame = FrozenFrame(byteArrayOf(1, 2, 3), 412.0, 915.0, 2.0),
                    dump = CompletableDeferred(Result.failure(RuntimeFailure("none"))),
                    route = "settings",
                )
            }
        }
        composeRule.onNodeWithTag(ReportTags.OFFER_BANNER).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.OFFER_OPEN).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ReportTags.OVERLAY).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.SEND).performClick()
        assertTrue(ShellAction.SendReport in actions)
        composeRule.onNodeWithTag(ReportTags.CLOSE).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(ReportTags.OVERLAY).assertDoesNotExist()

        // Paywall route renders plans and emits subscribe action
        composeRule.runOnIdle { router.push(Route.Paywall(Tab.Agents)) }
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(PaywallTags.SCREEN).assertIsDisplayed()
        composeRule.onNodeWithTag(PaywallTags.SUBSCRIBE_BUTTON).performClick()
        assertTrue(ShellAction.PurchasePaywallPlan in actions)
    }

    @Test
    fun signInOfferSheetRendersIncomingOfferAndEmitsAcceptAndDecline() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val answered = mutableListOf<Pair<String, Boolean>>()
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            scope = shellScope,
            answerSignInOfferOp = { offer, accept ->
                answered += offer.email to accept
                Result.success(SignInOfferOutcome.Declined)
            },
        )
        stores.fleet.show(FleetView(emptyList(), 0, 0), hosts = emptyList())
        stores.moved(
            FleetEvent.SignInOffers(
                listOf(
                    SignInOfferView(
                        hostId = List(16) { 5 },
                        hostName = "studio",
                        email = "ada@example.com",
                        accountName = "Ada",
                    ),
                ),
            ),
        )

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_SHEET).assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_ACCEPT).performClick()
        composeRule.waitForIdle()
        assertEquals(listOf("ada@example.com" to true), answered)

        // Present a second offer and verify Decline.
        composeRule.runOnIdle {
            stores.moved(
                FleetEvent.SignInOffers(
                    listOf(
                        SignInOfferView(
                            hostId = List(16) { 6 },
                            hostName = "desk",
                            email = "grace@example.com",
                            accountName = "Grace",
                        ),
                    ),
                ),
            )
        }
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_OFFER_DECLINE).performClick()
        composeRule.waitForIdle()
        assertEquals(listOf("ada@example.com" to true, "grace@example.com" to false), answered)
    }

    @Test
    fun signInResultDialogRendersMismatchAndEmitsKeep() {
        val router = Router()
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        val stores = StoreBundle(account = AccountId("signed-out"), scope = shellScope)
        stores.fleet.show(FleetView(emptyList(), 0, 0), hosts = emptyList())
        val signIn = SignInStore(
            initialPhase = SignInStore.Phase.Mismatched(
                wanted = SignedInAccount(AccountId("sub-1"), "ada@example.com"),
                got = SignedInAccount(AccountId("sub-2"), "other@example.com"),
            ),
            initialIntent = SignInIntent.Returning(SignedInAccount(AccountId("sub-1"), "ada@example.com")),
        )
        val actions = mutableListOf<ShellAction>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                    signIn = signIn,
                    onAction = { actions += it },
                )
            }
        }

        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_RESULT_DIALOG).assertIsDisplayed()
        composeRule.onNodeWithTag(AccountSheetTags.SIGN_IN_RESULT_KEEP).performClick()
        assertTrue(ShellAction.KeepMismatchedSignIn in actions)
    }

    @Test
    fun deleteAccountStateIncludesBillingRenewalWarningWhenSubscriptionRenews() {
        val accounts = AccountRegistry(makeStores = { StoreBundle(account = it, scope = shellScope) })
        accounts.show(
            listOf(
                ProfileView(
                    id = "prof-1",
                    label = "Ada",
                    subject = "sub-ada",
                    account = AccountView(
                        binding = AccountBinding.SignedIn,
                        email = "ada@example.com",
                        name = "Ada",
                        picture = "",
                        pro = true,
                        relay = RelayLink.Connected,
                    ),
                ),
            ),
        )
        accounts.entitlement(
            Entitlement.Active(
                grant = Grant.Purchased(EntitlementSource.Web),
                renewsAtMs = 1_800_000_000_000L,
            ),
            forAccount = AccountId("sub-ada"),
        )
        val deletion = DeletionStore(asking = AccountId("sub-ada"))
        val ui = AccountMappers.toDeleteAccountSheetState(
            deletion = deletion.snapshot,
            registry = accounts.snapshot,
            formatRenewing = { date, place -> "Renews on $date through $place" },
        )
        assertNotNull(ui)
        assertTrue("expected non-null billing warning", !ui!!.billingWarning.isNullOrBlank())
        assertTrue(ui.billingWarning!!.startsWith("Renews on "))
        assertTrue(ui.billingWarning!!.endsWith("through amux.sh"))
    }

    @Test
    fun signedOutSelectedAccountGatesRelayPairingAndReportSendEvenWhenAnotherAccountIsSignedIn() {
        val scope = TestScope(StandardTestDispatcher())
        var pairCalls = 0
        val accounts = AccountRegistry(
            makeStores = { id ->
                StoreBundle(
                    account = id,
                    scope = scope,
                    beginPairOp = {
                        pairCalls += 1
                        Result.failure(RuntimeFailure("should not pair when selected profile is signed out"))
                    },
                )
            },
        )
        accounts.show(
            listOf(
                ProfileView(
                    id = "prof-out",
                    label = "Ada",
                    subject = "sub-out",
                    account = AccountView(
                        binding = AccountBinding.SignedOut,
                        email = "ada@example.com",
                        name = "Ada",
                        picture = "",
                        pro = false,
                        relay = RelayLink.Off,
                    ),
                ),
                ProfileView(
                    id = "prof-in",
                    label = "Grace",
                    subject = "sub-in",
                    account = AccountView(
                        binding = AccountBinding.SignedIn,
                        email = "grace@example.com",
                        name = "Grace",
                        picture = "",
                        pro = true,
                        relay = RelayLink.Connected,
                    ),
                ),
            ),
        )
        accounts.select(AccountId("sub-out"))
        val stores = accounts.stores!!

        val router = Router()
        val relayHost = HostId(UUID.fromString("33333333-4444-5555-6666-777777777777"))
        router.presentPairing(
            PairingModal.Link(
                PairingInvitation(
                    host = relayHost,
                    addrs = emptyList(),
                    link = "https://amux.sh/p#relay-only-test",
                ),
            ),
        )

        val report = ReportStore(redactor = { frame, _ -> frame.png })
        val actions = mutableListOf<ShellAction>()

        composeRule.setContent {
            AmuxTheme(appearance = Appearance.Light, photographed = true) {
                Shell(
                    router = router,
                    accounts = accounts,
                    stores = stores,
                    deviceName = "Pixel 9",
                    report = report,
                    onAction = { actions += it },
                )
            }
        }

        // Relay-only invitation must show the sign-in prompt and must NOT start pairing.
        composeRule.onNodeWithTag(TestTags.PAIRING_SIGN_IN).assertIsDisplayed()
        assertEquals(0, pairCalls)
        composeRule.onNodeWithTag(TestTags.PAIRING_SIGN_IN).performClick()
        assertTrue(ShellAction.SignIn in actions)

        // Report overlay on the signed-out selected profile must show the signed-out notice and not send.
        val capture = ReportCapture(
            frame = FrozenFrame(byteArrayOf(1, 2, 3), 400.0, 800.0, 2.0),
            dump = CompletableDeferred(Result.failure(RuntimeException("no dump"))),
            route = "agents",
            account = AccountId("sub-out"),
        )
        report.begin { capture }
        composeRule.waitForIdle()

        composeRule.onNodeWithTag(ReportTags.OVERLAY).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.SIGNED_OUT).assertIsDisplayed()
        composeRule.onNodeWithTag(ReportTags.SEND).assertIsNotEnabled()
    }

    private class FakeShellChatBridge : ChatBridge {
        override val attachmentLimit: Long = 64L * 1024L * 1024L
        override fun commandMatches(commands: List<CommandView>, draft: String): List<CommandMatch> = emptyList()
        override fun pastesAsAttachment(text: String): Boolean = false
        override fun formProblems(fields: List<FormField>, values: List<FormValue>): List<FormProblem> = emptyList()
    }

    private class FakeShellChat(private val agent: AgentKey) : OpenChat {
        override val id: Long = 42L
        var closed: Boolean = false
        val sent = mutableListOf<Draft>()
        val answeredChoices = mutableListOf<Pair<String, Int>>()
        var reviewOpens = 0
        var reviewCloses = 0

        override suspend fun openReview(baseBranch: String?, refresh: Boolean): ReviewOutcome? {
            reviewOpens += 1
            return ReviewOutcome.Done
        }

        override fun closeReview() {
            reviewCloses += 1
        }

        private val initialRows = listOf(
            Row(
                id = "m1",
                order = 1L,
                atMs = 1_000L,
                kind = RowKind.Prose(
                    text = listOf(ProsePart.Text(plainMarkdown("Ready for the next step."))),
                    streaming = false,
                    workingNote = false,
                ),
                collapsed = false,
                attention = false,
            ),
        )

        private val changes = ChangeTotals(files = 1, added = 1, removed = 1)
        private val initialFrame = ChatFrame(
            agent = agent,
            name = "desk-work",
            kind = Kind.ClaudeSdk,
            phase = PhaseView.Idle,
            composer = ComposerView(mode = Composer.Send, switchedAway = false, activity = null),
            controls = ControlsSummary(effort = null, mode = null, model = "Opus 4.6", permission = null),
            connection = Connection.Live,
            caughtUp = true,
            hasOlder = false,
            arrivalsHeld = false,
            queue = emptyList(),
            underway = emptyList(),
            refused = emptyList(),
            askInput = null,
            context = null,
            ended = null,
            git = Git(baseBranch = "main", uncommitted = changes, onBranch = changes),
            signIn = null,
        )

        private val initialSettings = SettingsView(
            models = listOf(
                ModelChoice("opus", "Opus 4.6", "Deep reasoning", emptyList(), current = true, unlisted = false),
            ),
            efforts = emptyList(),
            permissions = emptyList(),
            modes = emptyList(),
            changeable = Changeability(
                model = Changeable.Pick,
                effort = Changeable.NotOffered,
                permission = Changeable.NotOffered,
                mode = Changeable.NotOffered,
            ),
            commands = emptyList(),
        )

        override fun close() {
            closed = true
        }
        override fun opening(): ChatDelta = ChatDelta(
            chat = id,
            rows = emptyList(),
            agents = ChatAgents(emptyList(), emptyList(), EMPTY_GROUP_STATE),
            ask = AskMoved(card = null),
            frame = initialFrame,
            keys = KeysMoved.Reload(initialRows.map { it.id }),
            overview = Overview(emptyList(), emptyList(), null, null, null),
            settings = initialSettings,
        )
        override fun follow(following: Boolean) {}
        override fun rowsFor(keys: List<String>): List<Row> = initialRows.filter { it.id in keys }
        override suspend fun send(draft: Draft): Result<SendOutcome> {
            sent += draft
            return Result.success(SendOutcome(listOf(sent.size), InputState.Sent))
        }
        override suspend fun answer(ask: String, choice: Int, note: String?): ActOutcome {
            answeredChoices += ask to choice
            return ActOutcome.Done
        }
        override suspend fun answerQuestions(ask: String, responses: List<QuestionResponse>): ActOutcome = ActOutcome.Done
        override suspend fun replyInstead(ask: String, text: String, soFar: List<QuestionResponse>): ActOutcome = ActOutcome.Done
        override suspend fun answerForm(ask: String, values: List<FormValue>): ActOutcome = ActOutcome.Done
        override fun toggleRun(member: String) {}
        override fun toggleGroup(member: String) {}
        override fun foldTurns(on: Boolean) {}
        override fun toggleTurn(end: String) {}
        override suspend fun withdraw(inputId: List<Int>): ActOutcome = ActOutcome.Done
        override fun draftOf(inputId: List<Int>): Draft? = null
        override suspend fun sendNow(inputId: List<Int>): ActOutcome = ActOutcome.Done
        override suspend fun resend(inputId: List<Int>): SendOutcome? = null
        override fun discard(inputId: List<Int>) {}
        override suspend fun interrupt(): ActOutcome = ActOutcome.Done
        override suspend fun stopJob(key: String): ActOutcome = ActOutcome.Done
        override suspend fun changeSetting(setting: SettingChange): ActOutcome = ActOutcome.Done
        override suspend fun resume(draft: Draft): ActOutcome = ActOutcome.Done
        override suspend fun pageOlder(rows: Int): PageOutcome = PageOutcome.Arrived(0)
        override suspend fun putBlob(data: ByteArray, name: String, mime: String): Result<BlobRef> =
            Result.success(BlobRef(listOf(1), name, mime, data.size.toLong()))
        override fun blob(hash: List<Int>): ByteArray? = null
        override suspend fun openOverview(comparison: Comparison): Result<Overview> =
            Result.success(Overview(emptyList(), emptyList(), null, null, null))
    }
}
