import AmuxValues
import Foundation
import Observation

/// One pairing, from the code or the link to a trusted machine.
///
/// Pairing is two steps: the secret reaches and authenticates a machine,
/// which answers with its name and fingerprint, and only then does the
/// person decide. Every refusal reads the same — a wrong code, an expired
/// one and a machine that did not answer must not be told apart, or the
/// screen would help somebody guess.
@MainActor
@Observable
public final class PairingStore {
    public enum Phase: Equatable, Sendable {
        case entering
        case checking
        case confirming(PendingPair)
        case trusted(String)
        case refused
        /// The machine is only reachable through the relay, which this
        /// account does not buy.
        case needsSubscription
    }

    public static let codeLength = 6
    /// How long a machine holds a code open, as its person is told.
    public static let offerWindow = "5 minutes"

    public private(set) var digits = ""
    public private(set) var phase = Phase.entering
    /// The machine the code is for, where the person chose one.
    public private(set) var machine: HostView?
    /// Why the last attempt was refused, for diagnostics: the screen says
    /// every refusal the same way, and a report or a driver needs the reason.
    @ObservationIgnored public private(set) var refusal: String?
    /// Counts attempts, so an answer to one the person has since left is
    /// dropped.
    @ObservationIgnored private var attempt = 0
    @ObservationIgnored private let clock: @MainActor () -> Date

    public init(clock: @escaping @MainActor () -> Date = { Date() }) {
        self.clock = clock
    }

    public var now: Date { clock() }

    /// Starts a fresh attempt, for a machine or for whatever the code reaches.
    public func open(machine: HostView? = nil) {
        self.machine = machine
        digits = ""
        phase = .entering
        attempt += 1
    }

    public var taking: Bool { phase == .entering || phase == .refused }

    public func enter(_ typed: String) {
        guard taking else { return }
        digits = String(typed.filter(\.isNumber).prefix(Self.codeLength))
        if !digits.isEmpty { phase = .entering }
    }

    /// The request the six digits make, once there are six.
    public var request: PairRequest? {
        guard digits.count == Self.codeLength else { return nil }
        return .pin(pin: digits, addrs: machine?.addrs ?? [], hostId: machine?.hostId)
    }

    /// Marks an attempt under way and answers its number.
    public func checking() -> Int {
        attempt += 1
        phase = .checking
        return attempt
    }

    /// What reaching the machine came to. `relayOnly` says the only way to
    /// the machine was the relay and this account does not buy it.
    public func reached(
        _ result: Result<PendingPair, RuntimeFailure>, attempt: Int, relayOnly: Bool = false
    ) {
        guard attempt == self.attempt else { return }
        switch result {
        case .success(let pending):
            phase = .confirming(pending)
        case .failure(let why):
            refusal = why.description
            digits = ""
            phase = relayOnly ? .needsSubscription : .refused
        }
    }

    /// What confirming came to: the name of the machine now trusted, or
    /// why it is not.
    public func paired(_ result: Result<String, RuntimeFailure>, attempt: Int) {
        guard attempt == self.attempt else { return }
        switch result {
        case .success(let name): phase = .trusted(name)
        case .failure(let why):
            refusal = why.description
            digits = ""
            phase = .refused
        }
    }

    /// The person turned the machine away; the keypad starts again.
    public func abandoned() {
        attempt += 1
        digits = ""
        phase = .entering
    }
}
