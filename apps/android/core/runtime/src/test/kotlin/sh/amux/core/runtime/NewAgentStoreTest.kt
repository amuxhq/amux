package sh.amux.core.runtime

import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.values.AgentKey
import sh.amux.values.AgentNameProblem
import sh.amux.values.Attention
import sh.amux.values.Catalogue
import sh.amux.values.Directories
import sh.amux.values.Directory
import sh.amux.values.Filing
import sh.amux.values.FleetCard
import sh.amux.values.FleetRow
import sh.amux.values.Capabilities
import sh.amux.values.HostProvider
import sh.amux.values.HostView
import sh.amux.values.Kind
import sh.amux.values.NewAgent
import sh.amux.values.NewAgentPick
import sh.amux.values.OfferedMode
import sh.amux.values.OfferedModel
import sh.amux.values.OfferedPermission
import sh.amux.values.Offline
import sh.amux.values.Platform
import sh.amux.values.Reach
import sh.amux.values.RouteKind
import sh.amux.values.SecondLine

@OptIn(ExperimentalCoroutinesApi::class)
class NewAgentStoreTest {
    private val deskId = HostId(UUID.fromString("11111111-1111-1111-1111-111111111111"))
    private val studioId = HostId(UUID.fromString("22222222-2222-2222-2222-222222222222"))
    private val awayId = HostId(UUID.fromString("33333333-3333-3333-3333-333333333333"))
    private val agentUuid = UUID.fromString("99999999-9999-9999-9999-999999999999")

    private val codexCatalogue = Catalogue(
        models = listOf(
            OfferedModel(
                value = "gpt-6-astra",
                displayName = "GPT-6-Astra",
                description = "Fast reasoning model",
                efforts = listOf("low", "high"),
                resolvedModel = "gpt-6-astra",
                defaultEffort = "low",
            ),
            OfferedModel(
                value = "gpt-6-sol",
                displayName = "GPT-6-Sol",
                description = "Compact model",
                efforts = listOf("low"),
                resolvedModel = "gpt-6-sol",
                defaultEffort = "low",
            ),
        ),
        commands = emptyList(),
        permissions = listOf(
            OfferedPermission(
                value = "default",
                displayName = "Default",
                normal = true,
                neverAsks = false,
                settable = true,
                models = emptyList(),
                level = 0,
            ),
            OfferedPermission(
                value = "auto",
                displayName = "Auto",
                normal = false,
                neverAsks = false,
                settable = true,
                models = listOf("gpt-6-astra"),
                level = 0,
            ),
            OfferedPermission(
                value = "locked",
                displayName = "Locked",
                normal = false,
                neverAsks = false,
                settable = false,
                models = emptyList(),
                level = 0,
            ),
        ),
        modes = listOf(
            OfferedMode(value = "default", displayName = "Default", normal = true, settable = true),
            OfferedMode(value = "plan", displayName = "Plan", normal = false, settable = true),
        ),
        skills = emptyList(),
        newAgent = null,
    )

    private val claudeCatalogue = Catalogue(
        models = listOf(
            OfferedModel(
                value = "claude-sonnet",
                displayName = "Sonnet",
                description = "Balanced model",
                efforts = listOf("medium", "high"),
                resolvedModel = "claude-sonnet-4",
                defaultEffort = "medium",
            ),
        ),
        commands = emptyList(),
        permissions = listOf(
            OfferedPermission(
                value = "default",
                displayName = "Default",
                normal = true,
                neverAsks = false,
                settable = true,
                models = emptyList(),
                level = 0,
            ),
        ),
        modes = emptyList(),
        skills = emptyList(),
        newAgent = null,
    )

    private val sampleDirectories = Directories(
        recent = listOf(
            Directory(path = "/src/relay", name = "relay"),
            Directory(path = "/src/scratch", name = "scratch"),
        ),
        repositories = listOf(
            Directory(path = "/src/web", name = "web"),
            Directory(path = "/src/relay", name = "relay"),
        ),
        history = listOf(
            Directory(path = "/src/notes", name = "notes"),
            Directory(path = "/src/scratch", name = "scratch"),
        ),
    )

    private fun hostView(
        id: HostId,
        name: String,
        reach: Reach = Reach.Online(RouteKind.Direct),
        providers: List<HostProvider> = listOf(offered("claude", "Claude"), offered("codex", "Codex")),
    ): HostView = HostView(
        hostId = id.bytes,
        link = 0L,
        sameNetwork = false,
        name = name,
        local = false,
        trusted = true,
        candidate = false,
        reach = reach,
        addrs = emptyList(),
        relayOnly = false,
        pairingOpen = false,
        current = true,
        providers = providers,
        platform = Platform.Macos,
    )

    /** An agent a host lists, by its id and the name the host gives it. */
    private fun offered(provider: String, name: String, signedIn: Boolean? = true): HostProvider = HostProvider(
        provider = provider,
        name = name,
        signInHint = "",
        version = "",
        capabilities = Capabilities(
            stopsJob = true,
            switchesBack = false,
            attachable = false,
            resumable = true,
            sendsNow = true,
            images = true,
            files = true,
            passesUnknownCommands = true,
        ),
        signedIn = signedIn,
    )

    private fun fleetRow(
        host: HostId,
        name: String,
        cwd: String,
        kind: Kind = Kind.ClaudeSdk,
        phaseSinceMs: Long = 1000L,
    ): FleetRow = FleetRow(
        card = FleetCard(
            agent = AgentKey(host = host.bytes, agent = List(16) { 5 }),
            name = name,
            handle = name,
            kind = kind,
            provider = if (kind == Kind.Codex) "codex" else "claude",
            providerName = if (kind == Kind.Codex) "Codex" else "Claude",
            attachable = false,
            attention = Attention.Idle,
            cwd = cwd,
            phaseSinceMs = phaseSinceMs,
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
        secondLine = SecondLine.Blank,
    )

    private class FakeNewAgentSource : NewAgentSource {
        var catalogueHandler: suspend (HostId, String) -> Result<Catalogue> =
            { _, _ -> Result.failure(RuntimeFailure("no catalogue")) }
        var directoriesHandler: suspend (HostId, String, Int) -> Result<Directories> =
            { _, _, _ -> Result.failure(RuntimeFailure("no directories")) }
        var pinHandler: suspend (HostId, String, Boolean) -> Result<Unit> =
            { _, _, _ -> Result.success(Unit) }
        var createHandler: suspend (NewAgent) -> Result<AgentKey> =
            { Result.failure(RuntimeFailure("not configured")) }

        val directoryQueries = mutableListOf<Triple<HostId, String, Int>>()
        val pinCalls = mutableListOf<Triple<HostId, String, Boolean>>()
        val createdRequests = mutableListOf<NewAgent>()

        override suspend fun hostCatalogue(host: HostId, provider: String): Result<Catalogue> =
            catalogueHandler(host, provider)

        override suspend fun directories(host: HostId, query: String, limit: Int): Result<Directories> {
            directoryQueries.add(Triple(host, query, limit))
            return directoriesHandler(host, query, limit)
        }

        override suspend fun pinFolder(host: HostId, path: String, pinned: Boolean): Result<Unit> {
            pinCalls.add(Triple(host, path, pinned))
            return pinHandler(host, path, pinned)
        }

        override suspend fun createAgent(agent: NewAgent): Result<AgentKey> {
            createdRequests.add(agent)
            return createHandler(agent)
        }
    }

    @Test
    fun autoSelectsFirstReadyHostPrefillsRecentDirectoryAndDeduplicatesLists() = runTest {
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> Result.success(sampleDirectories) }
            catalogueHandler = { _, provider ->
                when (provider) {
                    "claude" -> Result.success(claudeCatalogue)
                    "codex" -> Result.success(codexCatalogue)
                    else -> Result.failure(RuntimeFailure("unknown"))
                }
            }
        }
        val store = NewAgentStore(source = source, scope = this)

        store.open(
            hosts = listOf(
                hostView(awayId, "away-box", reach = Reach.Away),
                hostView(deskId, "desk"),
                hostView(studioId, "studio"),
            ),
        )
        advanceUntilIdle()

        val s = store.snapshot
        assertEquals(deskId, s.machine)
        assertEquals(ListingState.Ready, s.listing)
        assertEquals("/src/relay", s.directory)
        assertEquals(listOf("web", "relay"), s.repositories.map { it.name })
        assertEquals(listOf("scratch"), s.recent.map { it.name })
        assertEquals(listOf("notes"), s.history.map { it.name })
        assertEquals("", s.name)
        assertNull(s.nameProblem)
        assertTrue(s.ready)
        assertEquals(
            NewAgent(
                hostId = deskId.bytes,
                provider = "claude",
                cwd = "/src/relay",
                name = "",
                effort = null,
                mode = null,
                model = null,
                newWorktree = null,
                permission = null,
            ),
            s.request,
        )
        store.close()
    }

    @Test
    fun realBridgeAppliesNewAgentSettingsAndPickRulesAndOnlyCodexCarriesMode() = runTest {
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> Result.success(sampleDirectories) }
            catalogueHandler = { _, provider ->
                if (provider == "codex") Result.success(codexCatalogue) else Result.success(claudeCatalogue)
            }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()

        store.selectProvider("codex")
        assertEquals(codexCatalogue, store.snapshot.catalogue)

        // Before picking a model, permissions scoped to a specific model are withheld and unsettable ones omitted.
        assertEquals(
            listOf("default"),
            store.snapshot.settings?.permissions?.map { it.value },
        )

        store.pick(NewAgentPick.Model("gpt-6-astra"))
        assertNull(store.snapshot.effort)
        assertEquals("low", store.snapshot.settings?.efforts?.firstOrNull { it.current }?.value)
        assertEquals(listOf("low", "high"), store.snapshot.settings?.efforts?.map { it.value })
        assertEquals(listOf("default", "auto"), store.snapshot.settings?.permissions?.map { it.value })
        assertEquals("default", store.snapshot.settings?.modes?.firstOrNull { it.current }?.value)

        store.pick(NewAgentPick.Effort("high"))
        store.pick(NewAgentPick.Permission("auto"))
        store.pick(NewAgentPick.Mode("plan"))
        store.setNewWorktree(true)

        assertEquals(
            NewAgent(
                hostId = deskId.bytes,
                provider = "codex",
                cwd = "/src/relay",
                name = "",
                effort = "high",
                mode = "plan",
                model = "gpt-6-astra",
                newWorktree = true,
                permission = "auto",
            ),
            store.snapshot.request,
        )

        // Picking a model that does not support "auto" resets permission to "default" and effort back to default.
        store.pick(NewAgentPick.Model("gpt-6-sol"))
        assertNull(store.snapshot.effort)
        assertEquals("default", store.snapshot.permission)

        // Picking the normal mode leaves mode unset.
        store.pick(NewAgentPick.Mode("default"))
        assertNull(store.snapshot.mode)

        // Switching to Claude resets provider-specific choices and strips mode from request.
        store.selectProvider("claude")
        assertNull(store.snapshot.model)
        assertNull(store.snapshot.effort)
        assertNull(store.snapshot.permission)
        assertNull(store.snapshot.mode)
        assertNull(store.snapshot.request?.mode)
        store.close()
    }

    @Test
    fun liveNameValidationUsesRealBridgeAndBlocksInvalidNames() = runTest {
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> Result.success(sampleDirectories) }
            catalogueHandler = { _, _ -> Result.success(claudeCatalogue) }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()

        // Blank name is optional.
        store.updateName("   ")
        assertNull(store.snapshot.nameProblem)
        assertTrue(store.snapshot.ready)
        assertEquals("", store.snapshot.request?.name)

        // Valid lowercase hyphenated name passes.
        store.updateName("fix-auth-2")
        assertNull(store.snapshot.nameProblem)
        assertTrue(store.snapshot.ready)
        assertEquals("fix-auth-2", store.snapshot.request?.name)

        // Uppercase or spaces trigger Characters problem and block submission.
        store.updateName("Bad Name")
        assertEquals(AgentNameProblem.Characters, store.snapshot.nameProblem)
        assertFalse(store.snapshot.ready)
        assertNull(store.snapshot.request)

        // Over 64 characters triggers TooLong.
        store.updateName("a".repeat(65))
        assertEquals(AgentNameProblem.TooLong, store.snapshot.nameProblem)
        assertFalse(store.snapshot.ready)

        // Clearing recovers readiness.
        store.updateName("")
        assertNull(store.snapshot.nameProblem)
        assertTrue(store.snapshot.ready)
        store.close()
    }

    @Test
    fun staleCatalogueAndDirectoryResponsesAreDroppedAfterHostSwitch() = runTest {
        val staleDirs = CompletableDeferred<Result<Directories>>()
        val staleCatalogue = CompletableDeferred<Result<Catalogue>>()
        val studioDirs = Directories(
            recent = listOf(Directory(path = "/studio/app", name = "app")),
            repositories = emptyList(),
        )

        val source = FakeNewAgentSource().apply {
            directoriesHandler = { host, _, _ ->
                if (host == deskId) staleDirs.await() else Result.success(studioDirs)
            }
            catalogueHandler = { host, _ ->
                if (host == deskId) staleCatalogue.await() else Result.success(claudeCatalogue)
            }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk"), hostView(studioId, "studio")))
        runCurrent()

        // Switch to studio while desk responses are still in flight.
        store.selectHost(studioId)
        advanceUntilIdle()
        assertEquals(studioId, store.snapshot.machine)
        assertEquals("/studio/app", store.snapshot.directory)
        assertEquals(OfferState.Offered(claudeCatalogue), store.snapshot.offers["claude"])

        // Late responses from desk must not overwrite studio's state.
        staleDirs.complete(Result.success(sampleDirectories))
        staleCatalogue.complete(Result.success(codexCatalogue))
        advanceUntilIdle()

        assertEquals(studioId, store.snapshot.machine)
        assertEquals("/studio/app", store.snapshot.directory)
        assertEquals(OfferState.Offered(claudeCatalogue), store.snapshot.offers["claude"])
        store.close()
    }

    /** The host's own folders: [sampleDirectories] with whatever it has pinned among the repositories. */
    private fun hostListing(pinned: Set<String>): Directories {
        val all = (sampleDirectories.repositories + sampleDirectories.recent + sampleDirectories.history.orEmpty())
            .distinctBy { it.path }
        return Directories(
            recent = sampleDirectories.recent.filter { it.path !in pinned },
            repositories = all.filter { it.path in pinned },
            history = sampleDirectories.history.orEmpty().filter { it.path !in pinned },
        )
    }

    @Test
    fun folderPinningMovesAtOnceAndARefusalSettlesOnTheHostsListing() = runTest {
        var failNextPin = false
        val hostPinned = mutableSetOf("/src/web", "/src/relay")
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> Result.success(hostListing(hostPinned)) }
            catalogueHandler = { _, _ -> Result.success(claudeCatalogue) }
            pinHandler = { _, path, pinned ->
                if (failNextPin) {
                    Result.failure(RuntimeFailure("disk read-only"))
                } else {
                    if (pinned) hostPinned.add(path) else hostPinned.remove(path)
                    Result.success(Unit)
                }
            }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()

        val notes = Directory(path = "/src/notes", name = "notes")
        assertFalse(store.snapshot.isPinned(notes.path))
        store.pinFolder(notes, pinned = true)
        assertTrue(store.snapshot.isPinned(notes.path))
        assertTrue(store.snapshot.history.none { it.path == notes.path })
        advanceUntilIdle()
        assertEquals(listOf(Triple(deskId, "/src/notes", true)), source.pinCalls)

        // A refused unpin says why, and the host's listing puts the folder back.
        failNextPin = true
        store.pinFolder(notes, pinned = false)
        assertFalse(store.snapshot.isPinned(notes.path))
        advanceUntilIdle()
        assertTrue(store.snapshot.isPinned(notes.path))
        assertEquals("disk read-only", store.snapshot.failure)
        store.close()
    }

    @Test
    fun aListingAskedForBeforeAPinCannotUndoIt() = runTest {
        val hostPinned = mutableSetOf("/src/web", "/src/relay")
        val held = CompletableDeferred<Unit>()
        val source = FakeNewAgentSource().apply {
            // Each listing is what the host held when it was asked for.
            directoriesHandler = { _, _, _ ->
                val asked = hostListing(hostPinned.toSet())
                held.await()
                Result.success(asked)
            }
            catalogueHandler = { _, _ -> Result.success(claudeCatalogue) }
            pinHandler = { _, path, pinned ->
                if (pinned) hostPinned.add(path) else hostPinned.remove(path)
                Result.success(Unit)
            }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        runCurrent()
        assertEquals(1, source.directoryQueries.size)

        val notes = Directory(path = "/src/notes", name = "notes")
        store.pinFolder(notes, pinned = true)
        runCurrent()
        held.complete(Unit)
        advanceUntilIdle()

        assertTrue("the listing from before the pin must not unpin it", store.snapshot.isPinned(notes.path))
        assertEquals("the host is asked again once the pin lands", 2, source.directoryQueries.size)
        assertEquals(ListingState.Ready, store.snapshot.listing)
        store.close()
    }

    @Test
    fun aPinRefusedDuringASearchStaysRefusedWhenTheSearchClears() = runTest {
        val filler = List(NewAgentStore.DIRECTORY_LIMIT) { i ->
            Directory(path = "/src/repo-$i", name = "repo-$i")
        }
        val notes = Directory(path = "/src/notes", name = "notes")
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, query, _ ->
                if (query.isEmpty()) {
                    Result.success(Directories(recent = filler, repositories = emptyList()))
                } else {
                    Result.success(Directories(recent = listOf(notes), repositories = emptyList()))
                }
            }
            catalogueHandler = { _, _ -> Result.success(claudeCatalogue) }
            pinHandler = { _, _, _ -> Result.failure(RuntimeFailure("disk read-only")) }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()
        store.updateQuery("notes")
        advanceUntilIdle()
        assertTrue(store.snapshot.searchesTheMachine)

        store.pinFolder(notes, pinned = true)
        assertTrue(store.snapshot.isPinned(notes.path))
        advanceUntilIdle()
        assertFalse(store.snapshot.isPinned(notes.path))
        assertEquals("disk read-only", store.snapshot.failure)

        store.updateQuery("")
        advanceUntilIdle()
        assertFalse("clearing the search must not bring the refused pin back", store.snapshot.isPinned(notes.path))
        store.close()
    }

    @Test
    fun cappedListingDebouncesRemoteSearchAndFallbackWorksWhenUnavailable() = runTest {
        val cappedRepos = List(NewAgentStore.DIRECTORY_LIMIT) { i ->
            Directory(path = "/src/repo-$i", name = "repo-$i")
        }
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, query, _ ->
                if (query.isEmpty()) {
                    Result.success(Directories(recent = emptyList(), repositories = cappedRepos))
                } else {
                    Result.success(
                        Directories(
                            recent = emptyList(),
                            repositories = listOf(Directory(path = "/src/match-$query", name = "match-$query")),
                        ),
                    )
                }
            }
            catalogueHandler = { _, _ -> Result.success(claudeCatalogue) }
        }
        val store = NewAgentStore(source = source, scope = this, searchDebounceMs = 200L)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()
        assertTrue(store.snapshot.capped)

        // Rapid typing cancels earlier search and only queries final text after debounce.
        store.updateQuery("a")
        advanceTimeBy(100L)
        store.updateQuery("amux")
        advanceTimeBy(199L)
        assertEquals(1, source.directoryQueries.size)
        advanceTimeBy(2L)
        advanceUntilIdle()
        assertEquals(2, source.directoryQueries.size)
        assertEquals("amux", source.directoryQueries.last().second)
        assertEquals(listOf("match-amux"), store.snapshot.repositories.map { it.name })

        // Typed path fallback works even when listing is unavailable.
        val failingSource = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> Result.failure(RuntimeFailure("offline")) }
            catalogueHandler = { _, _ -> Result.failure(RuntimeFailure("offline")) }
        }
        val fallbackStore = NewAgentStore(source = failingSource, scope = this)
        fallbackStore.open(
            hosts = listOf(hostView(deskId, "desk")),
            rows = listOf(fleetRow(deskId, "agent-1", "/Users/ada/work/api", kind = Kind.Codex, phaseSinceMs = 5000L)),
        )
        advanceUntilIdle()
        assertEquals(ListingState.Unavailable, fallbackStore.snapshot.listing)
        assertEquals("codex", fallbackStore.snapshot.provider)
        assertEquals("/Users/ada/work/api", fallbackStore.snapshot.directory)

        fallbackStore.updateTypedPath("  ~/custom/path  ")
        fallbackStore.useTypedPath()
        assertEquals("~/custom/path", fallbackStore.snapshot.directory)
        assertFalse(fallbackStore.snapshot.browsing)

        store.close()
        fallbackStore.close()
    }

    @Test
    fun submissionPreventsDoubleSubmitSurfacesErrorsAndClearsFailureOnHostRecovery() = runTest(StandardTestDispatcher()) {
        val gate = CompletableDeferred<Result<AgentKey>>()
        val createdKey = AgentKey(host = deskId.bytes, agent = HostId(agentUuid).bytes)
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> Result.success(sampleDirectories) }
            catalogueHandler = { _, _ -> Result.success(claudeCatalogue) }
            createHandler = { gate.await() }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()

        val first = async { store.submit() }
        runCurrent()
        assertTrue(store.snapshot.starting)
        assertFalse(store.snapshot.ready)

        // Second submit while busy is rejected immediately without hitting source twice.
        val second = store.submit()
        assertTrue(second.isFailure)
        assertEquals(1, source.createdRequests.size)

        // First fails -> error surfaced, starting cleared.
        gate.complete(Result.failure(RuntimeFailure("host is away")))
        val firstResult = first.await()
        assertTrue(firstResult.isFailure)
        assertFalse(store.snapshot.starting)
        assertEquals("host is away", store.snapshot.failure)

        // Host goes offline then back online -> stale offline failure clears.
        store.syncHosts(listOf(hostView(deskId, "desk", reach = Reach.Offline(Offline.Plain))))
        assertFalse(store.snapshot.ready)
        store.syncHosts(listOf(hostView(deskId, "desk", reach = Reach.Online(RouteKind.Direct))))
        assertNull(store.snapshot.failure)
        assertTrue(store.snapshot.ready)

        // Succeeding submit records created AgentKey.
        source.createHandler = { Result.success(createdKey) }
        val ok = store.submit()
        assertEquals(Result.success(createdKey), ok)
        assertEquals(createdKey, store.snapshot.created)
        assertNotNull(store.snapshot.created)
        store.close()
    }

    @Test
    fun cappedHostContinuesRemoteSearchAcrossShortResultsAndRestoresUnfilteredOnClear() = runTest {
        val cappedRepos = List(NewAgentStore.DIRECTORY_LIMIT) { i ->
            Directory(path = "/src/repo-$i", name = "repo-$i")
        }
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, query, _ ->
                if (query.isEmpty()) {
                    Result.success(Directories(recent = emptyList(), repositories = cappedRepos))
                } else {
                    Result.success(
                        Directories(
                            recent = emptyList(),
                            repositories = listOf(Directory(path = "/src/match-$query", name = "match-$query")),
                        ),
                    )
                }
            }
            catalogueHandler = { _, _ -> Result.success(claudeCatalogue) }
        }
        val store = NewAgentStore(source = source, scope = this, searchDebounceMs = 100L)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()
        assertTrue(store.snapshot.capped)
        assertEquals(NewAgentStore.DIRECTORY_LIMIT, store.snapshot.repositories.size)

        // First search returns < 200 matches; capped must remain true.
        store.updateQuery("amux")
        advanceTimeBy(101L)
        advanceUntilIdle()
        assertEquals(listOf("match-amux"), store.snapshot.repositories.map { it.name })
        assertTrue(store.snapshot.capped)
        assertTrue(store.snapshot.searchesTheMachine)

        // Second search must still query the host remotely.
        store.updateQuery("other")
        advanceTimeBy(101L)
        advanceUntilIdle()
        assertEquals(listOf("match-other"), store.snapshot.repositories.map { it.name })
        assertEquals(listOf("", "amux", "other"), source.directoryQueries.map { it.second })

        // Clearing query restores the full cached unfiltered listing.
        store.updateQuery("")
        advanceUntilIdle()
        assertEquals(NewAgentStore.DIRECTORY_LIMIT, store.snapshot.repositories.size)
        assertEquals("repo-0", store.snapshot.repositories.first().name)
        assertTrue(store.snapshot.capped)
        store.close()
    }

    @Test
    fun offlineToOnlineTransitionReloadsMissingDirectoriesAndCatalogues() = runTest {
        var online = false
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ ->
                if (online) Result.success(sampleDirectories) else Result.failure(RuntimeFailure("offline"))
            }
            catalogueHandler = { _, provider ->
                if (!online) {
                    Result.failure(RuntimeFailure("offline"))
                } else if (provider == "codex") {
                    Result.success(codexCatalogue)
                } else {
                    Result.success(claudeCatalogue)
                }
            }
        }
        val store = NewAgentStore(source = source, scope = this)

        // Open explicitly targeting an offline host: no initial fetches succeed.
        store.open(
            hosts = listOf(hostView(deskId, "desk", reach = Reach.Offline(Offline.Plain))),
            initialHost = deskId,
        )
        advanceUntilIdle()
        assertEquals(ListingState.None, store.snapshot.listing)
        assertTrue(store.snapshot.offers.isEmpty())

        // Host comes online -> store automatically fetches directories and catalogues.
        online = true
        store.syncHosts(listOf(hostView(deskId, "desk", reach = Reach.Online(RouteKind.Direct))))
        advanceUntilIdle()
        assertEquals(ListingState.Ready, store.snapshot.listing)
        assertEquals("/src/relay", store.snapshot.directory)
        assertEquals(OfferState.Offered(claudeCatalogue), store.snapshot.offers["claude"])
        assertEquals(OfferState.Offered(codexCatalogue), store.snapshot.offers["codex"])
        store.close()
    }

    @Test
    fun editingProviderSettingsLocksProviderAgainstSubsequentFleetUpdates() = runTest {
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> Result.success(sampleDirectories) }
            catalogueHandler = { _, provider ->
                if (provider == "codex") Result.success(codexCatalogue) else Result.success(claudeCatalogue)
            }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()
        assertEquals("claude", store.snapshot.provider)

        // Edit a Claude setting without tapping the provider picker.
        store.pick(NewAgentPick.Model("claude-sonnet"))
        assertEquals("claude-sonnet", store.snapshot.model)

        // A newer Codex fleet row must not hijack the draft mid-edit.
        store.remember(
            listOf(fleetRow(deskId, "codex-1", "/src/codex", kind = Kind.Codex, phaseSinceMs = 99_999L)),
        )
        assertEquals("claude", store.snapshot.provider)
        assertEquals("claude-sonnet", store.snapshot.model)
        store.close()
    }

    @Test
    fun aRefusedPinAmongSeveralSettlesOnlyWhereTheHostKeptIt() = runTest {
        val firstGate = CompletableDeferred<Result<Unit>>()
        val secondGate = CompletableDeferred<Result<Unit>>()
        val hostPinned = mutableSetOf("/src/web", "/src/relay")
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> Result.success(hostListing(hostPinned)) }
            catalogueHandler = { _, _ -> Result.success(claudeCatalogue) }
            pinHandler = { _, path, _ ->
                val answer = if (path == "/src/notes") firstGate.await() else secondGate.await()
                if (answer.isSuccess) hostPinned.add(path)
                answer
            }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        advanceUntilIdle()

        val notes = Directory(path = "/src/notes", name = "notes")
        val scratch = Directory(path = "/src/scratch", name = "scratch")

        store.pinFolder(notes, pinned = true)
        store.pinFolder(scratch, pinned = true)
        runCurrent()
        assertTrue(store.snapshot.isPinned("/src/notes"))
        assertTrue(store.snapshot.isPinned("/src/scratch"))

        // Second pin succeeds, then first pin is refused.
        secondGate.complete(Result.success(Unit))
        runCurrent()
        firstGate.complete(Result.failure(RuntimeFailure("notes read-only")))
        advanceUntilIdle()

        assertFalse(store.snapshot.isPinned("/src/notes"))
        assertTrue(store.snapshot.isPinned("/src/scratch"))
        assertTrue(store.snapshot.history.any { it.path == "/src/notes" })
        assertEquals("notes read-only", store.snapshot.failure)
        store.close()
    }

    @Test
    fun closeCancelsInFlightRequestsWithoutWritingUnavailableState() = runTest {
        val dirsGate = CompletableDeferred<Result<Directories>>()
        val catGate = CompletableDeferred<Result<Catalogue>>()
        val source = FakeNewAgentSource().apply {
            directoriesHandler = { _, _, _ -> dirsGate.await() }
            catalogueHandler = { _, _ -> catGate.await() }
        }
        val store = NewAgentStore(source = source, scope = this)
        store.open(hosts = listOf(hostView(deskId, "desk")))
        runCurrent()
        assertEquals(ListingState.Asking, store.snapshot.listing)

        store.close()
        advanceUntilIdle()
        assertEquals(ListingState.Asking, store.snapshot.listing)
        assertEquals(OfferState.Asking, store.snapshot.offers["claude"])
    }
}
