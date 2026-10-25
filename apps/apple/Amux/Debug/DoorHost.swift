import AmuxCore
import AmuxDesign
import AmuxFeatures
import AmuxShell
import Foundation
import Observation
import SwiftUI
import UIKit

/// What a driver reaches through the door: the running app itself.
///
/// A driven launch is the app against a served test network: the runtime
/// finds the network's machines in the network's own discovery scope, pairs
/// with them by code or link, and signs in to the network's relay. The door
/// reads what is drawn, presses and types into it, photographs it, and
/// answers questions about the runtime underneath. It never opens a screen
/// with invented state behind it; every picture it takes is of the app.
@MainActor
@Observable
final class DoorHost {
    static let shared = DoorHost()

    private init() {
        if let json = Self.said("amux-cloud-script"),
           let script = try? AmuxJSON.decoder.decode(CloudScript.self, from: Data(json.utf8)) {
            cloud.scripted = script.state
        }
    }

    /// The appearance the driver asked for, worn over whatever the app or
    /// the phone would choose; nothing until it asks.
    private(set) var appearance: Appearance?
    private(set) var design: Design = .app
    /// Whether the needs-you dot is drawn clear, the one small mark a
    /// perturbation can take away.
    private(set) var hidesNeedsYouDot = false
    private(set) var designVariant: DesignVariant = .production
    private(set) var typeSize: DynamicTypeSize = .large
    private(set) var reduceMotion = false
    private(set) var reduceTransparency = false
    /// Whether identified elements report their frames: as the launch asked,
    /// until the `geometry` verb says otherwise.
    private(set) var elementGeometry = ProcessInfo.processInfo.arguments.contains(
        "-\(Door.elementGeometryArgument)")
    /// What the app's root declared: everything no surface handed over.
    @ObservationIgnored var rootDeclared: [IdentifiedElement] = []
    /// What each surface on screen, a sheet among them, declared.
    let surfaces = SurfaceElements()
    let store = ScriptedStoreFront()
    let cloud = DoorCloud()
    let webAuth = ScriptedWebAuth()
    @ObservationIgnored private weak var composition: Composition?
    /// Where a push handed over through the door goes: the app's own handler.
    @ObservationIgnored private weak var delegate: AppDelegate?
    /// The background time asked for, while the driver hands over a push.
    @ObservationIgnored private var held: UIBackgroundTaskIdentifier = .invalid
    /// Where the person went, and what they changed about how the app looks,
    /// as a report's view-state recording carries it.
    @ObservationIgnored private var events: [TraceEvent] = []

    func adopt(_ delegate: AppDelegate) {
        self.delegate = delegate
        composition = delegate.composition
    }

    private var stores: StoreBundle? { composition?.stores }

    /// What the app wears: the driver's appearance once it has asked for
    /// one, else the app's own choice. One preference for both, because the
    /// driven root's outranks the app's beneath it.
    var worn: Appearance? { appearance ?? composition?.appearance }

    // MARK: - The report's recording

    func arrived(at place: Place) {
        guard events.last != .route(place) else { return }
        events.append(.route(place))
    }

    func trace(route: String?) -> Result<String, PartAbsent> {
        let account = composition?.accounts.selectedAccount
        let now = Date()
        let lines = events + [.account(account), .frozen(at: now, ordered: now)]
        do { return .success(try Trace.lines(lines)) } catch {
            return .failure(PartAbsent("the view-state recording could not be written"))
        }
    }

    // MARK: - Requests

    func handle(_ request: DoorRequest) async -> DoorReply {
        switch request {
        case .cloud(let script):
            cloud.scripted = script.state
            return .ack
        case .store(let script):
            store.scripted = script.state
            return .ack
        case .calls:
            return .calls(cloud: cloud.calls.map(Self.said), store: store.calls.map(Self.said))
        case .accounts: return .accounts(accountsState())
        case .connect(let relay, let token, let user, _):
            return await signIn(user: user, relay: relay, token: token)
        case .addAccount(let user, let token):
            guard let relay = Self.said(Door.relayArgument) else {
                return .error("the launch named no relay to sign in to")
            }
            return await signIn(user: user, relay: relay, token: token)
        case .awaitReconciled(let seconds):
            return await until(seconds, "the fleet to be read") { self.stores?.applied ?? 0 > 0 }
        case .awaitOffline(let seconds):
            return await until(seconds, "a host to go offline") {
                self.stores?.hosts.hosts.contains { !$0.online } ?? false
            }
        case .runtimeLog(let bytes):
            return .runtimeLog(composition?.runtime.logTail(bytes: bytes) ?? "")
        case .conversation(let agent): return reading(agent)
        case .signposts: return .signposts(Signposts.marks)
        case .condition: return .condition(DeviceCondition.now())
        case .measure(let seconds): return await measure(seconds)
        case .appearance(let appearance):
            await wear(appearance)
            events.append(.appearance(appearance))
            return .ack
        case .perturb(let token):
            guard let token else {
                design = .app
                hidesNeedsYouDot = false
                return .ack
            }
            if token == Perturbation.needsYouDot {
                hidesNeedsYouDot = true
                return .ack
            }
            guard let moved = Perturbation.design(.app, moving: token) else {
                return .error("the design has no colour token named \(token)")
            }
            design = moved
            return .ack
        case .designVariant(let name):
            guard let variant = DesignVariant(name: name) else {
                return .error("no design variant named \(name ?? "nil")")
            }
            designVariant = variant
            design = variant.design
            return .ack
        case .dynamicType(let name):
            guard let size = DynamicTypeSize(doorName: name) else {
                return .error("no type size named \(name)")
            }
            typeSize = size
            DoorWindow.wear(size)
            events.append(.dynamicType(name))
            return .ack
        case .assist(let motion, let transparency):
            reduceMotion = motion
            reduceTransparency = transparency
            return .ack
        case .geometry(let on):
            elementGeometry = on
            // The frames arrive with the next render; a tap asked for on the
            // ack's heels would otherwise find none to hit.
            for _ in 0..<3 where on && !declared.contains(where: { $0.frame != .zero }) {
                await DoorFrames.next()
            }
            return .ack
        case .screenshot:
            NotificationCenter.default.post(
                name: UIApplication.userDidTakeScreenshotNotification, object: nil)
            return .ack
        case .settle:
            await settle()
            return .ack
        case .query: return query()
        case .capture(let path): return await capture(to: path)
        // A finger lands where things are, and the system draws a sheet or a
        // page at its final place before it has slid there: acted on
        // mid-slide, a point aimed at a row in a rising sheet lands on what
        // the sheet is about to cover. So every action waits for stillness.
        case .tap(let identifier):
            await still()
            await reveal(identifier)
            return tap(identifier)
        case .choose(let label): await still(); return choose(label)
        case .perform(let identifier, let action): await still(); return perform(action, on: identifier)
        case .type(let identifier, let text): await still(); return type(text, into: identifier)
        case .clear(let identifier): await still(); return clear(identifier)
        case .search(let text): await still(); return search(text)
        case .scroll(let direction, let identifier): await still(); return scroll(direction, holding: identifier)
        case .paste(let identifier, let text): await still(); return paste(text, into: identifier)
        case .pair(let qr): return await pair(link: qr)
        case .pairByCode(let host, let pin): return await pair(pin: pin, on: host)
        case .localNetwork(let permission):
            guard let seen = Self.permission(permission) else {
                return .error("no local network permission named \(permission)")
            }
            // The stand-in holds for the rest of this launch: the real
            // browser stops reporting, and the profile, which hands its
            // stores the permission last reported when it comes up, is
            // waited for so it cannot overwrite the stand-in either.
            composition?.runtime.discovery?.permissionChanged = nil
            guard case .ack = await until(30, "the account running", { self.stores?.profile != nil }),
                  let stores
            else { return .error("nothing is running") }
            stores.hosts.sawLocalNetwork(seen)
            return .ack
        case .revoke(let host):
            guard let id = HostId(host) else { return .error("no host named \(host)") }
            stores?.revoke(id)
            return .ack
        case .awaitAgent(let agent, let seconds):
            return await until(seconds, "\(agent) in the fleet") {
                self.stores?.fleet.rows.contains { $0.name == agent || "\($0.id)" == agent }
                    ?? false
            }
        case .shutdown: return .ack
        case .open(let screen, let subject): return open(screen, about: subject)
        case .attach(let agent, let kind, let name, let mime, let base64):
            guard let data = Data(base64Encoded: base64) else { return .error("not base64") }
            return chatting(agent) { model in
                model.attach(data, name: name, mime: mime, image: kind == "image")
                return .ack
            }
        case .send(let agent, let text):
            return chatting(agent) { model in
                model.draft = text
                guard model.canSend else {
                    return .sendAttempt(delivered: false, reason: model.frame?.waiting.map { "\($0)" })
                }
                model.send()
                return .sendAttempt(delivered: true, reason: nil)
            }
        case .awaitSendable(let agent, let seconds):
            guard let model = chat(agent) else { return .error("no agent named \(agent)") }
            return await until(seconds, "\(agent) taking a message") {
                model.frame?.caughtUp == true && model.frame?.composer.mode == .send
            }
        case .awaitReply(let agent, let saying, let seconds):
            guard let model = chat(agent) else { return .error("no agent named \(agent)") }
            return await until(seconds, "\(agent) saying \(saying)") {
                model.ids.contains { id in
                    guard case .prose(let text, _, _)? = model.cell(for: id).row?.kind else {
                        return false
                    }
                    return ChatWords.text(of: text).contains(saying)
                }
            }
        case .uploaded(let path): return writeUploaded(to: path)
        case .holdBackground: return holdBackground()
        case .awaitBackground(let seconds):
            return await until(seconds, "the app in the background") {
                UIApplication.shared.applicationState == .background
                    && self.composition?.runtime.active == false
            }
        case .push(let path): return await push(from: path)
        case .refreshEntitlement, .late, .restoreSession, .bridge, .setModel, .states,
             .report, .move, .requestChanges, .watch, .sendDraft:
            return .error("this build's door does not \(Self.verb(request))")
        }
    }

    /// Goes where a person would tap to: a tab, or a page of the running app
    /// about the machine or agent `subject` names. Nothing is invented: a page
    /// about something the runtime does not list is refused.
    private func open(_ screen: String, about subject: String?) -> DoorReply {
        guard let composition, let stores else { return .error("the app has not started") }
        let router = composition.router
        let agent = subject.flatMap { named in
            stores.fleet.rows.first { $0.name == named || "\($0.id)" == named }
        }
        let host = subject.flatMap { named in
            (stores.hosts.hosts + stores.hosts.discovered).first {
                $0.name == named || $0.id?.description == named
            }
        }
        switch screen {
        case "home": router.setPath([], for: .agents); router.select(.agents)
        case "hosts":
            router.setPath([], for: .hosts)
            router.select(.hosts)
        case "you", "settings": router.setPath([], for: .you); router.select(.you)
        case "new-agent": router.composing = true
        case "sign-in":
            composition.handle(.signIn)
        case "paywall":
            composition.handle(.subscribe)
        case "pin":
            guard let id = host?.id else { return .error("no host named \(subject ?? "")") }
            router.pairing = .code(id)
        case "conversation":
            guard let agent else { return .error("no agent named \(subject ?? "")") }
            router.open(.conversation(agent.id))
        case "family":
            guard let agent else { return .error("no agent named \(subject ?? "")") }
            stores.toggleFamily(agent.id)
        default:
            return .error("the running app has no page named \(screen)")
        }
        return .ack
    }

    /// Signs the launch's account in to the served network's relay, the way
    /// the app keeps a sign-in: its profile is bound with the relay's own
    /// login, what the account may do is read from the network's account
    /// service, and the account goes on screen.
    private func signIn(user: String, relay: String, token: String) async -> DoorReply {
        guard let composition, let service = URL(string: relay) else {
            return .error("nothing to sign in with")
        }
        let account = AccountId(user)
        await cloud.read(plansFrom: service)
        switch await composition.runtime.bind(
            account, cloud: service, client: "cli", refreshToken: token) {
        case .success:
            // The runtime lends the read its bearer once the bound profile
            // is listed, which can be a moment after the bind returns.
            let deadline = Date().addingTimeInterval(15)
            while true {
                do {
                    let plan = try await cloud.entitlement(account)
                    composition.accounts.entitlement(plan, for: account)
                    return .ack
                } catch {
                    guard Date() < deadline else {
                        return .error("the account service did not say what \(user) may do: \(error)")
                    }
                    try? await Task.sleep(for: .milliseconds(200))
                }
            }
        case .failure(let why): return .error(why.description)
        }
    }

    private func pair(link: String) async -> DoorReply {
        // The door opens before the runtime has brought the account's
        // profile up, so a link handed over straight after launch waits
        // for it rather than being refused.
        guard case .ack = await until(30, "the account running", { self.stores?.profile != nil }),
              let stores, stores.pair(link: link)
        else { return .error("nothing is running") }
        return await confirm(in: stores)
    }

    private func pair(pin: String, on host: String) async -> DoorReply {
        guard let stores, let id = HostId(host) else { return .error("no host named \(host)") }
        stores.pairing.open(machine: stores.hosts.known(id))
        guard stores.pair(digits: pin) else { return .error("the code was not six digits") }
        return await confirm(in: stores)
    }

    /// Trusts whoever the pairing reached, and answers its name.
    private func confirm(in stores: StoreBundle) async -> DoorReply {
        guard case .confirming(let pending) = await settled(stores.pairing) else {
            return .error(
                "the pairing was refused: \(stores.pairing.refusal ?? "\(stores.pairing.phase)")")
        }
        stores.confirmPairing(pending)
        guard case .trusted(let name) = await settled(stores.pairing) else {
            return .error(
                "the machine was not trusted: \(stores.pairing.refusal ?? "\(stores.pairing.phase)")")
        }
        return .paired(host: name)
    }

    private func settled(_ pairing: PairingStore) async -> PairingStore.Phase {
        let deadline = Date().addingTimeInterval(30)
        while pairing.phase == .checking, Date() < deadline {
            try? await Task.sleep(for: .milliseconds(50))
        }
        return pairing.phase
    }

    private func until(
        _ seconds: Double, _ what: String, _ check: @MainActor () -> Bool
    ) async -> DoorReply {
        let deadline = Date().addingTimeInterval(seconds)
        while !check() {
            guard Date() < deadline else { return .error("never saw \(what)") }
            try? await Task.sleep(for: .milliseconds(50))
        }
        return .ack
    }

    /// Watches the display, the main thread and the footprint for `seconds`
    /// while the driver runs its workload, and answers what they did. Taken
    /// on the main actor so the CPU reading is the main thread's own; the
    /// watch's display link is not one the app asked for, so the idle count
    /// reads only the app's ticks.
    private func measure(_ seconds: Double) async -> DoorReply {
        let watch = FrameWatch()
        let cpu = CPUWatch()
        let idleTicks = Signposts.count(.idleTick)
        let commits = Signposts.count(.transcriptCommit)
        watch.start()
        try? await Task.sleep(for: .seconds(seconds))
        let hitch = watch.stop()
        return .measurement(Measurement(
            seconds: seconds, frames: watch.frames, hitchMsPerS: hitch,
            mainThreadCpuPercent: cpu.percent(), footprintMB: Footprint.megabytes(),
            idleTicks: Signposts.count(.idleTick) - idleTicks,
            transcriptCommits: Signposts.count(.transcriptCommit) - commits))
    }

    /// The chat a page holds for the agent a driver names, opened as the page
    /// would open it.
    private func chat(_ agent: String) -> ChatModel? {
        guard let stores,
              let row = stores.fleet.rows.first(where: { $0.name == agent || "\($0.id)" == agent })
        else { return nil }
        return try? stores.chat(row.id)
    }

    private func chatting(_ agent: String, _ body: (ChatModel) -> DoorReply) -> DoorReply {
        guard let model = chat(agent) else { return .error("no agent named \(agent)") }
        return body(model)
    }

    /// The chat the page on screen shows, if it shows one.
    private var shownChat: (agent: AgentKey, model: ChatModel)? {
        guard let stores, case .conversation(let agent)? = composition?.router.top,
              let model = try? stores.chat(agent)
        else { return nil }
        return (agent, model)
    }

    /// An agent's chat as the page on screen holds it, rows paged in and
    /// all; for a chat no page shows, as the runtime holds it, opened for
    /// the reading and closed again.
    private func reading(_ agent: String) -> DoorReply {
        guard let stores,
              let row = stores.fleet.rows.first(where: { $0.name == agent || "\($0.id)" == agent })
        else { return .error("no agent named \(agent)") }
        if let shown = shownChat, shown.agent == row.id {
            let model = shown.model
            return .conversation(ConversationReading(
                agent: agent, frame: model.frame,
                rows: model.ids.compactMap { model.cell(for: $0).row }, ask: model.ask))
        }
        do {
            let chat = try stores.openChat(row.id) { _ in }
            defer { stores.closeChat(chat) }
            let opening = chat.opening()
            // An opening always hands the window over whole.
            let keys = if case .reload(let keys)? = opening.keys { keys } else { [String]() }
            return .conversation(ConversationReading(
                agent: agent, frame: opening.frame, rows: chat.rows(for: keys),
                ask: opening.ask?.card))
        } catch {
            return .error(error.description)
        }
    }

    /// The last report the scripted account service was handed, written part
    /// by part into `path`: what a Send actually carried, read at the
    /// boundary it left the app through.
    private func holdBackground() -> DoorReply {
        let application = UIApplication.shared
        if held != .invalid { application.endBackgroundTask(held) }
        held = application.beginBackgroundTask(withName: "door push") { [weak self] in
            self?.release()
        }
        return held == .invalid ? .error("the system gave no background time") : .ack
    }

    private func release() {
        guard held != .invalid else { return }
        UIApplication.shared.endBackgroundTask(held)
        held = .invalid
    }

    /// The payload goes to the handler iOS calls, in the state iOS calls it
    /// in; the background time is given back after, as a woken app's is.
    private func push(from path: String) async -> DoorReply {
        defer { release() }
        guard let delegate else { return .error("the app has no delegate to hand a push to") }
        guard let data = FileManager.default.contents(atPath: path),
              let payload = try? JSONSerialization.jsonObject(with: data) as? [AnyHashable: Any]
        else { return .error("no push payload at \(path)") }
        let application = UIApplication.shared
        let reached = await until(10, "the app in the background") {
            application.applicationState == .background
        }
        guard case .ack = reached else { return reached }
        _ = await delegate.application(application, didReceiveRemoteNotification: payload)
        return .ack
    }

    private func writeUploaded(to path: String) -> DoorReply {
        guard let bundle = cloud.uploaded.last else { return .error("no report was sent") }
        let directory = URL(fileURLWithPath: path, isDirectory: true)
        let manager = FileManager.default
        do {
            try? manager.removeItem(at: directory)
            try manager.createDirectory(at: directory, withIntermediateDirectories: true)
            for part in bundle.parts {
                guard let data = part.data else { continue }
                let file = directory.appendingPathComponent(part.name)
                try manager.createDirectory(
                    at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
                try data.write(to: file)
            }
        } catch {
            return .error("the report could not be written: \(error.localizedDescription)")
        }
        let header = bundle.part("report.json")?.data.flatMap { String(data: $0, encoding: .utf8) }
        return .bundle(path: path, parts: bundle.parts.map(\.name), reportJSON: header)
    }

    private func accountsState() -> AccountsState {
        let accounts = composition?.accounts
        return AccountsState(
            selected: accounts?.selected?.value,
            accounts: accounts?.accounts.map {
                AccountsState.Known(
                    id: $0.id.value, email: $0.account.email, signedIn: $0.signedIn,
                    entitlement: Self.named($0.entitlement), hosts: $0.hosts,
                    attention: $0.attention)
            } ?? [],
            dropped: accounts?.dropped ?? 0)
    }

    private static func named(_ entitlement: Entitlement) -> String {
        switch entitlement {
        case .none: "none"
        case .active: "active"
        case .lapsed: "lapsed"
        }
    }

    static func said(_ call: CloudCall) -> String {
        switch call {
        case .signIn(.adding): "signIn select"
        case .signIn(.returning(let account)): "signIn hint \(account.email)"
        case .handOver(let id): "handOver \(id)"
        case .forgetSession(let id): "forgetSession \(id)"
        case .account(let id): "account \(id)"
        case .entitlement(let id): "entitlement \(id)"
        case .recordPurchase(let id): "recordPurchase \(id)"
        case .requestDeletion(let id, let email): "requestDeletion \(id) as \(email)"
        case .uploadReport(let id, let parts):
            "uploadReport \(id) \(parts.joined(separator: ","))"
        }
    }

    static func said(_ call: StoreCall) -> String {
        switch call {
        case .plans: "plans"
        case .buy(let plan): "buy \(plan)"
        case .restore: "restore"
        case .finish(let transaction): "finish \(transaction)"
        case .unfinished: "unfinished"
        }
    }

    private static func verb(_ request: DoorRequest) -> String {
        let data = (try? JSONEncoder().encode(request)) ?? Data()
        let fields = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        return (fields?["kind"] as? String) ?? "that"
    }

    private static func permission(_ named: String) -> LocalNetworkPermission? {
        switch named {
        case "granted": .granted
        case "denied": .denied
        case "unknown": .unknown
        default: nil
        }
    }

    /// What the launch said after `-name`, or nothing where it did not say it.
    ///
    /// Read off the arguments themselves rather than through the defaults. The
    /// defaults parse an argument's value as a property list, so a pairing
    /// payload — which is JSON, and JSON braces are a plist dictionary — comes
    /// back as a dictionary and `string(forKey:)` answers nothing at all.
    nonisolated static func said(_ name: String) -> String? {
        let arguments = ProcessInfo.processInfo.arguments
        guard let flag = arguments.firstIndex(of: "-\(name)"),
            arguments.index(after: flag) < arguments.endIndex
        else { return nil }
        return arguments[arguments.index(after: flag)]
    }

    /// A link the launch carried, for the app to open as if the system had
    /// handed it one. Nothing for an ordinary launch.
    static var linkAsLaunchAsks: URL? {
        said(Door.linkArgument).flatMap(URL.init(string:))
    }

    // MARK: - Reading and driving what is drawn

    /// Replaced rather than moved: a material already on screen cross-fades
    /// over a length of time nobody publishes.
    private func wear(_ appearance: Appearance) async {
        // Worn as a SwiftUI preference above the app's own, not as the
        // window's interface style: SwiftUI re-applies its preferred scheme to
        // the window when a page is pushed, which undid a window override
        // now and then, and the next photograph came out in the other one.
        self.appearance = appearance
        for _ in 0..<2 { await DoorFrames.next() }
    }

    private func settle() async {
        await still()
        guard let window = DoorWindow.current else { return }
        _ = await steady(window)
    }

    /// Until the system has stopped moving things: a sheet, a page, the
    /// keyboard. Bounded, because something on screen may move for as long
    /// as it is there; the system's own transitions take well under a
    /// second.
    private func still() async {
        // What was just asked of the app starts moving at its next frame.
        for _ in 0..<2 { await DoorFrames.next() }
        let started = Date()
        while DoorWindow.animating, Date().timeIntervalSince(started) < 2 {
            await DoorFrames.next()
        }
        // The frames the screen reports are laid out a frame after it
        // stops.
        for _ in 0..<2 { await DoorFrames.next() }
    }

    /// Every element the screens declared, with where it is drawn, in the
    /// order they are stacked: the root, then each surface, the top one last.
    private var declared: [IdentifiedElement] {
        rootDeclared + surfaces.stacked.flatMap { $0 }
    }

    /// The element a request names, found on the top surface first, so a
    /// sheet's control is the one acted on and not one under it that shares
    /// its name.
    private func declared(named identifier: String) -> IdentifiedElement? {
        for surface in ([rootDeclared] + surfaces.stacked).reversed() {
            if let found = surface.first(where: { $0.identifier == identifier }) { return found }
        }
        return nil
    }

    private func query() -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        let declared = self.declared.map {
            VisibleElement(
                identifier: $0.identifier, label: $0.label, value: $0.value,
                frame: VisibleFrame(
                    x: $0.frame.origin.x, y: $0.frame.origin.y,
                    width: $0.frame.width, height: $0.frame.height),
                enabled: $0.enabled)
        }
        let named = Set(declared.map(\.identifier))
        let windows = [window] + DoorWindow.others
        let leaves = (windows.flatMap(VisibleTree.elements(of:)) + windows.flatMap(TabSource.tabs(in:)))
            .filter { !named.contains($0.identifier) }
        return .state(VisibleState(
            screen: composition.map { shown in
                let router = shown.router
                if router.composing { return "new-agent" }
                if router.showsPairing, let pairing = router.pairing { return pairing.name }
                return router.top?.name ?? router.tab.rawValue
            } ?? "none",
            typeSize: typeSize.doorName,
            voiceOver: UIAccessibility.isVoiceOverRunning,
            elements: declared + leaves,
            reconciled: (stores?.applied ?? 0) > 0,
            unconfirmed: 0))
    }

    private func capture(to path: String) async -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard let (image, data) = await steady(window) else {
            return .error("capture failed: the window would not draw")
        }
        return DoorCapture.write(image, data, to: path)
    }

    private func steady(_ window: UIWindow) async -> (UIImage, Data)? {
        var previous: Data?
        var last: (UIImage, Data)?
        for _ in 0..<30 {
            guard let image = DoorCapture.render(of: window), let data = image.pngData()
            else { return last }
            last = (image, data)
            if data == previous { return last }
            previous = data
            await DoorFrames.next()
        }
        return last
    }

    /// An element out of sight is brought into it first, as VoiceOver does
    /// when it moves to one: a card a strip has scrolled past, or a row
    /// under the tab bar, is still the screen's, and a person would swipe
    /// to it.
    private func reveal(_ identifier: String) async {
        guard let window = DoorWindow.current, let declared = declared(named: identifier) else { return }
        let middle = CGPoint(x: declared.frame.midX, y: declared.frame.midY)
        guard let list = VisibleTree.scroller(holding: middle, in: window) else { return }
        // What the list shows clear of the bars laid over its edges.
        let clear = list.bounds.inset(by: list.adjustedContentInset)
        let wanted = list.convert(declared.frame, from: window)
        guard !clear.contains(wanted) else { return }
        // By hand rather than `scrollRectToVisible`, which counts what is
        // under the bars as shown.
        let inset = list.adjustedContentInset
        func moved(_ at: CGFloat, low: CGFloat, high: CGFloat, from: CGFloat, to: CGFloat,
                   least: CGFloat, most: CGFloat) -> CGFloat {
            var at = at
            if high > to { at += high - to } else if low < from { at -= from - low }
            return min(max(at, least), most)
        }
        let offset = CGPoint(
            x: moved(list.contentOffset.x, low: wanted.minX, high: wanted.maxX, from: clear.minX, to: clear.maxX,
                     least: -inset.left, most: max(-inset.left, list.contentSize.width + inset.right - list.bounds.width)),
            y: moved(list.contentOffset.y, low: wanted.minY, high: wanted.maxY, from: clear.minY, to: clear.maxY,
                     least: -inset.top, most: max(-inset.top, list.contentSize.height + inset.bottom - list.bounds.height)))
        list.setContentOffset(offset, animated: false)
        // The screen reports where things went once it has laid them out.
        for _ in 0..<3 { await DoorFrames.next() }
    }

    private func tap(_ identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard let element = element(named: identifier, in: window) else {
            return .error("no element named \(identifier)")
        }
        // What VoiceOver does to a control, which is the one way to act on a
        // SwiftUI element from inside the process.
        if element.accessibilityActivate() { return .ack }
        if let declared = declared(named: identifier),
           let under = VisibleTree.element(
               at: CGPoint(x: declared.frame.midX, y: declared.frame.midY), in: window,
               saying: declared.label),
           under.accessibilityActivate() {
            return .ack
        }
        if let label = declared(named: identifier)?.label,
           let saying = VisibleTree.element(saying: label, in: window),
           saying.accessibilityActivate() {
            return .ack
        }
        // Where it was aimed and what is there, so a refusal says whether
        // the element was covered, out of sight or simply would not act.
        let aimed = declared(named: identifier).map { CGPoint(x: $0.frame.midX, y: $0.frame.midY) }
        let hit = aimed.flatMap { window.hitTest($0, with: nil) }.map { String(describing: Swift.type(of: $0)) }
        return .error("\(identifier) did not activate (aimed at \(aimed.map { "\(Int($0.x)),\(Int($0.y))" } ?? "nowhere"), which reaches \(hit ?? "nothing"))")
    }

    /// Picks an item by its title, from a dialog on screen, the tab bar or
    /// a menu.
    ///
    /// A dialog's buttons and the tab bar's tabs carry no name the process
    /// can read, only their titles, so they are found by title and pressed.
    /// A presented menu's rows cannot be activated from inside the process,
    /// so the item is run as the menu's own action, found on the control
    /// that presents the menu, and the menu is put away as a tap would.
    private func choose(_ label: String) -> DoorReply {
        let windows = [DoorWindow.current].compactMap { $0 } + DoorWindow.others
        for window in windows {
            guard let dialog = DialogSource.shown(in: window) else { continue }
            if DialogSource.press(label, in: dialog) { return .ack }
            return .error("the dialog on screen offers no \(label)")
        }
        for window in windows where TabSource.press(label, in: window) {
            return .ack
        }
        for window in windows {
            guard let (button, action) = MenuSource.item(label, in: window) else { continue }
            button.contextMenuInteraction?.dismissMenu()
            MenuSource.run(action, from: button)
            return .ack
        }
        return .error("no dialog, tab or menu offers \(label)")
    }

    private func perform(_ action: String, on identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        var candidates = [element(named: identifier, in: window)].compactMap { $0 }
        if let declared = declared(named: identifier),
           let under = VisibleTree.element(
               at: CGPoint(x: declared.frame.midX, y: declared.frame.midY), in: window,
               saying: declared.label) {
            candidates.append(under)
        }
        for candidate in candidates {
            guard let custom = candidate.accessibilityCustomActions?
                .first(where: { $0.name == action }) else { continue }
            if let handler = custom.actionHandler {
                if handler(custom) { return .ack }
            } else if let target = custom.target as? NSObject {
                _ = target.perform(custom.selector, with: custom)
                return .ack
            }
        }
        return .error("\(identifier) offers no action named \(action)")
    }

    private func type(_ text: String, into identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard let element = element(named: identifier, in: window) else {
            return .error("no element named \(identifier)")
        }
        if let input = writable(named: identifier, in: window) {
            if !input.isFirstResponder { _ = input.becomeFirstResponder() }
            input.insertText(text)
            return .ack
        }
        guard let input = element as? UIKeyInput else {
            return .error("\(identifier) does not take text")
        }
        input.insertText(text)
        return .ack
    }

    /// The list on top, moved the way a swipe moves it: a page is most of
    /// what shows, so a row near the edge stays in sight as the reader's
    /// anchor.
    private func scroll(_ direction: String, holding identifier: String?) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        let found: UIScrollView?
        if let identifier {
            guard let declared = declared(named: identifier) else {
                return .error("no element named \(identifier)")
            }
            found = VisibleTree.list(holding: CGPoint(x: declared.frame.midX, y: declared.frame.midY), in: window)
        } else {
            found = VisibleTree.list(in: window)
        }
        guard let list = found else { return .error("nothing scrolls there") }
        let inset = list.adjustedContentInset
        let top = -inset.top
        let bottom = max(top, list.contentSize.height + inset.bottom - list.bounds.height)
        let page = list.bounds.height * 0.8
        let y: CGFloat
        switch direction {
        case "up": y = max(top, list.contentOffset.y - page)
        case "down": y = min(bottom, list.contentOffset.y + page)
        case "top": y = top
        case "bottom": y = bottom
        default: return .error("scroll goes up, down, top or bottom, not \(direction)")
        }
        // A person's drag up leaves the newest row, and the chat stops
        // following new rows when the drag ends. An offset set here has no
        // drag, so the shown chat is told, or its next row pulls the list back
        // down.
        if y < list.contentOffset.y { shownChat?.model.reading(atNewest: false) }
        list.setContentOffset(CGPoint(x: list.contentOffset.x, y: y), animated: true)
        return .ack
    }

    private func clear(_ identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard element(named: identifier, in: window) != nil else {
            return .error("no element named \(identifier)")
        }
        guard let input = writable(named: identifier, in: window) as? (any UITextInput & UIResponder)
        else { return .error("\(identifier) does not take text") }
        if !input.isFirstResponder { _ = input.becomeFirstResponder() }
        if input.hasText {
            guard let all = input.textRange(
                from: input.beginningOfDocument, to: input.endOfDocument)
            else { return .error("\(identifier) could not select its text") }
            input.selectedTextRange = all
            input.deleteBackward()
        }
        guard !input.hasText else { return .error("\(identifier) would not empty") }
        return .ack
    }

    /// The search field on show, made to hold `text`. Focusing it is what
    /// pulls a field the list has tucked away back into view, as a person's
    /// pull does.
    private func search(_ text: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard let field = DoorWindow.searchField(in: window) else {
            return .error("no search field on screen")
        }
        if !field.isFirstResponder { _ = field.becomeFirstResponder() }
        if field.hasText,
           let all = field.textRange(from: field.beginningOfDocument, to: field.endOfDocument) {
            field.selectedTextRange = all
            field.deleteBackward()
        }
        if !text.isEmpty { field.insertText(text) }
        return .ack
    }

    private func paste(_ text: String, into identifier: String) -> DoorReply {
        guard let window = DoorWindow.current else { return .error("no window on screen") }
        guard element(named: identifier, in: window) != nil else {
            return .error("no element named \(identifier)")
        }
        guard let input = writable(named: identifier, in: window) else {
            return .error("\(identifier) does not take a paste")
        }
        if !input.isFirstResponder { _ = input.becomeFirstResponder() }
        UIPasteboard.general.string = text
        input.paste(nil)
        return .ack
    }

    private func writable(
        named identifier: String, in window: UIWindow
    ) -> (any UIKeyInput & UIResponder)? {
        if let view = element(named: identifier, in: window) as? UIView,
           let input = DoorWindow.textInput(in: view) {
            return input
        }
        if let declared = declared(named: identifier),
           let under = window.hitTest(
               CGPoint(x: declared.frame.midX, y: declared.frame.midY), with: nil),
           let input = DoorWindow.textInput(in: under) {
            return input
        }
        if let focused = DoorWindow.focused(in: window) { return focused }
        let inputs = DoorWindow.allTextViews(in: window)
        // The field drawn where the screen says it is: a page under a
        // pushed one keeps its own fields, the agent list's search among
        // them, so there is seldom only one.
        if let declared = declared(named: identifier) {
            let middle = CGPoint(x: declared.frame.midX, y: declared.frame.midY)
            let drawn = inputs.last { input in
                !input.isHidden && input.window != nil
                    && input.convert(input.bounds, to: window).contains(middle)
            }
            if let drawn = drawn as? (any UIKeyInput & UIResponder) { return drawn }
        }
        if inputs.count == 1, let sole = inputs.first as? (any UIKeyInput & UIResponder) {
            return sole
        }
        return nil
    }

    private func element(named identifier: String, in window: UIWindow) -> NSObject? {
        if let found = VisibleTree.find(identifier, in: window) { return found }
        for other in DoorWindow.others {
            if let found = VisibleTree.find(identifier, in: other) { return found }
        }
        guard let declared = declared(named: identifier) else {
            return nil
        }
        return window.hitTest(CGPoint(x: declared.frame.midX, y: declared.frame.midY), with: nil)
    }
}

/// How a driven launch runs the app: in its served network's discovery scope,
/// with direct links on loopback, finding only the machines it names.
@MainActor
enum Launch {
    static var options: RuntimeCoordinator.Options {
        RuntimeCoordinator.Options(
            discoveryScope: DoorHost.said(Door.discoveryScopeArgument) ?? "",
            lanBind: DoorHost.said(Door.lanBindArgument),
            relayTCP: DoorHost.said(Door.relayTCPArgument),
            relayQUIC: DoorHost.said(Door.relayQUICArgument),
            relayRoot: DoorHost.said(Door.relayRootArgument))
    }

    static var discoverable: Set<HostId>? {
        guard let named = DoorHost.said(Door.discoverOnlyArgument) else { return nil }
        return Set(named.split(separator: ",").compactMap { HostId(String($0)) })
    }
}

/// The root a debug build draws: the app, under the appearance, type size and
/// design the door was asked for, reporting every element it declares.
struct DrivenRoot<Content: View>: View {
    @Environment(\.accessibilityVoiceOverEnabled) private var voiceOver
    @State private var host = DoorHost.shared
    private let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        content
            .preferredColorScheme(host.worn?.colorScheme)
            .environment(\.design, host.design)
            .environment(\.hidesNeedsYouDot, host.hidesNeedsYouDot)
            .modifier(DesignVariantLayout(variant: host.designVariant))
            .dynamicTypeSize(host.typeSize)
            .transformEnvironment(\.reducesMotion) { $0 = $0 || host.reduceMotion }
            .transformEnvironment(\.reducesTransparency) { $0 = $0 || host.reduceTransparency }
            .reportingIdentifiedElements(includeGeometry: !voiceOver && host.elementGeometry)
            .environment(\.surfaceElements, host.surfaces)
            .onPreferenceChange(IdentifiedElements.self) { declared in
                Task { @MainActor in DoorHost.shared.rootDeclared = declared }
            }
    }
}

/// The account service a driven launch talks to: the scripted one, except
/// that once the door has signed an account in to a served network, what an
/// account may do is read from that network's own account service. A plan
/// the network changes then reaches the phone wherever the app reads it, as
/// a real one does from amux.sh, rather than the plan scripted at launch.
final class DoorCloud: CloudService, @unchecked Sendable {
    let script = ScriptedCloudService()
    private let lock = NSLock()
    private var served: AmuxCloudService?
    private var lender: (@Sendable (AccountId) async -> String?)?

    var scripted: ScriptedCloudState {
        get { script.scripted }
        set { script.scripted = newValue }
    }

    var calls: [CloudCall] { script.calls }
    var uploaded: [ReportBundle] { script.uploaded }

    /// Reads plans from the account service at `base` from now on, with the
    /// bearer the runtime holds for the account.
    func read(plansFrom base: URL) async {
        let service = AmuxCloudService(endpoint: CloudEndpoint(
            base: base, clientID: "cli", callback: CloudEndpoint.production.callback,
            scopes: []))
        if let lender = lock.withLock({ self.lender }) {
            await service.lend(from: lender)
        }
        lock.withLock { served = service }
    }

    func signIn(
        _ intent: SignInIntent, presenting: any WebAuthPresenter
    ) async throws(CloudError) -> SignedInAccount {
        try await script.signIn(intent, presenting: presenting)
    }

    func handOver(_ id: AccountId) async -> Handover? {
        await script.handOver(id)
    }

    func forgetSession(_ id: AccountId) async throws {
        try await script.forgetSession(id)
    }

    func lend(from lender: @escaping @Sendable (AccountId) async -> String?) async {
        let served = lock.withLock {
            self.lender = lender
            return self.served
        }
        await script.lend(from: lender)
        await served?.lend(from: lender)
    }

    func account(_ id: AccountId) async throws(CloudError) -> AccountFacts {
        var facts = try await script.account(id)
        if lock.withLock({ served }) != nil {
            facts.entitlement = try await entitlement(id)
        }
        return facts
    }

    func entitlement(_ id: AccountId) async throws(CloudError) -> Entitlement {
        guard let served = lock.withLock({ self.served }) else {
            return try await script.entitlement(id)
        }
        return try await served.entitlement(id)
    }

    func recordPurchase(_ id: AccountId, signedTransaction: String) async throws(CloudError) {
        try await script.recordPurchase(id, signedTransaction: signedTransaction)
    }

    func requestDeletion(
        _ id: AccountId, confirmedEmail: String
    ) async throws(CloudError) -> DeletionOutcome {
        try await script.requestDeletion(id, confirmedEmail: confirmedEmail)
    }

    func uploadReport(
        _ id: AccountId, bundle: ReportBundle
    ) async throws(CloudError) -> ReportReceipt {
        try await script.uploadReport(id, bundle: bundle)
    }
}
