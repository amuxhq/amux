package sh.amux.testsupport

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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import sh.amux.design.AmuxGround
import sh.amux.design.AmuxPanel
import sh.amux.design.AmuxScrim
import sh.amux.design.AmuxTheme
import sh.amux.design.Space
import sh.amux.features.AccountUi
import sh.amux.features.AgentsHome
import sh.amux.features.AgentsHomeState
import sh.amux.features.HostsTab
import sh.amux.features.HostsTabState
import sh.amux.features.PairingPhaseUi
import sh.amux.features.PairingSheetContent
import sh.amux.features.PairingSheetState
import sh.amux.features.StoreFailureScreen
import sh.amux.push.Said
import sh.amux.push.Wants
import sh.amux.values.Activity
import sh.amux.values.ActivityKind
import sh.amux.values.ActivityLine
import sh.amux.values.AgentKey
import sh.amux.values.Attention
import sh.amux.values.ExitCause
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetFilter
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.HostView
import sh.amux.values.Kind
import sh.amux.values.Offline
import sh.amux.values.PendingPair
import sh.amux.values.Platform
import sh.amux.values.Provider
import sh.amux.values.ProviderSignIn
import sh.amux.values.Reach
import sh.amux.values.Repository
import sh.amux.values.RouteKind
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind

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

    private val desk = host(1, "desk", Platform.Macos)
    private val studio = host(2, "studio", Platform.Macos)
    private val studioAway = host(2, "studio", Platform.Macos, reach = Reach.Away)
    private val garagePi = host(3, "garage-pi", Platform.Linux, reach = Reach.Online(RouteKind.Relay))
    private val oldMini = host(7, "old-mini", Platform.Macos, reach = Reach.Offline(Offline.Plain))
    private val laptopFound = host(4, "laptop", Platform.Macos, trusted = false, candidate = true, pairingOpen = false)
    private val laptopOpen = host(4, "laptop", Platform.Macos, trusted = false, candidate = true, pairingOpen = true)
    private val studioFound = host(5, "studio", Platform.Macos, trusted = false, candidate = true, pairingOpen = false)

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

        // --- Hosts ---------------------------------------------------------
        Story(
            id = "hosts.paired",
            area = "Hosts",
            title = "Paired computers",
            description = "Paired computers grouped by reach: on this network, through the relay, away, and offline.",
        ) {
            HostsTab(
                state = HostsTabState(
                    hosts = listOf(desk, garagePi, studioAway, oldMini),
                    discovered = emptyList(),
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

        // --- Pairing -------------------------------------------------------
        Story(
            id = "pairing.pin-entering",
            area = "Pairing",
            title = "Pairing PIN entry",
            description = "Six-digit code entry sheet for a nearby computer with an open pairing window.",
        ) {
            PairingStoryFrame {
                PairingSheetContent(
                    state = PairingSheetState(
                        digits = "418",
                        phase = PairingPhaseUi.Entering,
                        machine = laptopOpen,
                        nowMs = NOW_MS,
                    ),
                    onAction = {},
                )
            }
        },
        Story(
            id = "pairing.confirm",
            area = "Pairing",
            title = "Pairing confirmation",
            description = "Authenticated computer name, route, and full key fingerprint awaiting confirmation.",
        ) {
            val pending = PendingPair(
                token = List(16) { 9 },
                hostId = laptopOpen.hostId,
                name = "laptop",
                fingerprint = "9f4b2c81d7e03a5612489bcdf013579a6c8e0241b5d7f93a1e2c4b6d8f0a3c5e",
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
            id = "pairing.refused",
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
    ): HostView = HostView(
        hostId = List(16) { index },
        name = name,
        local = false,
        trusted = trusted,
        candidate = candidate,
        reach = reach,
        addrs = if (candidate && !relayOnly) listOf("192.168.1.$index:4433") else emptyList(),
        relayOnly = relayOnly,
        pairingOpen = pairingOpen,
        current = true,
        providers = listOf(
            ProviderSignIn(Provider.Claude, signedIn = true),
            ProviderSignIn(Provider.Codex, signedIn = true),
        ),
        platform = platform,
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
