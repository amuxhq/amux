package sh.amux.shell

import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.text.AnnotatedString
import java.util.UUID
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
import sh.amux.core.runtime.FleetEvent
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.OpenChat
import sh.amux.core.runtime.RuntimeFailure
import sh.amux.core.runtime.StoreBundle
import sh.amux.design.AmuxTheme
import sh.amux.design.Appearance
import sh.amux.features.AskReviewTags
import sh.amux.features.ChatComposerTags
import sh.amux.features.ChatScreenTags
import sh.amux.features.TestTags
import sh.amux.values.ActOutcome
import sh.amux.values.AgentKey
import sh.amux.values.AskBody
import sh.amux.values.AskCard
import sh.amux.values.AskMoved
import sh.amux.values.Attention
import sh.amux.values.Base
import sh.amux.values.BlobRef
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
import sh.amux.values.Diff
import sh.amux.values.DiffBase
import sh.amux.values.DiffLine
import sh.amux.values.Draft
import sh.amux.values.DraftAttachment
import sh.amux.values.Empty
import sh.amux.values.FileStatus
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.FleetView
import sh.amux.values.FormField
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.FrozenReview
import sh.amux.values.Git
import sh.amux.values.HostView
import sh.amux.values.Hunk
import sh.amux.values.InputState
import sh.amux.values.KeysMoved
import sh.amux.values.Kind
import sh.amux.values.LineKind
import sh.amux.values.ModelChoice
import sh.amux.values.Overview
import sh.amux.values.PageOutcome
import sh.amux.values.PhaseView
import sh.amux.values.Platform
import sh.amux.values.QuestionResponse
import sh.amux.values.Reach
import sh.amux.values.ReviewComment
import sh.amux.values.ReviewDoc
import sh.amux.values.ReviewFile
import sh.amux.values.RouteKind
import sh.amux.values.Row
import sh.amux.values.RowKind
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind
import sh.amux.values.Segment
import sh.amux.values.SendOutcome
import sh.amux.values.SettingChange
import sh.amux.values.SettingsView
import sh.amux.words.EMPTY_GROUP_STATE

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ShellTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun switchesTabsPushesSettingsHidingBottomBarAndPopsBack() {
        val router = Router()
        val accounts = AccountRegistry()
        val stores = StoreBundle(account = AccountId("signed-out"))
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

        // Switch to Computers tab
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
        val accounts = AccountRegistry()
        val stores = StoreBundle(account = AccountId("signed-out"))
        val laptopId = HostId(UUID.fromString("44444444-4444-4444-4444-444444444444"))
        val laptop = HostView(
            hostId = laptopId.bytes,
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
        val beforeDiscovery = StateMappers.toPairingSheetState(
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
        val afterDiscovery = StateMappers.toPairingSheetState(
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
        val accounts = AccountRegistry()
        val abandoned = mutableListOf<sh.amux.values.PendingPair>()
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            abandonPairOp = { pending ->
                abandoned += pending
                Result.success(Unit)
            },
        )
        stores.fleet.show(FleetView(sections = emptyList(), needYou = 0, working = 0), hosts = emptyList())

        val hostId = HostId(UUID.fromString("66666666-6666-6666-6666-666666666666"))
        val desk = HostView(
            hostId = hostId.bytes,
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

        assertNull(router.snapshot.pairing)
        assertEquals(listOf(pending), abandoned)
    }

    @Test
    fun opensConversationSendsPromptAnswersAskOpensSheetsAndReviewsDiff() {
        val hostId = HostId(UUID.fromString("77777777-7777-7777-7777-777777777777"))
        val agentKey = AgentKey(host = hostId.bytes, agent = List(16) { 9 })
        val desk = HostView(
            hostId = hostId.bytes,
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
        val accounts = AccountRegistry()
        val stores = StoreBundle(
            account = AccountId("signed-out"),
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

        // Navigate to ReviewPage via header changes chip, add a comment, and Send Review
        composeRule.onNodeWithTag(ChatScreenTags.CHANGES).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SCREEN).assertIsDisplayed()
        composeRule.onNodeWithTag(AskReviewTags.reviewLine(0, 0, 1)).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_INPUT).performSemanticsAction(SemanticsActions.SetText) {
            it(AnnotatedString("Tighten error handling"))
        }
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_COMMENT_ADD).performClick()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SEND).assertIsDisplayed().performClick()
        composeRule.waitForIdle()

        // Back on ChatScreen with Review attached in composer (without calling stores.chat!)
        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()
        assertTrue(stores.holds(agentKey))
        assertTrue(stores.isShown(agentKey))
        assertEquals(1, stores.activeViewers(agentKey))
        composeRule.onNodeWithTag(ChatComposerTags.ATTACHMENTS_ROW).assertIsDisplayed()

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
        assertEquals(0, stores.activeViewers(agentKey))
        assertFalse(stores.isShown(agentKey))
    }

    private fun sampleDeskHost(uuid: String): Pair<HostId, HostView> {
        val hostId = HostId(UUID.fromString(uuid))
        val desk = HostView(
            hostId = hostId.bytes,
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

    @OptIn(ExperimentalCoroutinesApi::class)
    @Test
    fun conversationToChangesTransitionKeepsSessionAlivePastRetention() {
        val router = Router()
        val accounts = AccountRegistry()
        val (hostId, desk) = sampleDeskHost("88888888-8888-8888-8888-888888888888")
        val agentKey = AgentKey(
            host = hostId.bytes,
            agent = List(16) { 8 },
        )
        val agentRow = sampleFleetRow(agentKey, "desk-review")

        var nowMs = 1_000L
        var openCount = 0
        val dispatcher = StandardTestDispatcher()
        val testScope = TestScope(dispatcher)
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

        // Open Conversation
        composeRule.onNodeWithTag(TestTags.homeRow("desk-review")).performClick()
        composeRule.waitForIdle()
        assertEquals(1, openCount)
        assertEquals(1, stores.activeViewers(agentKey))
        assertFalse(fakeChat.closed)

        // Transition Conversation -> Changes: session stays active past retention window
        composeRule.onNodeWithTag(ChatScreenTags.CHANGES).performClick()
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SCREEN).assertIsDisplayed()
        nowMs += 10_000L
        testScope.advanceTimeBy(10_000L)
        testScope.runCurrent()
        assertFalse("Session must not close while viewing Changes", fakeChat.closed)
        assertTrue(stores.holds(agentKey))
        assertEquals(1, stores.activeViewers(agentKey))
        assertEquals(1, openCount)

        // Transition Changes -> Conversation: session still active past retention window
        composeRule.runOnIdle { router.pop() }
        composeRule.waitForIdle()
        composeRule.onNodeWithTag(TestTags.SCREEN_CONVERSATION).assertIsDisplayed()
        nowMs += 10_000L
        testScope.advanceTimeBy(10_000L)
        testScope.runCurrent()
        assertFalse("Session must not close after returning to Conversation", fakeChat.closed)
        assertTrue(stores.holds(agentKey))
        assertEquals(1, stores.activeViewers(agentKey))
        assertEquals(1, openCount)

        // Leave Conversation -> AgentsHome: lease drops to 0 and expires after retention
        composeRule.onNodeWithTag(TestTags.NAV_BACK).performClick()
        composeRule.waitForIdle()
        assertEquals(0, stores.activeViewers(agentKey))
        nowMs += 6_000L
        testScope.advanceTimeBy(6_000L)
        testScope.runCurrent()
        assertTrue("Session should close after retention once no route shows it", fakeChat.closed)
        assertFalse(stores.holds(agentKey))
    }

    @Test
    fun conversationReopensAutomaticallyWhenProfileVersionAdvances() {
        val router = Router()
        val accounts = AccountRegistry()
        val (hostId, _) = sampleDeskHost("99999999-9999-9999-9999-999999999999")
        val agentKey = AgentKey(host = hostId.bytes, agent = List(16) { 9 })
        var ready = false
        val fakeChat = FakeShellChat(agentKey)
        val stores = StoreBundle(
            account = AccountId("signed-out"),
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
    fun askActionsBindToRenderedAskKey() {
        val router = Router()
        val accounts = AccountRegistry()
        val (hostId, _) = sampleDeskHost("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        val agentKey = AgentKey(host = hostId.bytes, agent = List(16) { 10 })
        val fakeChat = FakeShellChat(agentKey)
        val stores = StoreBundle(
            account = AccountId("signed-out"),
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
    fun changesRouteRetryReacquiresChatLeaseAfterInitialFailure() {
        val router = Router()
        val accounts = AccountRegistry()
        val (hostId, _) = sampleDeskHost("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb")
        val agentKey = AgentKey(host = hostId.bytes, agent = List(16) { 11 })
        var ready = false
        var openAttempts = 0
        val fakeChat = FakeShellChat(agentKey)
        val stores = StoreBundle(
            account = AccountId("signed-out"),
            chatBridge = FakeShellChatBridge(),
            opener = {
                openAttempts += 1
                if (!ready) {
                    throw RuntimeFailure("host unreachable")
                }
                fakeChat
            },
        )
        router.push(Route.Changes(agent = agentKey, initialFile = null))

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

        composeRule.onNodeWithTag(AskReviewTags.REVIEW_ERROR).assertIsDisplayed()
        composeRule.onNodeWithText("host unreachable").assertIsDisplayed()
        assertEquals(1, openAttempts)

        ready = true
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_RETRY).performClick()
        composeRule.waitForIdle()
        assertEquals(2, openAttempts)
        composeRule.onNodeWithTag(AskReviewTags.REVIEW_SCREEN).assertIsDisplayed()
        composeRule.onNodeWithText("src/lib.rs").assertIsDisplayed()
    }

    private class FakeShellChatBridge : ChatBridge {
        override val attachmentLimit: Long = 64L * 1024L * 1024L
        override fun commandMatches(commands: List<CommandView>, draft: String): List<CommandMatch> = emptyList()
        override fun pastesAsAttachment(text: String): Boolean = false
        override fun formProblems(fields: List<FormField>, values: List<FormValue>): List<FormProblem> = emptyList()
        override fun reviewDoc(review: FrozenReview, comments: List<ReviewComment>): ReviewDoc = ReviewDoc(
            base = "main",
            head = review.diff.head,
            added = 1,
            removed = 1,
            files = listOf(
                ReviewFile(
                    path = "src/lib.rs",
                    status = FileStatus.Modified,
                    added = 1,
                    removed = 1,
                    binary = false,
                    comments = emptyList(),
                    hunks = listOf(
                        Hunk(
                            skips = false,
                            scope = "",
                            lines = listOf(
                                DiffLine(LineKind.Removed, "old()", emptyList(), newLine = null, oldLine = 1),
                                DiffLine(LineKind.Added, "new()", emptyList(), newLine = 1, oldLine = null),
                            ),
                        ),
                    ),
                ),
            ),
        )
    }

    private class FakeShellChat(private val agent: AgentKey) : OpenChat {
        override val id: Long = 42L
        var closed: Boolean = false
        val sent = mutableListOf<Draft>()
        val answeredChoices = mutableListOf<Pair<String, Int>>()

        private val initialRows = listOf(
            Row(
                id = "m1",
                order = 1L,
                atMs = 1_000L,
                kind = RowKind.Prose(
                    text = listOf(Segment.Text("Ready for the next step.")),
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
        override suspend fun review(comparison: Comparison): Result<FrozenReview> = Result.success(
            FrozenReview(
                diff = Diff(
                    head = "abc1234",
                    files = emptyList(),
                    base = DiffBase(Base.Head(Empty())),
                    patch = BlobRef(listOf(1, 2, 3), "patch", "text/x-diff", 10L),
                ),
                patch = "",
            ),
        )
        override suspend fun openOverview(comparison: Comparison): Result<Overview> =
            Result.success(Overview(emptyList(), emptyList(), null, null, null))
    }
}
