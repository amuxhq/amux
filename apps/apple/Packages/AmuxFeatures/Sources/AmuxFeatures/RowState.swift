import AmuxCore
import AmuxDesign
import SwiftUI

/// Everything a fleet row can say about itself, in one closed set.
///
/// It exists because the answer used to be spread across four places that
/// nothing reconciled: the agent's attention, whether this build can read its
/// provider, whether the machine that owns it has answered, and whether that
/// machine is online at all. The list row and the sentence VoiceOver reads
/// each recombined a different subset by hand, so no single
/// place said what a row could be — which is how two marks in the vocabulary
/// came to mean nothing without anybody noticing.
///
/// The marks are gone. A row draws one thing, the accent dot beside its age,
/// for the one state that is waiting on a person; everything else is said in
/// words on the third line, where `Idle` and `Finished · 4 files · +118 −40`
/// already were.
/// A word is more precise than a glyph and needs no vocabulary learnt first.
enum RowState: Equatable {
    /// Stopped and cannot continue without you. The only case with a mark.
    case needsYou
    case working
    case starting
    /// The machine that owns this agent is not answering.
    case hostOffline(String)
    /// Seen by the relay, and not reachable from here: the row is what this
    /// phone last held, and it stays on the list.
    case hostAway(String)
    /// A kind this build has no chat for.
    case unsupported
    /// The agent's process ended, and how.
    case exited(ExitCause)
    case idle

    /// Read once, in one order, so the order is a thing somebody can look at.
    ///
    /// Being unreadable comes first because it is a fact about this build
    /// rather than about the agent, and it outranks anything the agent might
    /// be doing — none of which can be acted on from here anyway. How an
    /// agent ended comes next, as the runtime's second line has it: an exited
    /// agent is history, whatever its machine is doing now. A dark machine
    /// comes next, and above `needsYou` deliberately: what an agent last
    /// asked for on a machine nobody can reach cannot be answered.
    init(row: AgentRow) {
        if !row.readable {
            self = .unsupported
            return
        }
        if row.attention == .exited {
            self = .exited(row.card.exitCause ?? .ended)
            return
        }
        let machine = PlaceNames.host(row.hostName)
        switch row.hostReach {
        case .online: break
        case .away:
            self = .hostAway(machine)
            return
        case .offline:
            self = .hostOffline(machine)
            return
        }
        switch row.attention {
        case .needsYou: self = .needsYou
        case .working: self = .working
        case .starting: self = .starting
        case .exited: self = .exited(row.card.exitCause ?? .ended)
        case .idle: self = .idle
        }
    }

    /// The word the third line opens with. A row that needs you says so in
    /// the accent: the inventory carries the phase, not what the ask is
    /// about. A one-shot agent that ended after its turn finished rather
    /// than exited.
    var word: String? {
        switch self {
        case .needsYou: "Needs you"
        case .working: "Working"
        case .starting: "Starting"
        case .hostOffline(let machine): "\(machine) offline"
        case .hostAway(let machine): "\(machine) away"
        case .unsupported: "Unknown agent"
        case .exited(.finished): "Finished"
        case .exited: "Exited"
        case .idle: "Idle"
        }
    }

    /// What follows the word, where it says more than the place. Why an
    /// agent exited is the row's second line, not this.
    var elaboration: String? {
        switch self {
        case .unsupported: "update amux to open it"
        case .hostAway: "not live"
        default: nil
        }
    }

    /// Whether the word already names the machine.
    var namesTheHost: Bool {
        switch self {
        case .hostOffline, .hostAway: true
        default: false
        }
    }

    var needsYou: Bool { self == .needsYou }

    /// The same, as VoiceOver says it.
    var spoken: String? {
        switch self {
        case .needsYou: "Needs you"
        case .working: "Working"
        case .starting: "Starting"
        case .hostOffline(let machine): "\(machine) is offline"
        case .hostAway(let machine): "\(machine) is away, not live"
        case .unsupported: "Unknown agent, update amux to open it"
        case .exited(.finished): "Finished"
        case .exited: "Exited"
        case .idle: "Idle"
        }
    }

    /// What a driver reads the row's state as.
    var name: String {
        switch self {
        case .needsYou: "needs-you"
        case .working: "working"
        case .starting: "starting"
        case .hostOffline: "host-offline"
        case .hostAway: "host-away"
        case .unsupported: "unsupported"
        case .exited: "exited"
        case .idle: "idle"
        }
    }
}
