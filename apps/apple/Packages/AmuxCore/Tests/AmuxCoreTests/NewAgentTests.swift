import Foundation
import XCTest

@testable import AmuxCore

@MainActor
final class NewAgentTests: XCTestCase {
    private let listed = Directories(
        recent: [Directory(path: "/src/amux", name: "amux")],
        repositories: [
            Directory(path: "/src/web", name: "web"),
            Directory(path: "/src/relay", name: "relay"),
        ])

    func testTheMostRecentDirectoryIsPrefilledAndTheNameLeftToTheHost() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.remember([Cards.row(1, "amux")])
        store.listed(.success(listed), asked: store.asking())
        XCTAssertEqual(store.listing, .ready)
        XCTAssertEqual(store.directory, "/src/amux")
        XCTAssertEqual(store.name, "", "nobody typed one")
        XCTAssertTrue(store.ready)
        XCTAssertEqual(
            store.request,
            NewAgent(hostId: Cards.desk.bytes, kind: .claudeSdk, cwd: "/src/amux",
                     name: "", effort: nil, mode: nil, model: nil, newWorktree: nil, permission: nil))
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

    func testCodexStartsCodexAndClaudeStartsTheSdkDriverOnAHostThatHasNotSaid() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        XCTAssertEqual(store.request?.kind, .claudeSdk)
        store.choose(provider: .codex)
        XCTAssertEqual(store.request?.kind, .codex)
    }

    /// A host whose person chose terminal Claude starts terminal Claude
    /// from the phone too, so an agent is the same kind wherever it was
    /// started; Codex is Codex on any host.
    func testClaudeStartsAsTheKindTheHostSays() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        store.machineStarts(claude: .claudePty)
        XCTAssertEqual(store.request?.kind, .claudePty)
        store.machineStarts(claude: .claudeSdk)
        XCTAssertEqual(store.request?.kind, .claudeSdk)
        store.machineStarts(claude: .claudePty)
        store.choose(provider: .codex)
        XCTAssertEqual(store.request?.kind, .codex)
        store.point(at: HostId(UUID()))
        store.choose(directory: "/src/x")
        store.choose(provider: .claude)
        XCTAssertEqual(store.request?.kind, .claudeSdk, "another host has not said yet")
    }

    /// The chooser lists the host's repositories, in its person's order, then
    /// where agents worked lately; a repository is not listed twice.
    func testRepositoriesComeBeforeRecentFoldersAndAreListedOnce() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.listed(.success(Directories(
            recent: [
                Directory(path: "/src/relay", name: "relay"),
                Directory(path: "/src/scratch", name: "scratch"),
            ],
            repositories: [
                Directory(path: "/src/web", name: "web"),
                Directory(path: "/src/relay", name: "relay"),
            ])), asked: store.asking())
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
            recent: [], repositories: [Directory(path: "/src/web", name: "web")])),
            asked: store.asking())
        XCTAssertEqual(store.directory, "/src/web")
    }

    func testTheAgentStartsAsTheKindMostRecentlyAtWorkUntilOneIsPicked() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.remember([
            Cards.row(1, "old", minutesAgo: 30, kind: .claudeSdk),
            Cards.row(2, "new", minutesAgo: 2, kind: .codex),
        ])
        XCTAssertEqual(store.provider, .codex)
        store.choose(provider: .claude)
        store.remember([Cards.row(3, "newer", minutesAgo: 1, kind: .codex)])
        XCTAssertEqual(store.provider, .claude, "the fleet arriving does not undo a pick")
        store.open(on: Cards.desk)
        store.remember([])
        XCTAssertEqual(store.provider, .claude, "with no agents, Claude")
    }

    func testANameIsOptionalAndARefusalIsSaid() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
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
            OfferedPermission(value: "default", displayName: "Default", normal: true, neverAsks: false, settable: true, models: []),
            OfferedPermission(value: "auto", displayName: "Auto", normal: false, neverAsks: false, settable: true, models: ["gpt-6-astra"]),
            OfferedPermission(value: "locked", displayName: "Locked", normal: false, neverAsks: false, settable: false, models: []),
        ],
        modes: [
            OfferedMode(value: "default", displayName: "Default", normal: true, settable: true),
            OfferedMode(value: "plan", displayName: "Plan", normal: false, settable: true),
        ],
        skills: [], newAgent: nil)

    func testTheChoicesComeFromTheHostsCatalogueAndGoInTheRequest() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        store.choose(provider: .codex)
        let asked = store.askingOffer(for: .codex)
        XCTAssertNil(store.catalogue, "nothing is offered until the host says")
        store.offered(.success(codex), for: .codex, asked: asked)
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
            NewAgent(hostId: Cards.desk.bytes, kind: .codex, cwd: "/src/x", name: "",
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

    func testAnotherProviderStartsAfreshAndOnlyCodexTakesAMode() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(directory: "/src/x")
        store.choose(provider: .codex)
        store.offered(.success(codex), for: .codex, asked: store.askingOffer(for: .codex))
        store.choose(.model("gpt-6-astra"))
        store.choose(.mode("plan"))
        store.choose(provider: .claude)
        XCTAssertNil(store.model)
        XCTAssertNil(store.mode)
        XCTAssertNil(store.catalogue, "Claude's catalogue was not asked for")
        XCTAssertNil(store.request?.mode)
        XCTAssertNil(store.request?.newWorktree, "the switch is off until turned on")
    }

    func testAnOfferForAHostLeftBehindIsDropped() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        store.choose(provider: .codex)
        let stale = store.askingOffer(for: .codex)
        store.point(at: HostId(UUID()))
        store.offered(.success(codex), for: .codex, asked: stale)
        XCTAssertNil(store.offers[.codex])
        let failed = store.askingOffer(for: .codex)
        store.offered(.failure(RuntimeFailure("no route")), for: .codex, asked: failed)
        XCTAssertEqual(store.offers[.codex], .unavailable)
    }

    func testAskingBothCataloguesLeavesTheListingCurrent() {
        let store = NewAgentStore()
        store.open(on: Cards.desk)
        let listing = store.asking()
        _ = store.askingOffer(for: .claude)
        _ = store.askingOffer(for: .codex)
        store.listed(.success(listed), asked: listing)
        XCTAssertEqual(store.listing, .ready)
        XCTAssertEqual(store.directory, "/src/amux")
    }
}
