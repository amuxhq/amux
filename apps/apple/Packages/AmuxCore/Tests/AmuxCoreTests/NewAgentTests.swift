import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class NewAgentTests: XCTestCase {
    /// What desk offers: Claude, Codex, and an ACP agent signed out there.
    private let offered = [
        HostProvider(
            provider: "claude", name: "Claude", signInHint: "Run claude and sign in with /login.", version: "",
            capabilities: Capabilities(
                stopsJob: true, switchesBack: false, attachable: false, resumable: true, sendsNow: true,
                images: true, files: true, passesUnknownCommands: true),
            signedIn: true),
        HostProvider(
            provider: "codex", name: "Codex", signInHint: "Run codex login.", version: "",
            capabilities: Capabilities(
                stopsJob: true, switchesBack: false, attachable: true, resumable: true, sendsNow: true,
                images: true, files: true, passesUnknownCommands: false),
            signedIn: true),
        HostProvider(
            provider: "grok", name: "Grok", signInHint: "Sign in with Grok", version: "1.0.50",
            capabilities: Capabilities(
                stopsJob: false, switchesBack: false, attachable: false, resumable: true, sendsNow: false,
                images: false, files: true, passesUnknownCommands: true),
            signedIn: false),
    ]

    /// A store opened on desk, which has listed what it offers.
    private func opened() -> NewAgentStore {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.machineOffers(offered)
        return store
    }

    private let listed = Directories(
        recent: [Directory(path: "/src/amux", name: "amux", repository: nil)],
        repositories: [
            Directory(path: "/src/web", name: "web", repository: nil),
            Directory(path: "/src/relay", name: "relay", repository: nil),
        ], history: [])

    func testTheMostRecentDirectoryIsPrefilledAndTheNameLeftToTheHost() {
        let store = opened()
        store.remember([Cards.row(1, "amux")])
        store.listed(.success(listed), asked: store.asking())
        XCTAssertEqual(store.listing, .ready)
        XCTAssertEqual(store.directory, "/src/amux")
        XCTAssertEqual(store.name, "", "nobody typed one")
        XCTAssertTrue(store.ready)
        XCTAssertEqual(
            store.request,
            NewAgent(hostId: Cards.desk.bytes, provider: "claude", cwd: "/src/amux",
                     name: "", effort: nil, mode: nil, model: nil, newWorktree: nil, permission: nil))
    }

    func testAFolderPinnedMovesToTheRepositoriesAtOnceAndHistoryIsListedOnce() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        let notes = Directory(path: "/src/notes", name: "notes", repository: nil)
        store.listed(
            .success(Directories(
                recent: listed.recent, repositories: listed.repositories,
                history: [notes, Directory(path: "/src/amux", name: "amux", repository: nil)])),
            asked: store.asking())
        XCTAssertEqual(store.history.map(\.path), ["/src/notes"], "a recent folder is listed once")
        XCTAssertFalse(store.isPinned("/src/notes"))

        store.pinned(notes, true)
        XCTAssertTrue(store.isPinned("/src/notes"))
        XCTAssertEqual(store.repositories.map(\.path), ["/src/web", "/src/relay", "/src/notes"])
        XCTAssertTrue(store.history.isEmpty)

        store.pinned(notes, false)
        XCTAssertFalse(store.isPinned("/src/notes"))
        XCTAssertEqual(store.recent.first?.path, "/src/notes")
        XCTAssertEqual(store.found.count, 0, "nothing is searched")
        store.query = "NOT"
        XCTAssertEqual(store.found.map(\.path), ["/src/notes"])
    }

    func testAHostThatCannotListOffersWhereItsAgentsWork() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.remember([Cards.row(1, "web")])
        store.listed(.failure(RuntimeFailure("not available")), asked: store.asking())
        XCTAssertEqual(store.listing, .unavailable)
        XCTAssertEqual(store.recent.map(\.path), ["/Users/pat/source/web"])
        XCTAssertEqual(store.directory, "/Users/pat/source/web")
    }

    func testChoosingAnotherHostForgetsTheLastOnesListing() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.listed(.success(listed), asked: store.asking())
        let other = HostId(UUID())
        store.point(at: other)
        XCTAssertEqual(store.listing, .none)
        XCTAssertEqual(store.directory, "")
        XCTAssertFalse(store.ready)
    }

    func testAStaleListingIsDropped() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        let stale = store.asking()
        store.point(at: HostId(UUID()))
        store.listed(.success(listed), asked: stale)
        XCTAssertEqual(store.listing, .none)
    }

    /// The agents offered are the host's, by its ids and names: nothing
    /// starts until it has said, and then the request names the agent by
    /// the id the host lists it by, whatever kind it runs as.
    func testTheAgentsOfferedAreTheHostsAndTheRequestNamesOneById() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        XCTAssertNil(store.provider)
        XCTAssertFalse(store.ready, "no agent to start until the host lists one")
        store.machineOffers(offered)
        XCTAssertEqual(store.agents.map(\.name), ["Claude", "Codex", "Grok"])
        XCTAssertEqual(store.provider, "claude", "the host's first")
        XCTAssertEqual(store.request?.provider, "claude")
        store.choose(provider: "grok")
        XCTAssertEqual(store.chosenAgent?.signedIn, false)
        XCTAssertEqual(store.chosenAgent?.signInHint, "Sign in with Grok")
        XCTAssertEqual(store.request?.provider, "grok")
        XCTAssertEqual(store.unasked, ["claude", "codex", "grok"])
    }

    /// A pick the host stops offering gives way to its first agent, and
    /// another host lists its own.
    func testAnAgentTheHostNoLongerOffersGivesWay() {
        let store = opened()
        store.choose(provider: "grok")
        store.machineOffers(Array(offered.prefix(2)))
        XCTAssertEqual(store.provider, "claude")
        store.point(at: HostId(UUID()))
        XCTAssertTrue(store.agents.isEmpty)
        store.machineOffers([offered[1]])
        XCTAssertEqual(store.provider, "codex")
    }

    /// The chooser lists the host's repositories, in its person's order, then
    /// where agents worked lately; a repository is not listed twice.
    func testRepositoriesComeBeforeRecentFoldersAndAreListedOnce() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.listed(.success(Directories(
            recent: [
                Directory(path: "/src/relay", name: "relay", repository: nil),
                Directory(path: "/src/scratch", name: "scratch", repository: nil),
            ],
            repositories: [
                Directory(path: "/src/web", name: "web", repository: nil),
                Directory(path: "/src/relay", name: "relay", repository: nil),
            ], history: [])), asked: store.asking())
        XCTAssertEqual(store.repositories.map(\.name), ["web", "relay"])
        XCTAssertEqual(store.recent.map(\.name), ["scratch"])
        XCTAssertEqual(store.likely.map(\.name), ["scratch", "web", "relay"])
        XCTAssertEqual(store.directory, "/src/relay", "where an agent worked last is prefilled, repository or not")
        XCTAssertFalse(store.searching)
        XCTAssertEqual(store.found, [])
        store.query = "a"
        XCTAssertTrue(store.searching)
        XCTAssertEqual(store.found.map(\.name), ["relay", "scratch"])
    }

    func testAHostWithOnlyRepositoriesPrefillsTheFirst() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.listed(.success(Directories(
            recent: [], repositories: [Directory(path: "/src/web", name: "web", repository: nil)], history: [])),
            asked: store.asking())
        XCTAssertEqual(store.directory, "/src/web")
    }

    func testTheAgentStartsAsTheOneMostRecentlyAtWorkUntilOneIsPicked() {
        let store = opened()
        store.remember([
            Cards.row(1, "old", minutesAgo: 30, kind: .claudeSdk),
            Cards.row(2, "new", minutesAgo: 2, kind: .acp, provider: "grok"),
        ])
        XCTAssertEqual(store.provider, "grok")
        store.choose(provider: "claude")
        store.remember([Cards.row(3, "newer", minutesAgo: 1, kind: .codex)])
        XCTAssertEqual(store.provider, "claude", "the fleet arriving does not undo a pick")
        store.open(on: Cards.desk)
        store.remember([Cards.row(4, "latest", minutesAgo: 1, kind: .codex)])
        store.machineOffers(offered)
        XCTAssertEqual(store.provider, "codex", "listed after the fleet arrived, the one at work still leads")
        store.open(on: Cards.desk)
        store.remember([])
        store.machineOffers(offered)
        XCTAssertEqual(store.provider, "claude", "with no agents, the host's first")
    }

    func testANameIsOptionalAndARefusalIsSaid() {
        let store = opened()
        store.choose(directory: "/src/x")
        store.choose(name: "  ")
        XCTAssertTrue(store.ready, "nobody has to name an agent")
        XCTAssertEqual(store.request?.name, "", "the host names it")
        store.choose(name: "x")
        store.starts()
        XCTAssertFalse(store.ready)
        store.started(.failure(RuntimeFailure("the host is busy")))
        XCTAssertEqual(store.failure, "the host is busy")
        store.started(.success(Cards.key(9)))
        XCTAssertEqual(store.created, Cards.key(9))
    }

    func testSearchingFiltersHereAndAsksTheHostOnlyWhenItsListWasCut() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.listed(.success(listed), asked: store.asking())
        store.query = "rel"
        XCTAssertEqual(store.found.map(\.name), ["relay"])
        XCTAssertFalse(store.searchesTheMachine)
    }

    private let codex = Catalogue(
        models: [
            OfferedModel(value: "gpt-6-astra", displayName: "GPT-6-Astra", description: "", efforts: ["low", "high"], resolvedModel: "", defaultEffort: "low"),
            OfferedModel(value: "gpt-6-sol", displayName: "GPT-6-Sol", description: "", efforts: ["low"], resolvedModel: "", defaultEffort: "low"),
        ],
        commands: [],
        permissions: [
            OfferedPermission(value: "default", displayName: "Default", normal: true, neverAsks: false, settable: true, models: [], level: 0),
            OfferedPermission(value: "auto", displayName: "Auto", normal: false, neverAsks: false, settable: true, models: ["gpt-6-astra"], level: 0),
            OfferedPermission(value: "locked", displayName: "Locked", normal: false, neverAsks: false, settable: false, models: [], level: 0),
        ],
        modes: [
            OfferedMode(value: "default", displayName: "Default", normal: true, settable: true, level: 0),
            OfferedMode(value: "plan", displayName: "Plan", normal: false, settable: true, level: 0),
        ],
        skills: [], newAgent: nil)

    func testTheChoicesComeFromTheHostsCatalogueAndGoInTheRequest() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        store.choose(provider: "codex")
        let asked = store.askingOffer(for: "codex")
        XCTAssertNil(store.catalogue, "nothing is offered until the host says")
        store.offered(.success(codex), for: "codex", asked: asked)
        XCTAssertEqual(store.catalogue, codex)

        let values = { (choices: [PermissionChoice]) in choices.map(\.value) }
        XCTAssertEqual(values(store.settings?.permissions ?? []), ["default"],
                       "a permission naming models waits for one of them; one not settable is never offered")
        store.choose(.model("gpt-6-astra"))
        XCTAssertNil(store.effort, "a model's own default effort is no pick of the person's")
        XCTAssertEqual(store.settings?.efforts.first(where: \.current)?.value, "low")
        XCTAssertEqual(store.settings?.efforts.map(\.value), ["low", "high"])
        XCTAssertEqual(values(store.settings?.permissions ?? []), ["default", "auto"])
        XCTAssertEqual(store.settings?.modes.map(\.value), ["default", "plan"])
        XCTAssertEqual(store.settings?.modes.first(where: \.current)?.value, "default",
                       "none chosen: the normal mode is in force")
        store.choose(.effort("high"))
        store.choose(.permission("auto"))
        store.choose(.mode("plan"))
        store.newWorktree = true
        XCTAssertEqual(
            store.request,
            NewAgent(hostId: Cards.desk.bytes, provider: "codex", cwd: "/src/x", name: "",
                     effort: "high", mode: "plan", model: "gpt-6-astra", newWorktree: true,
                     permission: "auto"))

        // Another model starts at its own default effort and gives up a
        // permission it does not take for the normal one.
        store.choose(.model("gpt-6-sol"))
        XCTAssertNil(store.effort)
        XCTAssertEqual(store.permission, "default")
        store.choose(.mode("default"))
        XCTAssertNil(store.mode, "the normal mode is no choice at all")
    }

    func testAnotherAgentStartsAfresh() {
        let store = opened()
        store.choose(directory: "/src/x")
        store.choose(provider: "codex")
        store.offered(.success(codex), for: "codex", asked: store.askingOffer(for: "codex"))
        store.choose(.model("gpt-6-astra"))
        store.choose(.mode("plan"))
        store.choose(provider: "claude")
        XCTAssertNil(store.model)
        XCTAssertNil(store.mode)
        XCTAssertNil(store.catalogue, "Claude's catalogue was not asked for")
        XCTAssertNil(store.request?.mode)
        XCTAssertNil(store.request?.newWorktree, "the switch is off until turned on")
    }

    /// An ACP agent's modes name no normal one: unpicked, no mode is sent
    /// and the agent starts in its own; a picked one is sent, never
    /// remembered. With no models listed yet there is no model to pick.
    func testAnAcpAgentsModeIsSentOnlyWhenPicked() {
        let store = opened()
        store.choose(directory: "/src/x")
        store.choose(provider: "grok")
        let modes = Catalogue(
            models: [], commands: [], permissions: [],
            modes: [
                OfferedMode(value: "default", displayName: "Default", normal: false, settable: true, level: 0),
                OfferedMode(value: "plan", displayName: "Plan", normal: false, settable: true, level: 0),
            ],
            skills: [], newAgent: nil)
        store.offered(.success(modes), for: "grok", asked: store.askingOffer(for: "grok"))
        XCTAssertEqual(store.settings?.models, [])
        XCTAssertNil(store.settings?.modes.first(where: \.current), "nothing is in force until picked")
        XCTAssertNil(store.request?.mode)
        store.choose(.mode("plan"))
        XCTAssertEqual(store.request?.mode, "plan")
        XCTAssertNil(store.request?.model)
        store.choose(.mode(nil))
        XCTAssertNil(store.request?.mode, "back to the agent's own")
    }

    func testAnOfferForAHostLeftBehindIsDropped() {
        let store = opened()
        store.choose(provider: "codex")
        let stale = store.askingOffer(for: "codex")
        store.point(at: HostId(UUID()))
        store.offered(.success(codex), for: "codex", asked: stale)
        XCTAssertNil(store.offers["codex"])
        let failed = store.askingOffer(for: "codex")
        store.offered(.failure(RuntimeFailure("no route")), for: "codex", asked: failed)
        XCTAssertEqual(store.offers["codex"], .unavailable)
    }

    func testAskingBothCataloguesLeavesTheListingCurrent() {
        let store = opened()
        let listing = store.asking()
        _ = store.askingOffer(for: "claude")
        _ = store.askingOffer(for: "codex")
        store.listed(.success(listed), asked: listing)
        XCTAssertEqual(store.listing, .ready)
        XCTAssertEqual(store.directory, "/src/amux")
        XCTAssertEqual(store.unasked, ["grok"])
    }
}
