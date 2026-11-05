import AmuxValues
import Foundation
import Observation

/// One account offered across a pairing, either way round.
///
/// The relay joins only devices on the same account, so a phone paired
/// with a signed-in computer would otherwise sign in a second time before
/// it reached that computer from anywhere. Pairing stays the only trust
/// decision; an offer only adds a route, and each side's person says yes
/// with the account's address in front of them.
public enum SignInPrompt: Equatable, Sendable {
    /// A paired machine offers this phone its account.
    case offered(SignInOfferView)
    /// This phone is signed in and just paired a machine that is not: the
    /// person here may offer it the account.
    case offering(host: HostView, email: String)

    /// Which side of the offer this phone is.
    public var role: OfferRole {
        switch self {
        case .offered: .receiver
        case .offering: .offerer
        }
    }

    /// The machine on the other side.
    public var host: [UInt8] {
        switch self {
        case .offered(let offer): offer.hostId
        case .offering(let host, _): host.hostId
        }
    }
}

/// The offers of an account on one profile: those its paired machines made
/// it, and the one it may make a machine it just paired.
@MainActor
@Observable
public final class SignInOffers {
    /// Why an answer did not sign anybody in.
    public enum Failure: Equatable, Sendable {
        /// The machine that offered could not be reached when the person
        /// accepted; its offer stands while that machine keeps it.
        case unreachable(String)
        /// Anything else, in the runtime's words for a report; the screen
        /// says it its own way.
        case refused(String)
    }

    /// Every offer paired machines make this profile now, as the runtime
    /// last listed them.
    public private(set) var received: [SignInOfferView] = []
    /// The answer or the offer under way, by the machine it is for.
    public private(set) var answering: [UInt8]?
    /// Why the last answer to an offer made to this phone signed nobody in.
    public private(set) var failure: Failure?
    /// The machine whose offer `failure` is about.
    @ObservationIgnored private var failedFor: [UInt8]?
    /// Machines paired since this profile came on screen, which may be
    /// offered the account once they say they are signed out.
    @ObservationIgnored private var paired: [HostId] = []
    /// Machines whose offer was made or turned down here.
    private var settled: Set<HostId> = []
    /// Prompts already reported as shown, so a redraw counts nothing.
    @ObservationIgnored private var reported: Set<[UInt8]> = []

    public init() {}

    /// The offers the runtime lists now; one gone is missing from the list,
    /// and what was said about answering it goes with it.
    public func listed(_ offers: [SignInOfferView]) {
        received = offers
        if let failedFor, !offers.contains(where: { $0.hostId == failedFor }) {
            failure = nil
            self.failedFor = nil
        }
    }

    /// A machine was paired just now.
    public func paired(_ host: HostId) {
        if !paired.contains(host) { paired.append(host) }
    }

    /// What to ask the person now, if anything: an offer made to this
    /// phone first, then the offer this phone may make a machine it just
    /// paired, while this phone is signed in and that machine says it is
    /// not.
    public func prompt(hosts: HostsStore) -> SignInPrompt? {
        if let offer = received.first { return .offered(offer) }
        guard let account = hosts.account, account.binding == .signedIn, !account.email.isEmpty
        else { return nil }
        for id in paired where !settled.contains(id) {
            if let host = hosts.host(id), host.signedIn == false {
                return .offering(host: host, email: account.email)
            }
        }
        return nil
    }

    /// Whether `prompt` was reported shown before; marks it reported.
    public func firstShown(_ prompt: SignInPrompt) -> Bool {
        reported.insert(prompt.host).inserted
    }

    /// Marks an answer under way.
    public func answers(_ host: [UInt8]) {
        answering = host
        failure = nil
    }

    /// What answering an offer made to this phone came to.
    public func answered(_ outcome: Result<SignInOfferOutcome, RuntimeFailure>, from host: [UInt8]) {
        answering = nil
        failedFor = host
        switch outcome {
        case .success(.signedIn), .success(.declined):
            failure = nil
            failedFor = nil
        case .success(.unreachable):
            let name = received.first { $0.hostId == host }?.hostName ?? ""
            failure = .unreachable(name)
        case .failure(let why):
            failure = .refused(why.description)
        }
    }

    /// Offering a machine this phone's account went to the runtime. It is
    /// not asked about again either way: an offer that went stands on that
    /// machine until it answers, and one refused was refused because the
    /// phone signed out or unpaired it meanwhile, which the screens already
    /// say.
    public func offered(_ outcome: Result<Nothing, RuntimeFailure>, to host: HostId) {
        answering = nil
        settled.insert(host)
    }

    /// The person turned down offering a machine this phone's account.
    public func notOffering(_ host: HostId) {
        settled.insert(host)
    }

    /// The failure shown, put away.
    public func dismissFailure() {
        failure = nil
        failedFor = nil
    }
}
