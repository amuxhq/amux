package sh.amux.core.runtime

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import sh.amux.values.AgentKey
import sh.amux.values.AgentNameProblem
import sh.amux.values.Catalogue
import sh.amux.values.Directories
import sh.amux.values.Directory
import sh.amux.values.FleetRow
import sh.amux.values.HostView
import sh.amux.values.Kind
import sh.amux.values.NewAgent
import sh.amux.values.NewAgentChoices
import sh.amux.values.NewAgentPick
import sh.amux.values.Provider
import sh.amux.values.SettingsView

/**
 * Runtime seam consumed by [NewAgentStore] so the store can be tested without
 * opening an embedded profile while delegating directly to [Profile] in app
 * wiring.
 */
interface NewAgentSource {
    suspend fun hostCatalogue(host: HostId, provider: Provider): Result<Catalogue>
    suspend fun directories(host: HostId, query: String, limit: Int): Result<Directories>
    suspend fun pinFolder(host: HostId, path: String, pinned: Boolean): Result<Unit>
    suspend fun createAgent(agent: NewAgent): Result<AgentKey>
}

/** Wraps a live [Profile] as a [NewAgentSource]. */
fun Profile.asNewAgentSource(): NewAgentSource = object : NewAgentSource {
    override suspend fun hostCatalogue(host: HostId, provider: Provider): Result<Catalogue> =
        this@asNewAgentSource.hostCatalogue(host, provider)

    override suspend fun directories(host: HostId, query: String, limit: Int): Result<Directories> =
        this@asNewAgentSource.directories(host, query, limit)

    override suspend fun pinFolder(host: HostId, path: String, pinned: Boolean): Result<Unit> =
        this@asNewAgentSource.pinFolder(host, path, pinned)

    override suspend fun createAgent(agent: NewAgent): Result<AgentKey> =
        this@asNewAgentSource.createAgent(agent)
}

/**
 * Pure bridge functions used by [NewAgentStore] to resolve offered settings,
 * apply picks, and validate typed agent names. Defaults to the native Rust
 * bridge.
 */
interface NewAgentBridge {
    fun newAgentSettings(catalogue: Catalogue, chosen: NewAgentChoices): SettingsView?
    fun newAgentPick(catalogue: Catalogue, chosen: NewAgentChoices, pick: NewAgentPick): NewAgentChoices?
    fun agentNameProblem(name: String): AgentNameProblem?
}

object NativeNewAgentBridge : NewAgentBridge {
    override fun newAgentSettings(catalogue: Catalogue, chosen: NewAgentChoices): SettingsView? =
        Bridge.newAgentSettings(catalogue, chosen)

    override fun newAgentPick(
        catalogue: Catalogue,
        chosen: NewAgentChoices,
        pick: NewAgentPick,
    ): NewAgentChoices? = Bridge.newAgentPick(catalogue, chosen, pick)

    override fun agentNameProblem(name: String): AgentNameProblem? =
        Bridge.agentNameProblem(name)
}

/** The provider a person can start an agent with from the phone. */
enum class NewAgentProvider(
    val wire: Provider,
    val title: String,
    val wireName: String,
) {
    Claude(Provider.Claude, "Claude", "claude"),
    Codex(Provider.Codex, "Codex", "codex"),
    ;

    /**
     * The kind an agent of this provider starts as on a host whose settings
     * resolve Claude to [claude]. A host that has not said defaults to the SDK
     * driver, which carries every row the phone draws.
     */
    fun kind(claude: Kind?): Kind = when (this) {
        Claude -> when (claude) {
            Kind.ClaudePty -> Kind.ClaudePty
            Kind.ClaudeSdk, Kind.Codex, Kind.Unspecified, null -> Kind.ClaudeSdk
        }
        Codex -> Kind.Codex
    }

    companion object {
        fun fromKind(kind: Kind): NewAgentProvider? = when (kind) {
            Kind.ClaudePty, Kind.ClaudeSdk -> Claude
            Kind.Codex -> Codex
            Kind.Unspecified -> null
        }

        fun fromWire(provider: Provider): NewAgentProvider? = when (provider) {
            Provider.Claude -> Claude
            Provider.Codex -> Codex
            Provider.Unspecified -> null
        }
    }
}

/** What the chosen host said a provider offers. */
sealed interface OfferState {
    data object Asking : OfferState
    data class Offered(val catalogue: Catalogue) : OfferState
    data object Unavailable : OfferState
}

/** Status of the directory listing for the chosen host. */
enum class ListingState {
    None,
    Asking,
    Ready,
    Unavailable,
}

/** Immutable UI snapshot held by [NewAgentStore]. */
data class NewAgentState(
    val hosts: List<HostView> = emptyList(),
    val machine: HostId? = null,
    val claudeKind: Kind? = null,
    val listing: ListingState = ListingState.None,
    val repositories: List<Directory> = emptyList(),
    val recent: List<Directory> = emptyList(),
    val history: List<Directory> = emptyList(),
    val capped: Boolean = false,
    val directory: String = "",
    val query: String = "",
    val typed: String = "",
    val browsing: Boolean = false,
    val provider: NewAgentProvider = NewAgentProvider.Claude,
    val offers: Map<NewAgentProvider, OfferState> = emptyMap(),
    val chosen: NewAgentChoices = NewAgentChoices(),
    val settings: SettingsView? = null,
    val newWorktree: Boolean = false,
    val name: String = "",
    val nameProblem: AgentNameProblem? = null,
    val starting: Boolean = false,
    val failure: String? = null,
    val created: AgentKey? = null,
) {
    val selectedHost: HostView?
        get() = machine?.let { id -> hosts.firstOrNull { it.id == id } }

    /**
     * Providers offered by the chosen host, falling back to all known
     * providers until the host reports its list.
     */
    val availableProviders: List<NewAgentProvider>
        get() {
            val advertised = selectedHost
                ?.providers
                ?.mapNotNull { NewAgentProvider.fromWire(it.provider) }
                ?.distinct()
                .orEmpty()
            return if (advertised.isNotEmpty()) advertised else NewAgentProvider.entries
        }

    val catalogue: Catalogue?
        get() = (offers[provider] as? OfferState.Offered)?.catalogue

    val model: String?
        get() = chosen.model

    val effort: String?
        get() = chosen.effort

    val permission: String?
        get() = chosen.permission

    val mode: String?
        get() = chosen.mode

    val searching: Boolean
        get() = query.trim().isNotEmpty()

    /** Repositories and recent/history folders matching [query], repositories first. */
    val found: List<Directory>
        get() {
            val wanted = query.trim().lowercase()
            if (wanted.isEmpty()) return emptyList()
            return (repositories + recent + history).filter { dir ->
                dir.name.lowercase().contains(wanted) || dir.path.lowercase().contains(wanted)
            }
        }

    /** Most likely folders for quick chips under the directory row. */
    val likely: List<Directory>
        get() = recent + repositories

    /** Whether a search must query the host because its initial listing hit the cap. */
    val searchesTheMachine: Boolean
        get() = capped && searching

    val typedPath: String
        get() = typed.trim()

    val chosenName: String
        get() = name.trim()

    fun isPinned(path: String): Boolean = repositories.any { it.path == path }

    val ready: Boolean
        get() {
            val host = machine ?: return false
            val view = hosts.firstOrNull { it.id == host }
            if (view != null && !view.online) return false
            return directory.isNotEmpty() && nameProblem == null && !starting
        }

    /** Payload sent to the host when starting the agent; `null` until [ready]. */
    val request: NewAgent?
        get() {
            val host = machine ?: return null
            if (!ready) return null
            return NewAgent(
                hostId = host.bytes,
                kind = provider.kind(claudeKind),
                cwd = directory,
                name = chosenName,
                effort = effort,
                mode = if (provider == NewAgentProvider.Codex) mode else null,
                model = model,
                newWorktree = if (newWorktree) true else null,
                permission = permission,
            )
        }
}

/**
 * State holder for the New Agent flow: host selection, provider catalogue
 * fetching, directory browsing and pinning, live agent-name validation,
 * settings picks, worktree option, and submission with double-submit and
 * stale-response guards.
 */
class NewAgentStore(
    private val source: NewAgentSource? = null,
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default),
    private val bridge: NewAgentBridge = NativeNewAgentBridge,
    private val searchDebounceMs: Long = DEFAULT_SEARCH_DEBOUNCE_MS,
) {
    private val _state = MutableStateFlow(NewAgentState())
    val state: StateFlow<NewAgentState> = _state.asStateFlow()

    val snapshot: NewAgentState
        get() = _state.value

    private var worked: Map<HostId, List<Directory>> = emptyMap()
    private var directoryGeneration: Int = 0
    private var offerGeneration: Int = 0
    private val offerAsked: MutableMap<NewAgentProvider, Int> = mutableMapOf()
    private var machineOnline: Boolean? = null
    private var providerPicked: Boolean = false
    private var directoryJob: Job? = null
    private var searchJob: Job? = null
    private val offerJobs: MutableMap<NewAgentProvider, Job> = mutableMapOf()

    /**
     * Opens a fresh New Agent flow. Automatically selects [initialHost] if
     * given, else the first online paired host (preferring one advertising
     * providers). When [source] is provided and a host is selected, directory
     * listings and provider catalogues are fetched automatically.
     */
    fun open(
        hosts: List<HostView> = emptyList(),
        initialHost: HostId? = null,
        rows: List<FleetRow> = emptyList(),
    ) {
        cancelInFlight()
        val chosenHost = pickInitialHost(hosts, initialHost)
        val hostView = chosenHost?.let { id -> hosts.firstOrNull { it.id == id } }
        machineOnline = hostView?.online
        providerPicked = false
        directoryGeneration += 1
        offerGeneration += 1
        offerAsked.clear()

        _state.value = NewAgentState(
            hosts = hosts,
            machine = chosenHost,
            claudeKind = hostView?.claudeKind,
        )

        if (rows.isNotEmpty()) {
            remember(rows)
        }
        reconcileProviderForHost(hostView)

        if (chosenHost != null && source != null && (hostView == null || hostView.online)) {
            fetchDirectories(chosenHost, query = "", quiet = false)
            fetchAllOffers(chosenHost)
        }
    }

    /**
     * Updates the known host list while the screen is open: refreshes the
     * selected host's `claudeKind`, clears transient unreachable errors when
     * the host comes back online, and auto-selects a ready host if none is
     * chosen yet.
     */
    fun syncHosts(hosts: List<HostView>) {
        val current = _state.value
        val currentMachine = current.machine
        val selected = currentMachine?.let { id -> hosts.firstOrNull { it.id == id } }
        if (currentMachine == null) {
            val auto = pickInitialHost(hosts, initialHost = null)
            val autoView = auto?.let { id -> hosts.firstOrNull { it.id == id } }
            _state.update { it.copy(hosts = hosts) }
            if (auto != null) {
                selectHost(auto)
            } else if (autoView != null) {
                _state.update { it.copy(claudeKind = autoView.claudeKind) }
            }
            return
        }

        _state.update { state ->
            state.copy(
                hosts = hosts,
                claudeKind = selected?.claudeKind ?: state.claudeKind,
            )
        }
        if (selected != null) {
            machineReached(selected.online)
        }
    }

    /** Records whether the selected host is reachable now, clearing stale offline failures on recovery. */
    fun machineReached(online: Boolean) {
        val wasOffline = machineOnline == false
        machineOnline = online
        if (online && wasOffline && _state.value.failure != null) {
            _state.update { it.copy(failure = null) }
        }
    }

    /** Updates which Claude driver the chosen host starts. */
    fun machineStarts(claude: Kind?) {
        _state.update { it.copy(claudeKind = claude) }
    }

    /**
     * Remembers recent directories per host and the most recently active
     * provider from the fleet until the person picks a provider explicitly.
     */
    fun remember(rows: List<FleetRow>) {
        val sorted = rows.sortedByDescending { it.lastActivityMs }
        if (!providerPicked) {
            val latest = sorted
                .firstOrNull { NewAgentProvider.fromKind(it.card.kind) != null }
                ?.let { NewAgentProvider.fromKind(it.card.kind) }
            if (latest != null) {
                val allowed = _state.value.availableProviders
                if (latest in allowed) {
                    _state.update { state ->
                        val cat = (state.offers[latest] as? OfferState.Offered)?.catalogue
                        state.copy(
                            provider = latest,
                            settings = cat?.let { bridge.newAgentSettings(it, state.chosen) },
                        )
                    }
                }
            }
        }

        val places = mutableMapOf<HostId, MutableList<Directory>>()
        for (row in sorted) {
            val host = row.hostId ?: continue
            val path = row.card.cwd
            if (path.isEmpty()) continue
            val list = places.getOrPut(host) { mutableListOf() }
            if (list.any { it.path == path }) continue
            val name = path.trimEnd('/').substringAfterLast('/').ifEmpty { path }
            list.add(Directory(path = path, name = name, repository = null))
        }
        worked = places
    }

    /** Selects a different host, cancelling and forgetting everything learned from the previous host. */
    fun selectHost(host: HostId) {
        if (_state.value.machine == host) return
        cancelInFlight()
        val hostView = _state.value.hosts.firstOrNull { it.id == host }
        machineOnline = hostView?.online
        directoryGeneration += 1
        offerGeneration += 1
        offerAsked.clear()

        _state.update { state ->
            state.copy(
                machine = host,
                claudeKind = hostView?.claudeKind,
                listing = ListingState.None,
                repositories = emptyList(),
                recent = emptyList(),
                history = emptyList(),
                capped = false,
                directory = "",
                query = "",
                offers = emptyMap(),
                chosen = NewAgentChoices(),
                settings = null,
                failure = null,
            )
        }
        reconcileProviderForHost(hostView)

        if (source != null && (hostView == null || hostView.online)) {
            fetchDirectories(host, query = "", quiet = false)
            fetchAllOffers(host)
        }
    }

    /** Switches provider, clearing provider-specific model/effort/permission/mode picks. */
    fun selectProvider(provider: NewAgentProvider) {
        providerPicked = true
        val current = _state.value
        if (current.provider == provider) return
        val freshChoices = NewAgentChoices()
        val cat = (current.offers[provider] as? OfferState.Offered)?.catalogue
        _state.update { state ->
            state.copy(
                provider = provider,
                chosen = freshChoices,
                settings = cat?.let { bridge.newAgentSettings(it, freshChoices) },
                failure = null,
            )
        }
        val host = _state.value.machine
        if (host != null && source != null && _state.value.offers[provider] == null) {
            fetchOffer(host, provider)
        }
    }

    fun setBrowsing(open: Boolean) {
        _state.update { it.copy(browsing = open) }
    }

    fun chooseDirectory(path: String) {
        _state.update { it.copy(directory = path, failure = null) }
    }

    fun updateTypedPath(typed: String) {
        _state.update { it.copy(typed = typed) }
    }

    fun useTypedPath() {
        val path = _state.value.typedPath
        if (path.isEmpty()) return
        _state.update { it.copy(directory = path, browsing = false, failure = null) }
    }

    /**
     * Updates the folder search query immediately for local filtering and
     * triggers a debounced host search when the host's listing was capped.
     */
    fun updateQuery(query: String) {
        val wasSearchingMachine = _state.value.searchesTheMachine
        _state.update { it.copy(query = query) }
        val host = _state.value.machine ?: return
        if (source == null) return

        searchJob?.cancel()
        val trimmed = query.trim()
        if (_state.value.searchesTheMachine) {
            searchJob = scope.launch {
                if (searchDebounceMs > 0L) {
                    delay(searchDebounceMs)
                }
                fetchDirectories(host, query = trimmed, quiet = true)
            }
        } else if (wasSearchingMachine && trimmed.isEmpty()) {
            searchJob = scope.launch {
                fetchDirectories(host, query = "", quiet = true)
            }
        }
    }

    /** Optimistically pins or unpins [directory] and sends the change to the chosen host. */
    fun pinFolder(directory: Directory, pinned: Boolean) {
        val host = _state.value.machine ?: return
        val before = _state.value
        applyPinOptimistically(directory, pinned)
        val runtimeSource = source ?: return
        scope.launch {
            val result = runtimeSource.pinFolder(host, directory.path, pinned)
            result.onFailure { error ->
                if (_state.value.machine == host) {
                    _state.update { current ->
                        current.copy(
                            repositories = before.repositories,
                            recent = before.recent,
                            history = before.history,
                            failure = error.failureDescription,
                        )
                    }
                }
            }
        }
    }

    /** Applies a model, effort, permission, or mode pick through the shared Rust rules. */
    fun pick(pick: NewAgentPick) {
        val current = _state.value
        val offered = current.catalogue ?: EMPTY_CATALOGUE
        val next = bridge.newAgentPick(offered, current.chosen, pick) ?: return
        val nextSettings = current.catalogue?.let { bridge.newAgentSettings(it, next) }
        _state.update { it.copy(chosen = next, settings = nextSettings, failure = null) }
    }

    fun setNewWorktree(enabled: Boolean) {
        _state.update { it.copy(newWorktree = enabled) }
    }

    /** Updates the agent name and runs live validation when non-empty. */
    fun updateName(written: String) {
        val trimmed = written.trim()
        val problem = if (trimmed.isEmpty()) null else bridge.agentNameProblem(trimmed)
        _state.update { it.copy(name = written, nameProblem = problem, failure = null) }
    }

    /**
     * Submits the new agent request to [source], guarding against double
     * submission and surfacing any refusal in [NewAgentState.failure].
     */
    suspend fun submit(): Result<AgentKey> {
        val current = _state.value
        if (current.starting) {
            return Result.failure(RuntimeFailure("already starting"))
        }
        val req = current.request
            ?: return Result.failure(RuntimeFailure("agent is not ready to start"))
        val runtimeSource = source
            ?: return Result.failure(RuntimeFailure("no host connection available"))

        _state.update { it.copy(starting = true, failure = null) }
        val result = try {
            runtimeSource.createAgent(req)
        } catch (t: Throwable) {
            Result.failure(RuntimeFailure(t.failureDescription))
        }
        result.fold(
            onSuccess = { key ->
                _state.update { it.copy(starting = false, created = key, failure = null) }
            },
            onFailure = { error ->
                _state.update { it.copy(starting = false, failure = error.failureDescription) }
            },
        )
        return result
    }

    /** Launches [submit] on [scope] for fire-and-observe UI callers. */
    fun start() {
        if (!_state.value.ready) return
        scope.launch { submit() }
    }

    /** Cancels all in-flight asynchronous requests owned by this store. */
    fun close() {
        cancelInFlight()
    }

    // --- Internal orchestration --------------------------------------------

    private fun pickInitialHost(hosts: List<HostView>, initialHost: HostId?): HostId? {
        if (initialHost != null) return initialHost
        val online = hosts.filter { it.online && !it.local }
        val withProviders = online.firstOrNull { host ->
            host.providers.any { it.provider != Provider.Unspecified }
        }
        return (withProviders ?: online.firstOrNull())?.id
    }

    private fun reconcileProviderForHost(hostView: HostView?) {
        if (hostView == null) return
        val advertised = hostView.providers
            .mapNotNull { NewAgentProvider.fromWire(it.provider) }
            .distinct()
        if (advertised.isNotEmpty() && _state.value.provider !in advertised) {
            _state.update { it.copy(provider = advertised.first()) }
        }
    }

    private fun fetchDirectories(host: HostId, query: String, quiet: Boolean) {
        val runtimeSource = source ?: return
        directoryJob?.cancel()
        directoryGeneration += 1
        val gen = directoryGeneration
        if (!quiet) {
            _state.update { it.copy(listing = ListingState.Asking) }
        }
        directoryJob = scope.launch {
            val result = try {
                runtimeSource.directories(host, query, DIRECTORY_LIMIT)
            } catch (t: Throwable) {
                Result.failure(RuntimeFailure(t.failureDescription))
            }
            applyDirectories(host, result, generation = gen)
        }
    }

    private fun applyDirectories(
        host: HostId,
        result: Result<Directories>,
        generation: Int,
    ) {
        if (generation != directoryGeneration || _state.value.machine != host) return
        val fallback = worked[host].orEmpty()
        var prefillCandidate = fallback.firstOrNull()

        result.fold(
            onSuccess = { dirs ->
                val repos = dirs.repositories
                val lately = if (dirs.recent.isEmpty()) fallback else dirs.recent
                prefillCandidate = lately.firstOrNull()
                val repoPaths = repos.map { it.path }.toSet()
                val filteredRecent = lately.filter { it.path !in repoPaths }
                val shownPaths = repoPaths + filteredRecent.map { it.path }
                val filteredHistory = dirs.history.orEmpty().filter { it.path !in shownPaths }
                val totalCount = dirs.recent.size + dirs.repositories.size + dirs.history.orEmpty().size
                val isCapped = totalCount >= DIRECTORY_LIMIT

                _state.update { state ->
                    val nextDir = if (state.directory.isEmpty()) {
                        (prefillCandidate ?: repos.firstOrNull())?.path.orEmpty()
                    } else {
                        state.directory
                    }
                    state.copy(
                        repositories = repos,
                        recent = filteredRecent,
                        history = filteredHistory,
                        capped = isCapped,
                        listing = ListingState.Ready,
                        directory = nextDir,
                    )
                }
            },
            onFailure = {
                _state.update { state ->
                    val nextDir = if (state.directory.isEmpty()) {
                        prefillCandidate?.path.orEmpty()
                    } else {
                        state.directory
                    }
                    state.copy(
                        repositories = emptyList(),
                        recent = fallback,
                        history = emptyList(),
                        capped = false,
                        listing = ListingState.Unavailable,
                        directory = nextDir,
                    )
                }
            },
        )
    }

    private fun fetchAllOffers(host: HostId) {
        for (provider in _state.value.availableProviders) {
            fetchOffer(host, provider)
        }
    }

    private fun fetchOffer(host: HostId, provider: NewAgentProvider) {
        val runtimeSource = source ?: return
        offerJobs.remove(provider)?.cancel()
        offerGeneration += 1
        val gen = offerGeneration
        offerAsked[provider] = gen
        _state.update { state ->
            state.copy(offers = state.offers + (provider to OfferState.Asking))
        }
        offerJobs[provider] = scope.launch {
            val result = try {
                runtimeSource.hostCatalogue(host, provider.wire)
            } catch (t: Throwable) {
                Result.failure(RuntimeFailure(t.failureDescription))
            }
            applyOffer(host, provider, result, generation = gen)
        }
    }

    private fun applyOffer(
        host: HostId,
        provider: NewAgentProvider,
        result: Result<Catalogue>,
        generation: Int,
    ) {
        if (offerAsked[provider] != generation || _state.value.machine != host) return
        val nextOffer = result.fold(
            onSuccess = { OfferState.Offered(it) },
            onFailure = { OfferState.Unavailable },
        )
        _state.update { state ->
            val updatedOffers = state.offers + (provider to nextOffer)
            val updatedSettings = if (state.provider == provider) {
                (nextOffer as? OfferState.Offered)?.catalogue?.let {
                    bridge.newAgentSettings(it, state.chosen)
                }
            } else {
                state.settings
            }
            state.copy(
                offers = updatedOffers,
                settings = updatedSettings,
            )
        }
    }

    private fun applyPinOptimistically(directory: Directory, pinned: Boolean) {
        val path = directory.path
        _state.update { state ->
            if (pinned) {
                val nextRepos = if (state.isPinned(path)) {
                    state.repositories
                } else {
                    state.repositories + directory
                }
                state.copy(
                    repositories = nextRepos,
                    recent = state.recent.filterNot { it.path == path },
                    history = state.history.filterNot { it.path == path },
                    failure = null,
                )
            } else {
                val nextRecent = if (state.recent.any { it.path == path }) {
                    state.recent
                } else {
                    listOf(directory) + state.recent
                }
                state.copy(
                    repositories = state.repositories.filterNot { it.path == path },
                    recent = nextRecent,
                    failure = null,
                )
            }
        }
    }

    private fun cancelInFlight() {
        directoryJob?.cancel()
        directoryJob = null
        searchJob?.cancel()
        searchJob = null
        for (job in offerJobs.values) {
            job.cancel()
        }
        offerJobs.clear()
    }

    companion object {
        const val DIRECTORY_LIMIT: Int = 200
        const val DEFAULT_SEARCH_DEBOUNCE_MS: Long = 200L

        private val EMPTY_CATALOGUE = Catalogue(
            models = emptyList(),
            commands = emptyList(),
            permissions = emptyList(),
            modes = emptyList(),
            skills = emptyList(),
            newAgent = null,
        )
    }
}
