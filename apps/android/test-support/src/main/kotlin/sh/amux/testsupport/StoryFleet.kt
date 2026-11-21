package sh.amux.testsupport

import sh.amux.features.AccountUi
import sh.amux.features.AgentsHomeState
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
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.HostView
import sh.amux.values.Kind
import sh.amux.values.Platform
import sh.amux.values.Provider
import sh.amux.values.ProviderSignIn
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.SecondLine
import sh.amux.values.SectionKind

/**
 * The iOS catalogue's invented fleet (`StoryFleet` in
 * `apps/apple/Amux/Debug/Stories.swift`), value for value: four machines,
 * agents in every state, a family and a few the person closed, so the
 * fleet stories compare one to one with iOS's.
 */
internal object StoryFleet {
    private const val NOW_MS = CatalogFixtures.NOW_MS
    private const val MINUTE_MS = 60_000L

    val desk = host(1, "desk", Platform.Macos)
    val studio = host(2, "studio", Platform.Macos)
    val pi = host(3, "garage-pi", Platform.Linux)
    val laptop = host(4, "laptop", Platform.Macos, trusted = false, candidate = true)
    val studioFound = host(5, "studio", Platform.Macos, trusted = false, candidate = true)

    /** A machine on the account that only the relay reaches. */
    val onTheAccount = host(6, "nova-test", Platform.Linux, trusted = false, candidate = true, relayOnly = true)

    /** The laptop with its pairing window open, as `amux pair` holds it. */
    val laptopOpen = host(4, "laptop", Platform.Macos, trusted = false, candidate = true, pairingOpen = true)

    val ada = AccountUi(id = "ada", name = "Ada", email = "ada@example.com", line = "3 devices", signedIn = true, selected = true)
    val lovelace = AccountUi(id = "ada-work", name = "Lovelace Labs", email = "ada@lovelace.dev", line = "2 devices", signedIn = true, selected = false)

    /**
     * A day's agents.
     *
     * @param nearby whether a machine this phone has not paired is on its
     *   network, offering itself above the list.
     * @param subagents lists the migration's subagents under it, as a person
     *   who opened its family sees them.
     */
    fun busy(
        away: Boolean = false,
        accounts: Int = 1,
        nearby: Boolean = true,
        subagents: Boolean = false,
    ): AgentsHomeState {
        val studioReach: Reach = if (away) Reach.Away else Reach.Online(RouteKind.Direct)
        val studioHost = studio.copy(reach = studioReach)
        val pinned = listOf(
            row(10, "nightly-triage", desk, Attention.Idle, minutes = 95, filing = Filing.Pinned, second = SecondLine.LastSaid("Nothing new failed overnight.")),
        )
        val needs = listOf(
            row(
                1, "release-notes", desk, Attention.NeedsYou, minutes = 2,
                second = SecondLine.Said(Said.Ask(key = "", wants = Wants.Question("Which changes should the release notes keep?"), why = "", more = 0)),
            ),
            row(
                2, "auth-refactor", studioHost, Attention.NeedsYou, minutes = 6, reach = studioReach, branch = "refactor-auth",
                second = SecondLine.Said(Said.Ask(key = "", wants = Wants.Run("cargo test -p amux-auth"), why = "", more = 0)),
            ),
        )
        val family = if (subagents) {
            // Claude's subagents, by the titles Claude gave them; each its own
            // agent with no folder, listed under the parent that runs it.
            listOf(
                row(
                    12, "Port the schema to the new store", desk, Attention.Working, minutes = 3, handle = "sub-3f9a01c2", native = true,
                    second = SecondLine.Step(ActivityLine(activity(ActivityKind.Running("t2")), step = "cargo test -p store")),
                ),
                row(
                    13, "Check the old migrations still apply", desk, Attention.NeedsYou, minutes = 1, handle = "sub-81c4d2e7", native = true,
                    second = SecondLine.Said(Said.Ask(key = "", wants = Wants.Run("sqlite3 fleet.db .schema"), why = "", more = 0)),
                ),
            )
        } else {
            emptyList()
        }
        val live = listOf(
            row(
                3, "flaky-e2e", desk, Attention.Working, minutes = 1, kind = Kind.ClaudePty,
                second = SecondLine.Step(ActivityLine(activity(ActivityKind.Running("t1")), step = "cargo test -p e2e reconnect")),
            ),
            row(
                4, "migration", desk, Attention.Working, minutes = 3, children = 2, expanded = subagents,
                second = SecondLine.Step(ActivityLine(activity(ActivityKind.Subagents(2)), step = null)),
            ),
        ) + family + listOf(
            row(
                5, "docs-pass", studioHost, Attention.Working, minutes = 4, kind = Kind.Codex, reach = studioReach,
                second = SecondLine.Step(ActivityLine(activity(ActivityKind.Thinking), step = null)),
            ),
            row(6, "licensing", desk, Attention.Idle, minutes = 18, second = SecondLine.LastSaid("Drafted the licensing page; it's ready for you to read.")),
            row(7, "sensor-logger", pi, Attention.Idle, minutes = 52, kind = Kind.Codex, second = SecondLine.LastSaid("Running under systemd now.")),
        )
        val sections = listOf(
            FleetSection(SectionKind.Pinned, families = 1, foldedByDefault = false, rows = pinned),
            FleetSection(SectionKind.NeedsYou, families = 2, foldedByDefault = false, rows = needs),
            FleetSection(SectionKind.Live, families = 5, foldedByDefault = false, rows = live),
            FleetSection(SectionKind.Closed, families = 3, foldedByDefault = true, rows = settled(studioReach)),
        )
        val signedIn = listOf(ada, lovelace).take(accounts)
        return AgentsHomeState(
            loaded = true,
            rows = sections.flatMap { it.rows },
            sections = sections,
            needYou = 2,
            working = 3,
            machines = listOf(desk, studioHost, pi),
            discoveredHosts = if (nearby) listOf(laptop) else emptyList(),
            orderedAtMs = NOW_MS,
            accounts = signedIn,
            selectedAccount = signedIn.firstOrNull { it.selected },
            accountPro = if (signedIn.isEmpty()) null else !away,
            heldAwayHost = if (away) studioHost.name else null,
        )
    }

    /**
     * A settled agent says what it last said, as it did on the list; one
     * that failed says why.
     */
    fun settled(studioReach: Reach = Reach.Online(RouteKind.Direct)): List<FleetRow> = listOf(
        row(
            8, "spike-quic", studio.copy(reach = studioReach), Attention.Exited, minutes = 180, reach = studioReach, exit = ExitCause.Finished,
            filing = Filing.Closed, second = SecondLine.LastSaid("QUIC holds up behind both NATs. The notes are in spike/quic.md."),
        ),
        row(
            11, "readme-pass", desk, Attention.Exited, minutes = 240, exit = ExitCause.Ended, filing = Filing.Closed,
            second = SecondLine.LastSaid("Added a Getting started section to the README."),
        ),
        row(
            9, "bisect-hang", desk, Attention.Exited, minutes = 300, exit = ExitCause.Failed("the provider exited: exit code 1"), filing = Filing.Closed,
            second = SecondLine.Exited(ExitCause.Failed("the provider exited: exit code 1")),
        ),
    )

    fun noAgents(): AgentsHomeState = AgentsHomeState(
        loaded = true,
        machines = listOf(desk),
        orderedAtMs = NOW_MS,
        accounts = listOf(ada),
        selectedAccount = ada,
    )

    /** Nothing paired, with what this phone has found on its network. */
    fun firstRun(
        found: List<HostView> = listOf(laptop),
        accounts: Int = 1,
        pro: Boolean? = null,
    ): AgentsHomeState {
        val signedIn = listOf(ada, lovelace).take(accounts)
        return AgentsHomeState(
            loaded = true,
            machines = emptyList(),
            discoveredHosts = found,
            orderedAtMs = NOW_MS,
            accounts = signedIn,
            selectedAccount = signedIn.firstOrNull { it.selected },
            accountPro = pro,
        )
    }

    /** A machine; one nobody paired was found on this network unless only the relay reaches it. */
    fun host(
        index: Int,
        name: String,
        platform: Platform,
        trusted: Boolean = true,
        candidate: Boolean = false,
        relayOnly: Boolean = false,
        pairingOpen: Boolean = false,
    ): HostView = HostView(
        hostId = List(16) { index },
        link = 0L,
        sameNetwork = false,
        name = name,
        local = false,
        trusted = trusted,
        candidate = candidate,
        reach = Reach.Online(RouteKind.Direct),
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

    private fun activity(kind: ActivityKind): Activity =
        Activity(kind = kind, sinceMs = NOW_MS - MINUTE_MS, elapsedMs = MINUTE_MS, turnElapsedMs = MINUTE_MS)

    fun row(
        index: Int,
        name: String,
        host: HostView,
        attention: Attention,
        minutes: Long,
        kind: Kind = Kind.ClaudeSdk,
        reach: Reach = Reach.Online(RouteKind.Direct),
        children: Int = 0,
        branch: String? = null,
        exit: ExitCause? = null,
        filing: Filing = Filing.Open,
        expanded: Boolean = false,
        handle: String? = null,
        native: Boolean = false,
        second: SecondLine,
    ): FleetRow = FleetRow(
        card = FleetCard(
            agent = AgentKey(host = host.hostId, agent = List(16) { index }),
            name = name,
            handle = handle ?: name,
            kind = kind,
            attention = attention,
            cwd = if (native) "" else "/Users/ada/src/$name",
            phaseSinceMs = NOW_MS - minutes * MINUTE_MS,
            host = host.name,
            hostReach = reach,
            children = children,
            familyAttention = attention,
            members = children + 1,
            descendants = children,
            backgroundJobs = 0,
            closeHeld = false,
            filing = filing,
            topLevel = !native,
            native = native,
            branch = branch,
            exitCause = exit,
            repository = null,
        ),
        depth = if (native) 1 else 0,
        expanded = expanded,
        secondLine = second,
    )
}
