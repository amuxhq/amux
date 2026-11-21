package sh.amux.core.runtime

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import sh.amux.values.AgentKey
import sh.amux.values.DeleteWarning
import sh.amux.values.FleetFilter
import sh.amux.values.FleetOnly
import sh.amux.values.FleetRow
import sh.amux.values.FleetSection
import sh.amux.values.FleetView
import sh.amux.values.HostView
import sh.amux.values.RelayLink
import sh.amux.values.SectionKind

/** Delegate wired by [StoreBundle] so screens can invoke fleet actions through [FleetStore]. */
interface FleetActions {
    fun filterFleet(filter: FleetFilter)
    fun toggleFamily(head: AgentKey)
    suspend fun pin(agent: AgentKey): Result<Unit>
    suspend fun settle(agent: AgentKey): Result<Unit>
    suspend fun unsettle(agent: AgentKey): Result<Unit>
    suspend fun rename(name: String, agent: AgentKey): Result<Unit>
    suspend fun delete(agent: AgentKey, discardingChanges: Boolean = false): Result<Unit>
    suspend fun cancelStop(agent: AgentKey): Result<Unit>
    suspend fun previewDelete(agent: AgentKey): Result<DeleteWarning?>
}

/** Immutable UI snapshot of the fleet for Compose `collectAsState()`. */
data class FleetState(
    val rows: List<FleetRow> = emptyList(),
    val sections: List<FleetSection> = emptyList(),
    val needYou: Int = 0,
    val working: Int = 0,
    val hosts: Map<HostId, HostView> = emptyMap(),
    val flipped: Set<SectionKind> = emptySet(),
    val filter: FleetFilter = FleetFilter(words = "", only = null),
    val orderedAtMs: Long = 0L,
    val loaded: Boolean = false,
    val relay: RelayLink = RelayLink.Off,
    val heldAway: String? = null,
) {
    /** Trusted hosts other than this phone. */
    val machines: List<HostView>
        get() = hosts.values.filter { !it.local }

    /** Families whose members are currently expanded under their head. */
    val expandedFamilies: Set<AgentKey>
        get() = rows.filter { it.expanded }.map { it.card.agent }.toSet()

    val subtitle: String
        get() {
            if (needYou <= 0) {
                return "Nothing needs you · $working working"
            }
            val agents = rows
                .filter { !it.closed }
                .sumOf { row -> if (row.expanded) 1 else row.card.members }
            val noun = if (agents == 1) "agent" else "agents"
            return "$needYou need you · $agents $noun"
        }

    val relayTrouble: String?
        get() = when (relay) {
            RelayLink.SignInAgain -> "Sign in again to reach your computers from anywhere"
            RelayLink.VersionMismatch -> "Update amux to reach your computers from anywhere"
            RelayLink.Off,
            RelayLink.Connecting,
            RelayLink.Retrying,
            RelayLink.Connected,
            -> null
        }

    val awayHost: String?
        get() = machines.filter { it.group == HostGroup.Away }.map { it.name }.sorted().firstOrNull()

    val closed: FleetSection?
        get() = sections.firstOrNull { it.kind == SectionKind.Closed }

    val unreachableHost: String?
        get() = machines.filter { !it.online }.map { it.name }.sorted().firstOrNull()

    fun folded(section: FleetSection): Boolean =
        section.foldedByDefault != (section.kind in flipped)

    fun host(id: HostId?): HostView? = id?.let { hosts[it] }

    fun row(agent: AgentKey): FleetRow? = rows.firstOrNull { it.id == agent }
}

/**
 * Observable store for the active profile's fleet sections, counts, filters,
 * and agent actions.
 */
class FleetStore(
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private val _state = MutableStateFlow(FleetState(orderedAtMs = clock()))
    val state: StateFlow<FleetState> = _state.asStateFlow()

    val snapshot: FleetState
        get() = _state.value

    internal var actions: FleetActions? = null

    /** Applies the latest fleet view and host list from the runtime. */
    fun show(view: FleetView, hosts: List<HostView>) {
        _state.update { current ->
            val trustedHosts = buildMap {
                for (host in hosts) {
                    if (host.trusted) {
                        host.id?.let { put(it, host) }
                    }
                }
            }
            val allRows = view.sections.flatMap { it.rows }
            val newest = allRows.maxOfOrNull { it.lastActivityMs }
            val orderedAt = if (newest != null && newest > current.orderedAtMs) {
                newest
            } else {
                current.orderedAtMs
            }
            val machines = trustedHosts.values.filter { !it.local }
            val away = machines.filter { it.group == HostGroup.Away }
                .map { it.name }
                .sorted()
                .firstOrNull()
            val nextHeldAway = when {
                away != null -> away
                current.heldAway != null &&
                    !machines.any { it.name == current.heldAway && !it.online } -> null
                else -> current.heldAway
            }
            current.copy(
                loaded = true,
                hosts = trustedHosts,
                sections = view.sections,
                rows = allRows,
                needYou = view.needYou,
                working = view.working,
                orderedAtMs = orderedAt,
                heldAway = nextHeldAway,
            )
        }
    }

    /** Returns an immutable frozen snapshot for screens temporarily covered by another route. */
    fun still(): FleetState = _state.value

    fun relay(link: RelayLink) {
        _state.update { it.copy(relay = link) }
    }

    /** Advances the relative-age clock without ever moving backwards. */
    fun refreshOrder(nowMs: Long = clock()) {
        _state.update { current ->
            if (nowMs > current.orderedAtMs) current.copy(orderedAtMs = nowMs) else current
        }
    }

    fun toggleSection(section: SectionKind) {
        _state.update { current ->
            val updated = if (section in current.flipped) {
                current.flipped - section
            } else {
                current.flipped + section
            }
            current.copy(flipped = updated)
        }
    }

    /** Updates the local filter state and forwards it to the runtime when attached. */
    fun setFilter(filter: FleetFilter) {
        _state.update { it.copy(filter = filter) }
        actions?.filterFleet(filter)
    }

    fun setSearchWords(words: String) {
        setFilter(_state.value.filter.copy(words = words))
    }

    fun setOnly(only: FleetOnly?) {
        setFilter(_state.value.filter.copy(only = only))
    }

    fun toggleFamily(head: AgentKey) {
        actions?.toggleFamily(head)
    }

    suspend fun pin(agent: AgentKey): Result<Unit> =
        actions?.pin(agent) ?: Result.failure(RuntimeFailure("nothing is running"))

    suspend fun settle(agent: AgentKey): Result<Unit> =
        actions?.settle(agent) ?: Result.failure(RuntimeFailure("nothing is running"))

    suspend fun unsettle(agent: AgentKey): Result<Unit> =
        actions?.unsettle(agent) ?: Result.failure(RuntimeFailure("nothing is running"))

    suspend fun rename(name: String, agent: AgentKey): Result<Unit> =
        actions?.rename(name, agent) ?: Result.failure(RuntimeFailure("nothing is running"))

    suspend fun delete(agent: AgentKey, discardingChanges: Boolean = false): Result<Unit> =
        actions?.delete(agent, discardingChanges)
            ?: Result.failure(RuntimeFailure("nothing is running"))

    suspend fun cancelStop(agent: AgentKey): Result<Unit> =
        actions?.cancelStop(agent) ?: Result.failure(RuntimeFailure("nothing is running"))

    suspend fun previewDelete(agent: AgentKey): Result<DeleteWarning?> =
        actions?.previewDelete(agent) ?: Result.failure(RuntimeFailure("nothing is running"))
}
