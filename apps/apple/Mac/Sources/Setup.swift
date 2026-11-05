import AmuxCore
import AmuxValues
import Foundation
import Observation
import os

/// Setting up a profile, as the Mac app runs it: the daemon's steps, one
/// screen a step, each with Later.
///
/// The daemon keeps which steps are done or skipped, and the shared rules
/// (`Onboarding.next`, `OnboardingState.view`) say where a run opens, what
/// follows a step and how many are left; this model holds what a screen
/// has chosen so far and carries out what the person does. A step the
/// daemon sees happen elsewhere (a phone paired from the terminal, a
/// sign-in from the phone) moves the screen on by itself.
///
/// While the pairing step shows, the daemon holds one pairing window that
/// takes the PIN or the QR code's secret, renewed before it ends and closed
/// when the step is left. Pairing ends on "Keep it connected", the
/// reachability step, since that is where those questions make sense.
@MainActor
@Observable
final class Setup {
    /// How long a pairing window is asked for. Long, so the PIN does not
    /// change while somebody types it; the screen renews it before it ends.
    static let windowSeconds: UInt64 = 10 * 60
    /// How long before a window ends the screen renews it.
    private static let renewBefore: TimeInterval = 30

    /// The pairing window as the screen holds it.
    struct Pairing {
        var window: PairingWindowView?
        var openedAt = Date()
        var status: PairingStatusView?
        /// A fresh window was asked for: the old one closing is not an end.
        var renewing = false
        /// A window was seen open since the step showed. The first status
        /// can be the last window's outcome, a pairing made long ago.
        var seenOpen = false
        /// Why no window opened.
        var failed: String?

        var joining: Bool { status?.joining ?? false }
        var lockedOut: Bool { status.map { $0.lockedOut && !$0.active } ?? false }
    }

    /// A folder offered as a repository, ticked to keep.
    struct RepositoryRow: Identifiable, Equatable {
        let path: String
        var ticked: Bool
        var id: String { path }
    }

    @ObservationIgnored private unowned let app: AppModel
    let profile: Profile

    /// The steps as the daemon last served them.
    private(set) var state: OnboardingState?
    /// The step on screen; nil while setting up is not.
    private(set) var step: SetupStep?
    /// Opened from "Finish Setting Up": skipped steps are walked too.
    private(set) var finishing = false
    /// The step the analytics notice is shown on: the first one shown,
    /// when no client had shown it yet.
    private(set) var noticeOn: SetupStep?
    /// Setting up did not open and the notice was still to show: the main
    /// window carries it.
    private(set) var noticeOnMain = false

    private(set) var pairing = Pairing()
    /// The phone this run paired, for the screens after.
    private(set) var paired: PairedDevice?

    var startAtLogin = true
    var keepAwake = true

    /// The phone just paired, still signed out, to offer the account too.
    private(set) var also: PairedDevice?
    var alsoTicked = false
    private(set) var signingIn = false
    /// A paired machine's offer of its account is being accepted.
    private(set) var accepting = false
    /// The offers paired machines make this profile of their account.
    private(set) var offers: [SignInOfferView] = []
    @ObservationIgnored private var offerCounted: [UInt8]?

    var repositories: [RepositoryRow] = []
    private(set) var repositoriesLoaded = false

    var telemetry = true
    var autoReports = false

    /// The agents on this Mac when the last step opened: one that is not
    /// among them came from the phone.
    @ObservationIgnored private var agentsBefore: Set<AgentKey> = []
    /// Why the last thing the person asked for did not happen.
    private(set) var error: String?
    /// A confirmation is on its way.
    private(set) var busy = false
    /// Whether to open setting up was decided, on the first steps heard.
    @ObservationIgnored private var decided = false
    @ObservationIgnored private var renewal: Task<Void, Never>?
    /// The pairing window's calls to the daemon, each made after the one
    /// before has answered: two opens in flight together (a renewal and New
    /// Code) could land in either order, and the code shown would be the
    /// one the later landing cancelled.
    @ObservationIgnored private var pairingCalls: Task<Void, Never>?
    /// Counts what was asked of the window; only the latest ask's answer is
    /// shown, and an ask overtaken before its turn is not made.
    @ObservationIgnored private var pairingAsk = 0
    private static let log = Logger(subsystem: "sh.amux.desktop", category: "setup")

    init(app: AppModel, profile: Profile) {
        self.app = app
        self.profile = profile
    }

    /// How many steps the main window offers to finish.
    var toFinish: UInt32 { step == nil ? state?.view.toFinish ?? 0 : 0 }

    private func open(_ step: SetupStep) -> Bool { state?.open.contains(step) ?? false }

    // MARK: - Opening and walking

    /// The daemon's steps moved. The first time, setting up opens at the
    /// first step still needed; afterwards a step seen done elsewhere moves
    /// the screen on.
    func stepsMoved(_ now: OnboardingState) {
        state = now
        if let step {
            let done = !open(step)
            switch step {
            case .sign_in where done && !signingIn && !accepting: advance()
            case .first_agent where done: fleetMoved()
            default: break
            }
            return
        }
        guard !decided else { return }
        decided = true
        if let at = now.view.opensAt {
            begin(at, finishing: false)
        } else if now.view.notice {
            noticeOnMain = true
            answer(.noticeShown)
        }
    }

    /// "Finish Setting Up": the first step left, skipped ones included.
    func finishSettingUp() {
        guard let state, let next = Onboarding.next(in: state, after: nil, finishing: true) else {
            return
        }
        begin(next, finishing: true)
    }

    /// "Finish Setting Up" dismissed: what was skipped stays skipped.
    func dismissFinishing() {
        answer(.dismiss)
    }

    /// The notice on the main window was read.
    func noticeRead() {
        noticeOnMain = false
    }

    private func begin(_ at: SetupStep, finishing: Bool) {
        self.finishing = finishing
        noticeOn = state?.noticeShown == false ? at : nil
        if noticeOn != nil { answer(.noticeShown) }
        enter(at)
    }

    private func enter(_ step: SetupStep) {
        self.step = step
        error = nil
        busy = false
        answer(.shown(step))
        switch step {
        case .pair_phone:
            // The screen opens the window when it appears.
            break
        case .keep_reachable:
            // Both on until the person has answered; after that, what they
            // chose.
            let confirmed = !open(step)
            startAtLogin = !confirmed || app.settings?.startAtLogin == true
            keepAwake = !confirmed || app.settings?.keepAwake == true
        case .sign_in:
            also = paired.flatMap { phone in
                let host = app.hosts.first { $0.hostId == phone.hostId }
                return host?.signedIn == true ? nil : phone
            }
            alsoTicked = also != nil
            countOffer()
        case .repositories:
            if !repositoriesLoaded { suggest() }
        case .improve:
            telemetry = app.settings?.telemetry ?? true
            autoReports = app.settings?.autoReports ?? false
        case .first_agent:
            agentsBefore = Set(app.localAgents)
        }
    }

    /// What leaving the step on screen undoes.
    private func leave() {
        if step == .pair_phone { closePairing() }
    }

    /// On to the step after the one on screen, or the main window after
    /// the last.
    private func advance() {
        leave()
        // Pairing ends on the reachability questions, whatever the daemon
        // says of them: "Paired ✓" is where they make sense.
        let next: SetupStep? =
            if step == .pair_phone && paired != nil {
                .keep_reachable
            } else {
                state.flatMap { Onboarding.next(in: $0, after: step, finishing: finishing) }
            }
        if let next {
            enter(next)
        } else {
            home(nil)
        }
    }

    /// "Later": the step is skipped and the next one shows.
    func later() {
        guard let step, !busy else { return }
        if open(step) { answer(.skip(step)) }
        advance()
    }

    /// Setting up is over: the main window, with the agent the phone started
    /// selected when there is one.
    private func home(_ agent: AgentKey?) {
        step = nil
        noticeOn = nil
        if let agent {
            let name = app.agents.first { $0.agent == agent }?.name ?? ""
            app.show(agent, saying: name.isEmpty ? "There it is." : "There it is: \(name)")
        }
    }

    /// Closes what the screen holds open, as the profile closes or the app
    /// quits.
    func close() {
        leave()
    }

    /// Closes what the screen holds open and waits until the daemon has
    /// heard, as the app quits: a pairing window left open would let a
    /// phone pair until it timed out.
    func closeForQuit() async {
        guard step == .pair_phone else { return }
        closePairing()
        await pairingCalls?.value
    }

    private func answer(_ answer: OnboardingAnswer) {
        let profile = profile
        Task {
            if case .failure(let error) = await profile.answerOnboarding(answer) {
                Self.log.error("answer: \(error.description, privacy: .public)")
            }
        }
    }

    // MARK: - Confirming

    /// Applies the screen's choices, then confirms the step while it is
    /// still to do, and moves on once both are taken.
    private func confirm(_ change: SettingsChange) {
        guard let step, !busy else { return }
        busy = true
        error = nil
        let confirming = open(step)
        Task {
            if let failed = await app.apply(change) {
                busy = false
                error = failed
                return
            }
            if confirming {
                switch await profile.answerOnboarding(.done(step)) {
                case .success(let now): state = now
                case .failure(let failed):
                    busy = false
                    error = failed.description
                    return
                }
            }
            busy = false
            if self.step == step { advance() }
        }
    }

    /// Continue on "Keep it connected".
    func confirmReachable() {
        let canStart = app.settings?.startAtLogin != nil
        confirm(.with(startAtLogin: canStart ? startAtLogin : nil, keepAwake: keepAwake))
    }

    /// Continue on "Help improve amux".
    func confirmImprove() {
        confirm(.with(telemetry: telemetry, autoReports: autoReports))
    }

    // MARK: - Pairing

    /// The pairing screen is on screen: a window opens, or a fresh one
    /// replaces the last.
    func pairingShown() {
        guard step == .pair_phone, pairing.window == nil else { return }
        openPairing()
    }

    /// The pairing screen left the screen without the step changing (the
    /// window closed): the window closes with it.
    func pairingHidden() {
        if step == .pair_phone { closePairing() }
    }

    /// Makes `call` once every pairing call before it has answered.
    private func queuePairing(_ call: @escaping @MainActor () async -> Void) {
        let before = pairingCalls
        pairingCalls = Task { @MainActor in
            await before?.value
            await call()
        }
    }

    private func openPairing() {
        pairing.failed = nil
        pairingAsk += 1
        let ask = pairingAsk
        let profile = profile
        queuePairing { [weak self] in
            // Overtaken before its turn: the newer ask opens or closes it.
            guard let self, ask == self.pairingAsk else { return }
            let opened = await profile.openPairing(ttl: Self.windowSeconds)
            guard ask == self.pairingAsk else { return }
            // Left meanwhile: the window is not wanted.
            guard self.step == .pair_phone else {
                if case .success = opened { _ = await profile.closePairing() }
                return
            }
            self.opened(opened)
        }
    }

    private func opened(_ opened: Result<PairingWindowView, RuntimeFailure>) {
        switch opened {
        case .success(let window):
            pairing.window = window
            pairing.openedAt = Date()
            pairing.status = nil
            pairing.renewing = false
            scheduleRenewal()
        case .failure(let failed):
            pairing.failed = failed.description
            pairing.renewing = false
        }
    }

    private func closePairing() {
        renewal?.cancel()
        renewal = nil
        let held = pairing.window != nil
        pairing = Pairing()
        // Closed even when the window already ended with a pairing: the
        // watch on it stops too.
        if held || step == .pair_phone {
            // An open still on its way is not shown, and is closed here
            // once it has answered.
            pairingAsk += 1
            let profile = profile
            queuePairing { _ = await profile.closePairing() }
        }
    }

    /// Renews the window before it ends, unless a phone is joining it then.
    private func scheduleRenewal() {
        renewal?.cancel()
        guard let window = pairing.window else { return }
        let ttl = TimeInterval(window.ttlSeconds)
        let due = pairing.openedAt.addingTimeInterval(max(ttl - Self.renewBefore, ttl / 2))
        renewal = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(0, due.timeIntervalSinceNow)))
            while let self, !Task.isCancelled, self.pairing.joining {
                try? await Task.sleep(for: .seconds(1))
            }
            guard let self, !Task.isCancelled, self.step == .pair_phone else { return }
            self.pairing.renewing = true
            self.openPairing()
        }
    }

    /// Too many wrong codes closed the window: a new code.
    func newCode() {
        pairing.renewing = true
        openPairing()
    }

    /// The pairing window's status moved: a phone proving the code, then
    /// paired, which ends the step on "Paired ✓".
    func pairingMoved(_ status: PairingStatusView) {
        guard step == .pair_phone else { return }
        if status.active {
            pairing.seenOpen = true
        } else if !pairing.seenOpen {
            return
        }
        if !status.active, let phone = status.paired {
            paired = phone
            // The window closed with the pairing.
            renewal?.cancel()
            pairing = Pairing()
            advance()
            return
        }
        if pairing.renewing && !status.active { return }
        pairing.status = status
    }

    /// Renames this Mac, as every paired device will see it.
    func rename(_ name: String) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard name != app.settings?.hostName else { return }
        Task {
            if let failed = await app.apply(.with(hostName: name)) { error = failed }
        }
    }

    // MARK: - Signing in

    /// The paired machines' offers of their account moved.
    func offersMoved(_ offers: [SignInOfferView]) {
        self.offers = offers
        if step == .sign_in { countOffer() }
    }

    /// The offer on screen, counted once as shown; or, with none, the
    /// choice to offer the paired phone, counted the same way.
    private func countOffer() {
        let shown: ([UInt8], OfferRole)? =
            if let offer = offers.first {
                (offer.hostId, .receiver)
            } else if let also {
                (also.hostId, .offerer)
            } else {
                nil
            }
        guard let (key, role) = shown, offerCounted != key else { return }
        offerCounted = key
        answer(.signInOfferShown(role))
    }

    /// Sign In: a paired machine's offer when it made one, else the
    /// browser.
    func signIn() {
        guard !signingIn, !accepting else { return }
        error = nil
        if let offer = offers.first {
            accept(offer)
            return
        }
        signingIn = true
        Task {
            let outcome = await app.signIn()
            signingIn = false
            switch outcome {
            case .signedIn: signedIn()
            case .cancelled: break
            case .failed(let why): error = why
            }
        }
    }

    /// Signed in: the phone ticked is offered the account, the offer page
    /// opens in the browser, and the screen moves on.
    private func signedIn() {
        if alsoTicked, let also, let host = HostId(bytes: also.hostId) {
            let profile = profile
            Task {
                if case .failure(let failed) = await profile.offerSignIn(to: host) {
                    Self.log.error("offer: \(failed.description, privacy: .public)")
                }
            }
        }
        openOfferPage()
        if step == .sign_in { advance() }
    }

    /// The account service's page after signing in: the trial, the plan.
    private func openOfferPage() {
        let id = profile.id
        Task { await app.openAccountPage("/offer", for: id) }
    }

    private func accept(_ offer: SignInOfferView) {
        accepting = true
        Task {
            let answered = await profile.answerSignInOffer(from: offer.hostId, accept: true)
            accepting = false
            switch answered {
            case .success(.signedIn):
                openOfferPage()
                if step == .sign_in { advance() }
            case .success(.declined):
                offers.removeAll { $0.hostId == offer.hostId }
            case .success(.unreachable):
                error = "\(offer.hostName) is offline. Open amux there, then try again."
            case .failure(let failed):
                error = failed.description
            }
        }
    }

    /// Not Now on a paired machine's offer: the browser sign-in is offered
    /// instead.
    func declineOffer() {
        guard let offer = offers.first, !accepting else { return }
        offers.removeFirst()
        let profile = profile
        Task { _ = await profile.answerSignInOffer(from: offer.hostId, accept: false) }
    }

    // MARK: - Repositories

    private func suggest() {
        guard let runtime = app.runtime else { return }
        Task {
            let suggested = (try? await runtime.suggestRepositories().get()) ?? []
            repositoriesLoaded = true
            // Repositories already chosen stay; they are not asked about again.
            let chosen = Set(app.settings?.repositories ?? [])
            repositories = suggested.filter { !chosen.contains($0.path) }
                .map { RepositoryRow(path: $0.path, ticked: true) }
        }
    }

    /// A folder the person picked, ticked.
    func add(_ folder: URL) {
        let path = folder.standardizedFileURL.path
        if let at = repositories.firstIndex(where: { $0.path == path }) {
            repositories[at].ticked = true
        } else {
            repositories.append(RepositoryRow(path: path, ticked: true))
        }
    }

    /// Continue on "Repositories": the ticked folders join those already
    /// chosen.
    func confirmRepositories() {
        var paths = app.settings?.repositories ?? []
        for row in repositories where row.ticked && !paths.contains(row.path) {
            paths.append(row.path)
        }
        confirm(.with(repositories: paths))
    }

    // MARK: - The first agent

    /// An agent new on this Mac while the last step shows is the one the
    /// phone started: the main window, with it selected.
    func fleetMoved() {
        guard step == .first_agent else { return }
        if let new = app.localAgents.first(where: { !agentsBefore.contains($0) }) {
            home(new)
        }
    }
}

extension SettingsChange {
    /// A change to only the settings named.
    static func with(
        hostName: String? = nil, startAtLogin: Bool? = nil, keepAwake: Bool? = nil,
        repositories: [String]? = nil, telemetry: Bool? = nil, autoReports: Bool? = nil
    ) -> SettingsChange {
        SettingsChange(
            autoReports: autoReports, hostName: hostName, keepAwake: keepAwake,
            repositories: repositories, startAtLogin: startAtLogin, telemetry: telemetry)
    }
}
