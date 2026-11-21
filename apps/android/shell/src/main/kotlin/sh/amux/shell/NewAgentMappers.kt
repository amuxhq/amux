package sh.amux.shell

import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.ListingState
import sh.amux.core.runtime.NewAgentState
import sh.amux.core.runtime.OfferState
import sh.amux.features.NewAgentListingUi
import sh.amux.features.NewAgentOfferUi
import sh.amux.features.NewAgentScreenState
import sh.amux.words.name

/** Pure mapper from the new-agent store to the New Agent screen's state. */
object NewAgentMappers {
    fun toNewAgentScreenState(
        state: NewAgentState,
        connecting: Set<HostId>,
    ): NewAgentScreenState = NewAgentScreenState(
        hosts = state.hosts,
        selectedHostId = state.machine?.bytes,
        connectingHostIds = connecting.map { it.bytes }.toSet(),
        provider = state.provider.wire,
        availableProviders = state.availableProviders.map { it.wire },
        offer = when (state.offers[state.provider]) {
            is OfferState.Offered -> NewAgentOfferUi.Offered
            OfferState.Unavailable -> NewAgentOfferUi.Unavailable
            OfferState.Asking, null -> NewAgentOfferUi.Asking
        },
        settings = state.settings,
        listing = when (state.listing) {
            ListingState.None -> NewAgentListingUi.None
            ListingState.Asking -> NewAgentListingUi.Asking
            ListingState.Ready -> NewAgentListingUi.Ready
            ListingState.Unavailable -> NewAgentListingUi.Unavailable
        },
        repositories = state.repositories,
        recent = state.recent,
        history = state.history,
        directory = state.directory,
        query = state.query,
        typedPath = state.typed,
        browsing = state.browsing,
        newWorktree = state.newWorktree,
        name = state.name,
        nameProblem = state.nameProblem,
        starting = state.starting,
        failure = state.failure,
    )

}
