import AmuxCore
import AmuxDesign
import Foundation
import SwiftUI

/// What a Mac-side driver can ask a debug build to do, and what it answers.
///
/// The wire is one JSON object per line over loopback. Every case is tagged by
/// a `kind` string with its fields alongside rather than nested, so a request
/// reads the same in a Swift test, in a Rust client and in a transcript
/// somebody is trying to understand after a failure.
public enum DoorRequest: Sendable, Equatable {
    /// Show a named screen, filled from a named state. Absent means the state
    /// whose name matches the screen.
    case open(screen: String, fixture: String?)
    /// Rewrite what the account service will answer from here on.
    case cloud(CloudScript)
    /// Rewrite what the App Store will answer from here on. A purchase sheet
    /// belongs to another process and cannot be pressed from a test, so what
    /// it comes to is said here and the screen is driven by a finger as
    /// usual.
    case store(StoreScript)
    /// Everything the scripted cloud and the scripted store were asked, in
    /// order. What a screen did behind the words on it — that it asked the
    /// account service what this account may do rather than believing the
    /// purchase — is only readable here.
    case calls
    /// The accounts this phone knows, as the registry every screen reads has
    /// them, and how many answers it has refused for an account no longer on
    /// screen.
    case accounts
    /// Sign another account in on this phone, with a relay credential of its
    /// own, and give it a profile of its own.
    ///
    /// The runtime takes its accounts when it starts, so this restarts it with
    /// every account this phone has been given and leaves the same one on
    /// screen. Which account is read is then the switcher's to change.
    case addAccount(user: String, token: String)
    /// Ask the link to read again what the account on screen may do.
    ///
    /// What a purchase leaves behind, without the purchase. A subscription is
    /// bought at the App Store, whose sheet belongs to another process and
    /// cannot be pressed from here; what the app does the moment one goes
    /// through is tell its own link to re-authenticate, because the relay
    /// reads the tier off the credential the link holds and nothing on this
    /// device knows the money moved until somebody asks. That is the half a
    /// driver can perform, and it is the half a machine's reachability turns
    /// on.
    case refreshEntitlement
    /// Deliver again, under the name of the account it answered for, the last
    /// thing that account's connection produced.
    ///
    /// A result that arrives after somebody has switched away cannot be asked
    /// for: it is a race with a network. This is that result, played at the
    /// moment the race would have lost — the same batch, from the same
    /// connection, still answering for the account it was about.
    case late(account: String)
    /// Start the shared runtime against a relay with a credential, and
    /// what that credential buys.
    ///
    /// The tier is said because nothing below can read one out of an
    /// opaque bearer: the account service issues it beside a real
    /// credential, and a driver handing one over stands in for that.
    /// Paid unless a journey is about an account that has not paid.
    case connect(relay: String, token: String, user: String, tier: Tier)
    /// Put an account on this phone from a session somebody signed in
    /// elsewhere, and reach whatever the account service says that account may
    /// reach.
    ///
    /// The other way in. `connect` is handed a relay and a credential and
    /// believes both, which is what a test relay on this Mac needs. This is
    /// handed only a refresh token — the one thing a browser sign-in leaves
    /// behind — and everything after it is the app's own production path: the
    /// account service says who the account is and what it may do, mints the
    /// relay credential, and names the relay to dial. Nothing here decides
    /// where to go or what the account is entitled to, so a run of this
    /// exercises the credential the relay will really validate rather than one
    /// a harness minted.
    case restoreSession(account: String, refresh: String)
    /// Wait until the fleet has been confirmed by a host, or give up after
    /// this many seconds. A connection is asynchronous, so a driver that read
    /// the screen straight after `connect` would read the moment before it.
    case awaitReconciled(seconds: Double)
    /// Wait until the connection has reported itself gone, or give up after
    /// this many seconds. A relay that has stopped answering is discovered by
    /// a connection failing, not by anything the driver did, so there is a
    /// moment to wait for here too.
    case awaitOffline(seconds: Double)
    /// Wait until the app is in the background and has told its runtime so,
    /// or give up after this many seconds. Putting the app away is the
    /// simulator's to do and takes a moment; what the runtime keeps current
    /// changes only once it has.
    case awaitBackground(seconds: Double)
    /// What library this app linked and what its connection has arrived at.
    case bridge
    /// The end of what this launch's runtime wrote about what it decided.
    /// Only a build with the driving tools writes it at all.
    case runtimeLog(bytes: Int)
    /// Read the production conversation's projection, including its native layer.
    case conversation(agent: String)
    /// Attempt the same typed model change as the settings sheet, including
    /// where the shared core refuses it and the screen disables the control.
    case setModel(agent: String, name: String)
    /// Every moment this launch has marked, in order. A driver reads them to
    /// tell a screen that was drawn from a frame that was shown, and to see
    /// when the fleet stopped being a memory.
    /// Watch the display, the main thread and the footprint for this long
    /// and answer what they did; the driver runs the workload meanwhile.
    case measure(seconds: Double)
    case signposts
    case appearance(Appearance)
    case dynamicType(String)
    /// Draw as the system does for a reader who has asked for less motion or
    /// less transparency.
    ///
    /// Both are system-wide settings a driver cannot change from inside a
    /// running app, and both change what a screen looks like rather than what
    /// it says — so a state that wants to be photographed with them on has to
    /// declare them, exactly as it declares a text size.
    case assist(motion: Bool, transparency: Bool)
    /// Whether every identified element reports where it is drawn. A driver
    /// needs the frames to tap; a measurement does not, and producing them
    /// is a real share of a launch's main thread, so a benchmark launches
    /// without them and turns them on only for as long as it taps.
    case geometry(on: Bool)
    /// Every state this build draws, as screen-and-state pairs. What lets a
    /// sweep over the whole app — the accessibility audit is one — cover
    /// whatever has been built today without a second list of it to keep in
    /// step.
    case states
    /// Move one named colour token, or put it back when nothing is named.
    /// The one thing a driver can ask for that makes the app draw something
    /// its baseline does not show.
    case perturb(token: String?)
    /// Select one of the Debug-only layouts compiled together for a design round.
    case designVariant(String?)
    /// Wait until the screen has stopped changing. A capture that does not
    /// wait for this photographs a frame mid-animation.
    case settle
    case query
    /// Write a PNG of the composited window at the given path, which is the
    /// app's own to write — a driver on the Mac reads it back out of the
    /// app's container.
    case capture(path: String)
    case tap(identifier: String)
    /// Activate the item reading `label` in what the system presented over
    /// the app, a dialog or a menu, whose items carry no names of their own.
    case choose(label: String)
    /// Run an element's named accessibility action, as VoiceOver's actions
    /// rotor does: what a drag or a hold does for a person with a finger.
    case perform(identifier: String, action: String)
    case type(identifier: String, text: String)
    /// Make the search field of the screen on top hold `text`, as a person
    /// does by pulling it into view, clearing it and typing. The system's
    /// search field takes no name from the screen that asks for it, so it
    /// is found as the one search field on show.
    case search(text: String)
    /// Empty a named field, through the field's own delete.
    ///
    /// Typing adds to what a field is already holding, which is what typing
    /// does; a driver that wants the field to hold one thing has to take the
    /// last thing out of it first, exactly as the person whose typo it was
    /// would.
    case clear(identifier: String)
    /// Move the scrolling list under the middle of the screen one page `up`
    /// (towards older rows) or `down`, or to its `top` or `bottom`, as a
    /// person's swipe does. Named, the list moved is the one the element is
    /// drawn in, in sight or scrolled out of it: a swipe on a card's own list
    /// rather than the screen's.
    case scroll(direction: String, identifier: String? = nil)
    /// Put this text on the system's clipboard and paste it into the named
    /// field, which is the message the system's own Paste menu item sends.
    ///
    /// The menu itself is the system's, drawn outside this app and reachable
    /// only by hitting a floating bar nobody can name; the paste it performs
    /// is the field's own, and that is what this is.
    case paste(identifier: String, text: String)
    /// Pick the character at `from` up and put it down before what is at
    /// `to`, through the named field's own draft.
    ///
    /// This is how a token moves, because in the sentence a token is one
    /// character. A finger does it by dragging the chip, which is a text
    /// view's own drag interaction — the system starts it from a long press
    /// on drawn text and no accessibility client can synthesize one, so a
    /// driver reaches the same edit the drag would make.
    case move(identifier: String, from: Int, to: Int)
    /// Store an attachment for an agent as though a picker had just handed it
    /// back: the kind, the name, the type and the bytes, base64 for the wire.
    ///
    /// The photo library and the file browser are the system's own screens,
    /// running outside this app. What they hand back is all this app ever
    /// sees of them, and it goes from here into the same code the pickers
    /// call — so the token that appears at the caret appears because a host
    /// stored the bytes, exactly as it would after a real pick.
    case attach(agent: String, kind: String, name: String, mime: String, base64: String)
    /// Trust the host a pairing payload names, over the relay this app is
    /// already connected to.
    ///
    /// A person pairs a phone by reading a code off a machine, on a screen
    /// that names the host and its fingerprint before anything is written.
    /// This takes both of the steps that screen takes — authenticate the
    /// payload against the machine, then write trust against the attempt the
    /// machine answered with — and skips only the person in the middle of
    /// them. A driver proving what a paired phone shows gets the trust the
    /// product's own path writes, not a shortcut around it.
    ///
    /// Either the link a machine's QR encodes or the offer inside it: a
    /// driver pointed at a real machine has the former and nothing else.
    case pair(qr: String)
    /// Pair with one machine by the six-digit code it printed, over the relay
    /// this app is already connected to.
    ///
    /// The same two steps and the same store the pairing screen uses, driven
    /// from the machine the relay offered rather than from a tap: a code
    /// proves possession of one machine's offer, so the machine is found among
    /// the ones the relay is offering before its code is tried against it.
    case pairByCode(host: String, pin: String)
    /// Say what the system answered when this app asked to look at the network.
    ///
    /// iOS asks once. A refusal cannot be provoked a second time, and a
    /// simulator grants it silently, so the one state a person can be left in
    /// — refused, with no way back inside the app — is unreachable by driving
    /// the app alone.
    case localNetwork(permission: String)
    /// Withdraw the key this phone holds for one machine, through the same
    /// store the paired devices sheet drives.
    ///
    /// The other half of `pairByCode`. A driver that re-enters a journey
    /// partway through has to leave behind what the acts before it left
    /// behind, and one of the things a person does on the way through is stop
    /// trusting a machine. What is skipped is the sheet and the press; the
    /// withdrawal itself is the app's own.
    case revoke(host: String)
    /// Ask the host holding an agent for the changes its working tree has
    /// against a base — a branch or a commit, or the working tree itself when
    /// the base is empty. The host computes the diff; the phone draws it.
    case requestChanges(agent: String, base: String)
    /// Wait until the fleet names this agent, or give up after this many
    /// seconds.
    ///
    /// A machine that has just admitted this phone has not finished saying
    /// what it is running: trust is written, and the account of the machine
    /// follows it over the same link. A driver that opened a conversation in
    /// that gap would be telling the runtime to watch an agent it has never
    /// heard of, and nothing would arrive.
    case awaitAgent(agent: String, seconds: Double)
    /// Open an agent's conversation without going to it, which is what tells
    /// the runtime this client is watching that agent.
    ///
    /// Opening one by pressing a row is a tap, and a tap takes long enough
    /// that the layer behind it has already caught up by the time the next
    /// thing happens. A driver asking what a conversation does in the moment
    /// it opens has to be there in that moment.
    case watch(agent: String)
    /// Wait until an agent's conversation will take a message, or give up.
    ///
    /// A machine that has just come back has not finished saying so: the
    /// connection returns before the layer behind it does. A driver that sent
    /// straight after a reconnection would be asking about the gap rather than
    /// about the send.
    case awaitSendable(agent: String, seconds: Double)
    /// Wait until an agent has answered with something that says this, or give
    /// up after this many seconds.
    ///
    /// A driver that read the transcript straight after sending would read the
    /// message it had just sent. Waiting on the words rather than on a count
    /// is what lets a driver wait for a real agent, which takes as long as it
    /// takes and says a great many things on the way; the words must be ones
    /// the message did not already carry, or the send's own row answers it.
    case awaitReply(agent: String, saying: String, seconds: Double)
    /// Try to send a message to an agent, exactly as pressing send will.
    ///
    /// The answer says whether it left the phone. A refusal is not an error:
    /// refusing to send is a thing this app does deliberately, and a driver
    /// asks for it on purpose.
    case send(agent: String, text: String)
    /// Say something beside the review a conversation is already holding, and
    /// send the two together.
    ///
    /// The composer is not built, so there is nowhere on screen to write the
    /// remark that goes with a review. This puts the words where the composer
    /// will keep them and sends the draft the conversation holds — the same
    /// message, through the same gate, with the patch attached.
    case sendDraft(agent: String, prose: String)
    /// Write the report this app would send into this directory: the frozen
    /// picture of the screen, the shared runtime's own recording, the
    /// view-state trace, the daemon's state and `report.json` declaring them,
    /// with the note and the rectangles carried on it.
    case report(path: String, note: String, marks: [ReportMark])
    /// Tell the app the system has just taken a screenshot of it, the way iOS
    /// tells it.
    ///
    /// The notification is the whole of what an app is given: iOS takes and
    /// saves its own picture first and no app can intercept the gesture. A
    /// driver cannot make the gesture happen either — a host-side capture
    /// posts nothing inside the app, and the simulator's own shortcut belongs
    /// to a window nobody is at during a test run — so this posts the same
    /// notification the system posts, and everything the app does about a
    /// screenshot happens from here on exactly as it would.
    ///
    /// Which preview the phone is set to show afterwards is not said, because
    /// the app is never told: full screen and thumbnail differ only in what
    /// covers the app next, which is the driver's to stage.
    case screenshot
    /// Write the last report the scripted account service was handed into this
    /// directory, and answer the files it left.
    ///
    /// What a person's Send actually uploaded, read at the boundary it left
    /// the app through. Not the same as writing a fresh bundle: this is the
    /// one that was sent, declarations and all.
    case uploaded(path: String)
    /// Hold the app reachable in the background from here until a push is
    /// handed over, or until the system's allowance for finishing work runs
    /// out.
    ///
    /// A simulator hands a remote notification's payload to no app put away:
    /// the notification shows, and the app's handler is never called. A
    /// suspended app cannot be driven, so the app asks for the same time a
    /// real app gets to finish a piece of work before it is put away, and
    /// the driver has a background app to hand the push to.
    case holdBackground
    /// Hand the payload in this file to the app's remote-notification handler,
    /// as iOS would to an app woken in the background, and answer once the
    /// handler has finished. Refused unless the app is in the background.
    case push(path: String)
    case shutdown
}

public enum DoorReply: Sendable, Equatable {
    case ack
    case state(VisibleState)
    case bridge(BridgeState)
    /// What the runtime wrote, ending at the moment this was asked, and
    /// empty in a build that writes none.
    case runtimeLog(String)
    case conversation(ConversationReading)
    case signposts([SignpostMark])
    case measurement(Measurement)
    case captured(path: String, width: Int, height: Int, scale: Int)
    /// A bundle was written at this path, holding these files.
    case bundle(path: String, parts: [String], reportJSON: String? = nil)
    /// This phone now trusts the machine of this name.
    case paired(host: String)
    /// What the scripted cloud and the scripted store were asked, in order.
    case calls(cloud: [String], store: [String])
    /// The accounts this phone knows.
    case accounts(AccountsState)
    /// Every state this build draws: the screen to open and the state to fill
    /// it from, in the order the catalogue describes them.
    case states([DrawnState])
    /// What became of an attempted send: whether it left the phone, and the
    /// sentence on screen when it did not.
    case sendAttempt(delivered: Bool, reason: String?)
    /// Why the request could not be answered, in one line. The door never
    /// half-answers: a request either happened or is reported here.
    case error(String)
}

/// One state this build draws: the screen to open, and the state to fill it
/// from.
public struct DrawnState: Codable, Sendable, Equatable {
    public let screen: String
    public let state: String
    /// The text size this state is drawn at, in the door's own words, where it
    /// names one.
    public let typeSize: String?

    public init(screen: String, state: String, typeSize: String?) {
        self.screen = screen
        self.state = state
        self.typeSize = typeSize
    }
}

/// The accounts this phone knows, as the registry the screens read has them.
///
/// A driver could read most of this off the screen, and where it can it
/// should. What it cannot read anywhere is the last line: a result refused
/// because it answered for an account nobody is looking at leaves nothing on
/// screen, which is the whole point of refusing it.
public struct AccountsState: Codable, Sendable, Equatable {
    public let selected: String?
    public let accounts: [Known]
    /// Answers refused because they were about an account that is no longer on
    /// screen.
    public let dropped: Int
    /// Accounts removed from this phone that a runtime has since started
    /// without, deleting their profiles and caches. Nothing on screen says it.
    public let deletedProfiles: [String]

    public init(
        selected: String?, accounts: [Known], dropped: Int, deletedProfiles: [String] = []
    ) {
        self.selected = selected
        self.accounts = accounts
        self.dropped = dropped
        self.deletedProfiles = deletedProfiles
    }

    /// One account, as its row is drawn from.
    public struct Known: Codable, Sendable, Equatable {
        public let id: String
        public let email: String
        public let signedIn: Bool
        /// What the relay will carry for this account and where that was
        /// bought, in the words the You page's relay row says it: *Subscribed
        /// through the App Store*, *Included with this account*, *Not
        /// subscribed · hosts on this network still work*.
        public let entitlement: String
        /// How many machines this account reached, where a connection has
        /// counted them.
        public let hosts: Int?
        /// How many agents are waiting on it, where something has reported it.
        public let attention: Int?

        public init(
            id: String, email: String, signedIn: Bool, entitlement: String,
            hosts: Int?, attention: Int?
        ) {
            self.id = id
            self.email = email
            self.signedIn = signedIn
            self.entitlement = entitlement
            self.hosts = hosts
            self.attention = attention
        }
    }
}

/// What is on screen, as the accessibility tree reports it: the same elements
/// a journey drives and a person using VoiceOver hears, depth-first.
public struct VisibleState: Codable, Sendable, Equatable {
    /// The screen the door was last asked to open, or `none`.
    public let screen: String
    /// The reader's type size this screen is drawn at, in the door's own
    /// spelling.
    ///
    /// Reported rather than assumed, because a state that names an
    /// accessibility size sets one and every state after it would keep it if
    /// opening a screen ever stopped putting it back. Nothing about the
    /// picture says which size it was taken at, so a run that leaked one
    /// would produce baselines nobody could tell apart from correct ones.
    public let typeSize: String
    /// Whether this launch is running under VoiceOver.
    ///
    /// Reported for the same reason the type size is. VoiceOver is a
    /// device-wide setting nobody inside the app can turn on, so a driver that
    /// turned it on outside has no way of knowing it took: the app is the only
    /// thing that can say whether it is in a VoiceOver session, and a run that
    /// claimed to be one without asking would be claiming it of the plist it
    /// wrote rather than of the app it drove.
    public let voiceOver: Bool
    public let elements: [VisibleElement]
    /// Whether the fleet on screen has been confirmed by a host.
    public let reconciled: Bool
    /// How many rows are still drawn as unconfirmed.
    public let unconfirmed: Int

    public init(
        screen: String, typeSize: String, voiceOver: Bool = false,
        elements: [VisibleElement], reconciled: Bool, unconfirmed: Int
    ) {
        self.screen = screen
        self.typeSize = typeSize
        self.voiceOver = voiceOver
        self.elements = elements
        self.reconciled = reconciled
        self.unconfirmed = unconfirmed
    }
}

/// What the shared runtime under this app is and what it has reached.
///
/// The build is the marker the linked library answers with, so a driver can
/// tell the shipping library from the one with the driving tools compiled in
/// without guessing from behaviour. The rest is what a connection actually
/// produced: an acknowledged `connect` only means the runtime started, and
/// hosts and agents named here mean it reached the other end.
public struct BridgeState: Codable, Sendable, Equatable {
    public let build: String
    /// Whether a connection has been started at all.
    public let started: Bool
    /// Startup or terminal worker diagnostic, kept separate from visible copy.
    public let failure: String?
    /// The connection's own word for where it is.
    public let connection: String
    /// Whether the fleet has been confirmed by a host rather than remembered.
    public let reconciled: Bool
    /// How many confirmed fleets have arrived since this app started.
    ///
    /// The flag above never goes back to false, so it cannot say whether the
    /// app reconciled again after an outage or after being picked up: it was
    /// already true. This has to move for that to have happened, so it is what
    /// a driver times a recovery against.
    public let reconciliations: Int
    /// The machines the fleet names, by name. A machine appears here once this
    /// device is paired with it; before that the fleet is confirmed and empty.
    public let hosts: [String]
    /// The agents the fleet names, by name.
    public let agents: [String]
    /// How many times this phone has dialled the relay since it started.
    ///
    /// It is the only place a dial at a relay that is not there leaves a
    /// trace: nothing on the far side can count a connection that never
    /// arrived.
    public let relayAttempts: UInt64
    /// How many of those dials happened early because somebody asked.
    ///
    /// This is what a driver pressing Retry Now reads, rather than the count
    /// above: the connection dials on its own schedule anyway, so an attempt
    /// alone cannot tell a press that reached the runtime from the backoff
    /// coming round. Only a press moves this.
    public let relayRetries: UInt64
    /// The machines the connection has seen on the other side, by name,
    /// whether or not this device is paired with them. Where the fleet is
    /// what the user may open, this is what the runtime found — the one thing
    /// that tells a connection which reached a host from one which only
    /// started.
    public let discovered: [String]
    /// The agents this phone is holding a stream for, by identifier.
    ///
    /// A stream exists because a conversation asked for it, so a conversation
    /// somebody closed should not be here — including after an outage, where
    /// a phone that reopened everything it once watched would be reading a
    /// machine on behalf of nobody.
    public let watching: [String]
    /// The conversations this app has asked the runtime to stop streaming, in
    /// the order it asked. Where `watching` is the runtime's own account,
    /// this is the app's: together they say whether a stream still open is
    /// one nobody let go of or one the runtime kept anyway.
    public let releasedStreams: [String]
    /// Why each machine this device knows of reads where it does: the carrier
    /// its link is riding, whether it is answering, and the last reason a dial
    /// at it failed. One line per machine, by name.
    ///
    /// A screen says a machine is away or offline without saying why, and the
    /// reason is never on the machine's own side: a dial that never landed
    /// leaves no trace there. So it is read off the device that tried.
    public let reach: [String]

    public init(
        build: String, started: Bool, connection: String, reconciled: Bool,
        reconciliations: Int = 0, hosts: [String], agents: [String], relayAttempts: UInt64,
        relayRetries: UInt64, discovered: [String], watching: [String] = [],
        releasedStreams: [String] = [], reach: [String] = [], failure: String? = nil
    ) {
        self.build = build
        self.started = started
        self.failure = failure
        self.connection = connection
        self.reconciled = reconciled
        self.reconciliations = reconciliations
        self.hosts = hosts
        self.agents = agents
        self.relayAttempts = relayAttempts
        self.relayRetries = relayRetries
        self.discovered = discovered
        self.watching = watching
        self.releasedStreams = releasedStreams
        self.reach = reach
    }
}

public struct VisibleElement: Codable, Sendable, Equatable {
    public let identifier: String
    public let label: String?
    public let value: String?
    public let frame: VisibleFrame
    public let enabled: Bool

    public init(
        identifier: String, label: String?, value: String?, frame: VisibleFrame, enabled: Bool
    ) {
        self.identifier = identifier
        self.label = label
        self.value = value
        self.frame = frame
        self.enabled = enabled
    }
}

/// A rectangle in points, in the window's coordinates.
public struct VisibleFrame: Codable, Sendable, Equatable {
    public let x: Double
    public let y: Double
    public let width: Double
    public let height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }
}

// MARK: - The wire

extension DoorRequest: Codable {
    private enum Key: String, CodingKey {
        case kind, screen, fixture, cloud, store, relay, token, user, appearance, size, path
        case account, refresh, saying
        case identifier, text, seconds, qr, agent, base, prose, from, to
        case attachment, name, mime, base64, host, pin
        case note, marks
        case motion, transparency, on
        case permission, tier
        case bytes, label, action, direction
    }

    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: Key.self)
        let kind = try fields.decode(String.self, forKey: .kind)
        switch kind {
        case "open":
            self = .open(
                screen: try fields.decode(String.self, forKey: .screen),
                fixture: try fields.decodeIfPresent(String.self, forKey: .fixture))
        case "cloud":
            self = .cloud(try fields.decode(CloudScript.self, forKey: .cloud))
        case "store":
            self = .store(try fields.decode(StoreScript.self, forKey: .store))
        case "calls": self = .calls
        case "accounts": self = .accounts
        case "addAccount":
            self = .addAccount(
                user: try fields.decode(String.self, forKey: .user),
                token: try fields.decode(String.self, forKey: .token))
        case "refreshEntitlement": self = .refreshEntitlement
        case "late":
            self = .late(account: try fields.decode(String.self, forKey: .account))
        case "connect":
            self = .connect(
                relay: try fields.decode(String.self, forKey: .relay),
                token: try fields.decode(String.self, forKey: .token),
                user: try fields.decode(String.self, forKey: .user),
                tier: try fields.decodeIfPresent(Tier.self, forKey: .tier) ?? .pro)
        case "restoreSession":
            self = .restoreSession(
                account: try fields.decode(String.self, forKey: .account),
                refresh: try fields.decode(String.self, forKey: .refresh))
        case "awaitReconciled":
            self = .awaitReconciled(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "awaitOffline":
            self = .awaitOffline(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "awaitBackground":
            self = .awaitBackground(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "conversation":
            self = .conversation(agent: try fields.decode(String.self, forKey: .agent))
        case "setModel":
            self = .setModel(agent: try fields.decode(String.self, forKey: .agent),
                             name: try fields.decode(String.self, forKey: .name))
        case "bridge": self = .bridge
        case "runtimeLog":
            self = .runtimeLog(
                bytes: try fields.decodeIfPresent(Int.self, forKey: .bytes) ?? 64_000)
        case "signposts": self = .signposts
        case "measure":
            self = .measure(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "appearance":
            self = .appearance(try fields.decode(Appearance.self, forKey: .appearance))
        case "dynamicType":
            self = .dynamicType(try fields.decode(String.self, forKey: .size))
        case "states": self = .states
        case "assist":
            self = .assist(
                motion: try fields.decode(Bool.self, forKey: .motion),
                transparency: try fields.decode(Bool.self, forKey: .transparency))
        case "geometry":
            self = .geometry(on: try fields.decode(Bool.self, forKey: .on))
        case "perturb":
            self = .perturb(token: try fields.decodeIfPresent(String.self, forKey: .token))
        case "designVariant":
            self = .designVariant(try fields.decodeIfPresent(String.self, forKey: .name))
        case "settle": self = .settle
        case "query": self = .query
        case "capture":
            self = .capture(path: try fields.decode(String.self, forKey: .path))
        case "tap":
            self = .tap(identifier: try fields.decode(String.self, forKey: .identifier))
        case "choose":
            self = .choose(label: try fields.decode(String.self, forKey: .label))
        case "perform":
            self = .perform(
                identifier: try fields.decode(String.self, forKey: .identifier),
                action: try fields.decode(String.self, forKey: .action))
        case "type":
            self = .type(
                identifier: try fields.decode(String.self, forKey: .identifier),
                text: try fields.decode(String.self, forKey: .text))
        case "search":
            self = .search(text: try fields.decode(String.self, forKey: .text))
        case "clear":
            self = .clear(identifier: try fields.decode(String.self, forKey: .identifier))
        case "scroll":
            self = .scroll(
                direction: try fields.decode(String.self, forKey: .direction),
                identifier: try fields.decodeIfPresent(String.self, forKey: .identifier))
        case "paste":
            self = .paste(
                identifier: try fields.decode(String.self, forKey: .identifier),
                text: try fields.decode(String.self, forKey: .text))
        case "move":
            self = .move(
                identifier: try fields.decode(String.self, forKey: .identifier),
                from: try fields.decode(Int.self, forKey: .from),
                to: try fields.decode(Int.self, forKey: .to))
        case "attach":
            self = .attach(
                agent: try fields.decode(String.self, forKey: .agent),
                kind: try fields.decode(String.self, forKey: .attachment),
                name: try fields.decode(String.self, forKey: .name),
                mime: try fields.decode(String.self, forKey: .mime),
                base64: try fields.decode(String.self, forKey: .base64))
        case "pair":
            self = .pair(qr: try fields.decode(String.self, forKey: .qr))
        case "pairByCode":
            self = .pairByCode(
                host: try fields.decode(String.self, forKey: .host),
                pin: try fields.decode(String.self, forKey: .pin))
        case "localNetwork":
            self = .localNetwork(permission: try fields.decode(String.self, forKey: .permission))
        case "revoke":
            self = .revoke(host: try fields.decode(String.self, forKey: .host))
        case "requestChanges":
            self = .requestChanges(
                agent: try fields.decode(String.self, forKey: .agent),
                base: try fields.decode(String.self, forKey: .base))
        case "watch":
            self = .watch(agent: try fields.decode(String.self, forKey: .agent))
        case "awaitReply":
            self = .awaitReply(
                agent: try fields.decode(String.self, forKey: .agent),
                saying: try fields.decode(String.self, forKey: .saying),
                seconds: try fields.decode(Double.self, forKey: .seconds))
        case "awaitAgent":
            self = .awaitAgent(
                agent: try fields.decode(String.self, forKey: .agent),
                seconds: try fields.decode(Double.self, forKey: .seconds))
        case "awaitSendable":
            self = .awaitSendable(
                agent: try fields.decode(String.self, forKey: .agent),
                seconds: try fields.decode(Double.self, forKey: .seconds))
        case "send":
            self = .send(
                agent: try fields.decode(String.self, forKey: .agent),
                text: try fields.decode(String.self, forKey: .text))
        case "sendDraft":
            self = .sendDraft(
                agent: try fields.decode(String.self, forKey: .agent),
                prose: try fields.decode(String.self, forKey: .prose))
        case "report":
            self = .report(
                path: try fields.decode(String.self, forKey: .path),
                note: try fields.decodeIfPresent(String.self, forKey: .note) ?? "",
                marks: try fields.decodeIfPresent([ReportMark].self, forKey: .marks) ?? [])
        case "screenshot": self = .screenshot
        case "uploaded":
            self = .uploaded(path: try fields.decode(String.self, forKey: .path))
        case "holdBackground": self = .holdBackground
        case "push":
            self = .push(path: try fields.decode(String.self, forKey: .path))
        case "shutdown": self = .shutdown
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: fields, debugDescription: "no door request named \(kind)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Key.self)
        switch self {
        case .open(let screen, let fixture):
            try fields.encode("open", forKey: .kind)
            try fields.encode(screen, forKey: .screen)
            try fields.encodeIfPresent(fixture, forKey: .fixture)
        case .cloud(let state):
            try fields.encode("cloud", forKey: .kind)
            try fields.encode(state, forKey: .cloud)
        case .store(let state):
            try fields.encode("store", forKey: .kind)
            try fields.encode(state, forKey: .store)
        case .calls:
            try fields.encode("calls", forKey: .kind)
        case .accounts:
            try fields.encode("accounts", forKey: .kind)
        case .addAccount(let user, let token):
            try fields.encode("addAccount", forKey: .kind)
            try fields.encode(user, forKey: .user)
            try fields.encode(token, forKey: .token)
        case .refreshEntitlement:
            try fields.encode("refreshEntitlement", forKey: .kind)
        case .late(let account):
            try fields.encode("late", forKey: .kind)
            try fields.encode(account, forKey: .account)
        case .connect(let relay, let token, let user, let tier):
            try fields.encode("connect", forKey: .kind)
            try fields.encode(relay, forKey: .relay)
            try fields.encode(token, forKey: .token)
            try fields.encode(user, forKey: .user)
            try fields.encode(tier, forKey: .tier)
        case .restoreSession(let account, let refresh):
            try fields.encode("restoreSession", forKey: .kind)
            try fields.encode(account, forKey: .account)
            try fields.encode(refresh, forKey: .refresh)
        case .awaitReconciled(let seconds):
            try fields.encode("awaitReconciled", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .awaitOffline(let seconds):
            try fields.encode("awaitOffline", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .awaitBackground(let seconds):
            try fields.encode("awaitBackground", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .conversation(let agent):
            try fields.encode("conversation", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
        case .setModel(let agent, let name):
            try fields.encode("setModel", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(name, forKey: .name)
        case .bridge:
            try fields.encode("bridge", forKey: .kind)
        case .runtimeLog(let bytes):
            try fields.encode("runtimeLog", forKey: .kind)
            try fields.encode(bytes, forKey: .bytes)
        case .signposts:
            try fields.encode("signposts", forKey: .kind)
        case .measure(let seconds):
            try fields.encode("measure", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .appearance(let appearance):
            try fields.encode("appearance", forKey: .kind)
            try fields.encode(appearance, forKey: .appearance)
        case .dynamicType(let size):
            try fields.encode("dynamicType", forKey: .kind)
            try fields.encode(size, forKey: .size)
        case .states:
            try fields.encode("states", forKey: .kind)
        case .assist(let motion, let transparency):
            try fields.encode("assist", forKey: .kind)
            try fields.encode(motion, forKey: .motion)
            try fields.encode(transparency, forKey: .transparency)
        case .geometry(let on):
            try fields.encode("geometry", forKey: .kind)
            try fields.encode(on, forKey: .on)
        case .perturb(let token):
            try fields.encode("perturb", forKey: .kind)
            try fields.encodeIfPresent(token, forKey: .token)
        case .designVariant(let name):
            try fields.encode("designVariant", forKey: .kind)
            try fields.encodeIfPresent(name, forKey: .name)
        case .settle:
            try fields.encode("settle", forKey: .kind)
        case .query:
            try fields.encode("query", forKey: .kind)
        case .capture(let path):
            try fields.encode("capture", forKey: .kind)
            try fields.encode(path, forKey: .path)
        case .tap(let identifier):
            try fields.encode("tap", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
        case .choose(let label):
            try fields.encode("choose", forKey: .kind)
            try fields.encode(label, forKey: .label)
        case .perform(let identifier, let action):
            try fields.encode("perform", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
            try fields.encode(action, forKey: .action)
        case .type(let identifier, let text):
            try fields.encode("type", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
            try fields.encode(text, forKey: .text)
        case .search(let text):
            try fields.encode("search", forKey: .kind)
            try fields.encode(text, forKey: .text)
        case .clear(let identifier):
            try fields.encode("clear", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
        case .scroll(let direction, let identifier):
            try fields.encode("scroll", forKey: .kind)
            try fields.encode(direction, forKey: .direction)
            try fields.encodeIfPresent(identifier, forKey: .identifier)
        case .paste(let identifier, let text):
            try fields.encode("paste", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
            try fields.encode(text, forKey: .text)
        case .move(let identifier, let from, let to):
            try fields.encode("move", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
            try fields.encode(from, forKey: .from)
            try fields.encode(to, forKey: .to)
        case .attach(let agent, let kind, let name, let mime, let base64):
            try fields.encode("attach", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(kind, forKey: .attachment)
            try fields.encode(name, forKey: .name)
            try fields.encode(mime, forKey: .mime)
            try fields.encode(base64, forKey: .base64)
        case .pair(let qr):
            try fields.encode("pair", forKey: .kind)
            try fields.encode(qr, forKey: .qr)
        case .pairByCode(let host, let pin):
            try fields.encode("pairByCode", forKey: .kind)
            try fields.encode(host, forKey: .host)
            try fields.encode(pin, forKey: .pin)
        case .localNetwork(let permission):
            try fields.encode("localNetwork", forKey: .kind)
            try fields.encode(permission, forKey: .permission)
        case .revoke(let host):
            try fields.encode("revoke", forKey: .kind)
            try fields.encode(host, forKey: .host)
        case .requestChanges(let agent, let base):
            try fields.encode("requestChanges", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(base, forKey: .base)
        case .watch(let agent):
            try fields.encode("watch", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
        case .awaitAgent(let agent, let seconds):
            try fields.encode("awaitAgent", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(seconds, forKey: .seconds)
        case .awaitSendable(let agent, let seconds):
            try fields.encode("awaitSendable", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(seconds, forKey: .seconds)
        case .awaitReply(let agent, let saying, let seconds):
            try fields.encode("awaitReply", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(saying, forKey: .saying)
            try fields.encode(seconds, forKey: .seconds)
        case .send(let agent, let text):
            try fields.encode("send", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(text, forKey: .text)
        case .sendDraft(let agent, let prose):
            try fields.encode("sendDraft", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(prose, forKey: .prose)
        case .report(let path, let note, let marks):
            try fields.encode("report", forKey: .kind)
            try fields.encode(path, forKey: .path)
            try fields.encode(note, forKey: .note)
            try fields.encode(marks, forKey: .marks)
        case .screenshot:
            try fields.encode("screenshot", forKey: .kind)
        case .uploaded(let path):
            try fields.encode("uploaded", forKey: .kind)
            try fields.encode(path, forKey: .path)
        case .holdBackground:
            try fields.encode("holdBackground", forKey: .kind)
        case .push(let path):
            try fields.encode("push", forKey: .kind)
            try fields.encode(path, forKey: .path)
        case .shutdown:
            try fields.encode("shutdown", forKey: .kind)
        }
    }
}

extension DoorReply: Codable {
    private enum Key: String, CodingKey {
        case kind, state, bridge, path, width, height, scale, message, parts, marks, measurement
        case host, delivered, reason, cloud, store, known, states, conversation, reportJSON
        case log
    }

    public init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: Key.self)
        let kind = try fields.decode(String.self, forKey: .kind)
        switch kind {
        case "conversation":
            self = .conversation(try fields.decode(ConversationReading.self, forKey: .conversation))
        case "ack": self = .ack
        case "state":
            self = .state(try fields.decode(VisibleState.self, forKey: .state))
        case "bridge":
            self = .bridge(try fields.decode(BridgeState.self, forKey: .bridge))
        case "runtimeLog":
            self = .runtimeLog(try fields.decode(String.self, forKey: .log))
        case "signposts":
            self = .signposts(try fields.decode([SignpostMark].self, forKey: .marks))
        case "measurement":
            self = .measurement(try fields.decode(Measurement.self, forKey: .measurement))
        case "captured":
            self = .captured(
                path: try fields.decode(String.self, forKey: .path),
                width: try fields.decode(Int.self, forKey: .width),
                height: try fields.decode(Int.self, forKey: .height),
                scale: try fields.decode(Int.self, forKey: .scale))
        case "bundle":
            self = .bundle(
                path: try fields.decode(String.self, forKey: .path),
                parts: try fields.decode([String].self, forKey: .parts),
                reportJSON: try fields.decodeIfPresent(String.self, forKey: .reportJSON))
        case "paired":
            self = .paired(host: try fields.decode(String.self, forKey: .host))
        case "calls":
            self = .calls(
                cloud: try fields.decode([String].self, forKey: .cloud),
                store: try fields.decode([String].self, forKey: .store))
        case "accounts":
            self = .accounts(try fields.decode(AccountsState.self, forKey: .known))
        case "states":
            self = .states(try fields.decode([DrawnState].self, forKey: .states))
        case "sendAttempt":
            self = .sendAttempt(
                delivered: try fields.decode(Bool.self, forKey: .delivered),
                reason: try fields.decodeIfPresent(String.self, forKey: .reason))
        case "error":
            self = .error(try fields.decode(String.self, forKey: .message))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: fields, debugDescription: "no door reply named \(kind)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: Key.self)
        switch self {
        case .ack:
            try fields.encode("ack", forKey: .kind)
        case .state(let state):
            try fields.encode("state", forKey: .kind)
            try fields.encode(state, forKey: .state)
        case .conversation(let reading):
            try fields.encode("conversation", forKey: .kind)
            try fields.encode(reading, forKey: .conversation)
        case .bridge(let state):
            try fields.encode("bridge", forKey: .kind)
            try fields.encode(state, forKey: .bridge)
        case .runtimeLog(let log):
            try fields.encode("runtimeLog", forKey: .kind)
            try fields.encode(log, forKey: .log)
        case .signposts(let marks):
            try fields.encode("signposts", forKey: .kind)
            try fields.encode(marks, forKey: .marks)
        case .measurement(let measurement):
            try fields.encode("measurement", forKey: .kind)
            try fields.encode(measurement, forKey: .measurement)
        case .captured(let path, let width, let height, let scale):
            try fields.encode("captured", forKey: .kind)
            try fields.encode(path, forKey: .path)
            try fields.encode(width, forKey: .width)
            try fields.encode(height, forKey: .height)
            try fields.encode(scale, forKey: .scale)
        case .bundle(let path, let parts, let reportJSON):
            try fields.encode("bundle", forKey: .kind)
            try fields.encode(path, forKey: .path)
            try fields.encode(parts, forKey: .parts)
            try fields.encodeIfPresent(reportJSON, forKey: .reportJSON)
        case .paired(let host):
            try fields.encode("paired", forKey: .kind)
            try fields.encode(host, forKey: .host)
        case .calls(let cloud, let store):
            try fields.encode("calls", forKey: .kind)
            try fields.encode(cloud, forKey: .cloud)
            try fields.encode(store, forKey: .store)
        case .accounts(let known):
            try fields.encode("accounts", forKey: .kind)
            try fields.encode(known, forKey: .known)
        case .states(let drawn):
            try fields.encode("states", forKey: .kind)
            try fields.encode(drawn, forKey: .states)
        case .sendAttempt(let delivered, let reason):
            try fields.encode("sendAttempt", forKey: .kind)
            try fields.encode(delivered, forKey: .delivered)
            try fields.encodeIfPresent(reason, forKey: .reason)
        case .error(let message):
            try fields.encode("error", forKey: .kind)
            try fields.encode(message, forKey: .message)
        }
    }
}

/// The launch arguments and file names the door and its driver both depend on.
public enum Door {
    /// `-amux-door-ready PATH`: where the app writes the port it is listening
    /// on, once it is listening. The driver waits for this file rather than
    /// guessing a port or a delay.
    public static let readyArgument = "amux-door-ready"

    /// `-amux-door-port N`: listen here instead of on a port the kernel
    /// picks and a readiness file names.
    ///
    /// The readiness file lives in the app's own container, which a driver on
    /// the Mac reads back out of it. A UI test cannot: it runs on the device,
    /// in a container of its own, with no way into the app's. So a launch a UI
    /// test starts says which port to listen on, and the test connects to it
    /// over the loopback both processes share.
    public static let portArgument = "amux-door-port"

    /// `-amux-door-bind ADDRESS`: listen on this address instead of IPv4
    /// loopback. A phone's driver runs on the Mac, so a launch on a phone
    /// names the phone's address on its USB link, which the Mac reaches
    /// without the door listening on any other network.
    public static let bindArgument = "amux-door-bind"

    /// `-amux-relay URL`: the relay an account added through the door signs
    /// in to. The request to add an account carries the user and the bearer;
    /// where to reach them is fixed for the launch.
    public static let relayArgument = "amux-relay"

    /// `-amux-scripted-cloud`: the app runs as itself, against the scripted
    /// account service and the scripted App Store instead of the real ones.
    ///
    /// Every other launch argument says what to reach. This one says what not
    /// to: signing in must not open a browser at amux.sh, buying must not
    /// reach StoreKit, and deleting must not delete anybody's real account.
    /// What the screens do with what comes back is the app's own — the two
    /// doubles are the same shape as the adapters they stand in for, and no
    /// screen can tell which it is holding.
    public static let scriptedCloudArgument = "amux-scripted-cloud"

    /// `-amux-element-geometry`: include the app-layout rectangles used by
    /// placement-sensitive debug journeys. Normal journeys read the system's
    /// accessibility frames instead, avoiding a geometry reader on every
    /// named element.
    public static let elementGeometryArgument = "amux-element-geometry"

    /// `-amux-discover-only ID,ID`: the machines a driven launch's browser may
    /// report, by host identity.
    ///
    /// A simulator browses the Mac's real network, so a driven app would
    /// otherwise find every amux machine running there — the developer's own
    /// server included — and a test would read a different screen on every
    /// Mac. A launch that opens the door and names nothing here finds nothing
    /// at all.
    public static let discoverOnlyArgument = "amux-discover-only"

    /// `-amux-link URL`: a link the launch was opened with, handed to the app
    /// before its first frame exactly as the system hands one over.
    ///
    /// Not another way to pair. It is how a driver reaches the cold start
    /// itself: the system opens an app with a URL by launching it, and a UI
    /// test cannot launch the app that way and still say which relay and
    /// which door to use. So the link travels in the launch beside them, and
    /// what happens to it afterwards is the app's own routing — including a
    /// link that lands before anybody has signed in.
    public static let linkArgument = "amux-link"

    /// The discovery scope a driven launch's runtime lists machines in: the
    /// scope its served test network advertises, so real machines on the same
    /// network never appear and the test network's never leak out.
    public static let discoveryScopeArgument = "amux-discovery-scope"

    /// Where a driven launch's direct links listen, as `ip:port`: loopback,
    /// so a simulator run never listens on the Mac's network.
    public static let lanBindArgument = "amux-lan-bind"

    /// A served test relay's plaintext carrier, as `ip:port`, which a driven
    /// launch dials instead of where the test account service names it.
    public static let relayTCPArgument = "amux-relay-tcp"

    /// A served test relay's QUIC carrier, as `ip:port`, and its certificate
    /// as hex, which a driven launch dials and trusts instead of where the
    /// test account service names it: the carrier a phone away from home
    /// uses.
    public static let relayQUICArgument = "amux-relay-quic"
    public static let relayRootArgument = "amux-relay-root"

    /// What the ready file holds.
    public struct Ready: Codable, Sendable, Equatable {
        public let port: UInt16
        public let pid: Int32

        public init(port: UInt16, pid: Int32) {
            self.port = port
            self.pid = pid
        }
    }
}

extension SwiftUI.DynamicTypeSize {
    /// What the door calls this size, in the spelling a request and a fixture
    /// use. Derived from the same table as the parsing below, so a size the
    /// door can be asked for is a size it can name back.
    public var doorName: String {
        Self.doorNames.first { $0.value == self }?.key ?? "unknown"
    }

    private static let doorNames: [String: SwiftUI.DynamicTypeSize] = [
        "xSmall": .xSmall, "small": .small, "medium": .medium, "large": .large,
        "xLarge": .xLarge, "xxLarge": .xxLarge, "xxxLarge": .xxxLarge,
        "accessibility1": .accessibility1, "accessibility2": .accessibility2,
        "accessibility3": .accessibility3, "accessibility4": .accessibility4,
        "accessibility5": .accessibility5,
    ]

    /// The door's names for the reader's type sizes. They are the plain
    /// spellings a person would write in a request or a fixture, and a fixture
    /// and a door request that name the same size get the same size.
    public init?(doorName: String) {
        guard let size = Self.doorNames[doorName] else { return nil }
        self = size
    }
}

/// What an agent's chat holds on this phone, read through the runtime the
/// app runs: its frame, its rows and the ask at its head.
public struct ConversationReading: Codable, Sendable, Equatable {
    public let agent: String
    public let frame: ChatFrame?
    /// The rows the chat holds, oldest first.
    public let rows: [Row]
    public let ask: AskCard?

    public init(agent: String, frame: ChatFrame?, rows: [Row], ask: AskCard?) {
        self.agent = agent
        self.frame = frame
        self.rows = rows
        self.ask = ask
    }
}

/// What a test account buys, as a driver hands it a credential.
public enum Tier: String, Codable, Sendable, Equatable {
    case free
    case pro
}
