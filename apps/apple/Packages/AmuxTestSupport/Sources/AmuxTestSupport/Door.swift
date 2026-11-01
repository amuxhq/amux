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
    /// Start the shared runtime against a relay with a credential, and
    /// what that credential buys.
    ///
    /// The tier is said because nothing below can read one out of an
    /// opaque bearer: the account service issues it beside a real
    /// credential, and a driver handing one over stands in for that.
    /// Paid unless a journey is about an account that has not paid.
    case connect(relay: String, token: String, user: String, tier: Tier)
    /// Wait until the fleet has been confirmed by a host, or give up after
    /// this many seconds. A connection is asynchronous, so a driver that read
    /// the screen straight after `connect` would read the moment before it.
    case awaitReconciled(seconds: Double)
    /// Wait until the app is in the background and has told its runtime so,
    /// or give up after this many seconds. Putting the app away is the
    /// simulator's to do and takes a moment; what the runtime keeps current
    /// changes only once it has.
    case awaitBackground(seconds: Double)
    /// Read the production conversation's projection, including its native layer.
    case conversation(agent: String)
    /// Every moment this launch has marked, in order. A driver reads them to
    /// tell a screen that was drawn from a frame that was shown, and to see
    /// when the fleet stopped being a memory.
    /// Watch the display, the main thread and the footprint for this long
    /// and answer what they did; the driver runs the workload meanwhile.
    case measure(seconds: Double)
    case signposts
    /// Whether the device is hot or saving power, which would make whatever
    /// a run measured next about the device rather than the app.
    case condition
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
    /// Wait until the screen has stopped changing. A capture that does not
    /// wait for this photographs a frame mid-animation.
    case settle
    case query
    /// Write a PNG of the composited window at the given path, which is the
    /// app's own to write — a driver on the Mac reads it back out of the
    /// app's container.
    case capture(path: String)
    /// Press the element named `identifier`. With `after`, the element is
    /// found and the screen held still first, and the press waits that many
    /// seconds more, so a measurement begun meanwhile sees the app's answer
    /// to the press and none of the driver's search for it.
    case tap(identifier: String, after: Double? = nil)
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
    /// Wait until the fleet names this agent, or give up after this many
    /// seconds.
    ///
    /// A machine that has just admitted this phone has not finished saying
    /// what it is running: trust is written, and the account of the machine
    /// follows it over the same link. A driver that opened a conversation in
    /// that gap would be telling the runtime to watch an agent it has never
    /// heard of, and nothing would arrive.
    case awaitAgent(agent: String, seconds: Double)
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
    case conversation(ConversationReading)
    case signposts([SignpostMark])
    case measurement(Measurement)
    case condition(DeviceCondition)
    case captured(path: String, width: Int, height: Int, scale: Int)
    /// This phone now trusts the machine of this name.
    case paired(host: String)
    /// What the scripted cloud and the scripted store were asked, in order.
    case calls(cloud: [String], store: [String])
    /// The accounts this phone knows.
    case accounts(AccountsState)
    /// Why the request could not be answered, in one line. The door never
    /// half-answers: a request either happened or is reported here.
    case error(String)
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
        case identifier, text, seconds, after, qr, agent
        case attachment, name, mime, base64, host, pin
        case motion, transparency, on
        case permission, tier
        case label, action, direction
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
        case "connect":
            self = .connect(
                relay: try fields.decode(String.self, forKey: .relay),
                token: try fields.decode(String.self, forKey: .token),
                user: try fields.decode(String.self, forKey: .user),
                tier: try fields.decodeIfPresent(Tier.self, forKey: .tier) ?? .pro)
        case "awaitReconciled":
            self = .awaitReconciled(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "awaitBackground":
            self = .awaitBackground(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "conversation":
            self = .conversation(agent: try fields.decode(String.self, forKey: .agent))
        case "signposts": self = .signposts
        case "condition": self = .condition
        case "measure":
            self = .measure(seconds: try fields.decode(Double.self, forKey: .seconds))
        case "appearance":
            self = .appearance(try fields.decode(Appearance.self, forKey: .appearance))
        case "dynamicType":
            self = .dynamicType(try fields.decode(String.self, forKey: .size))
        case "assist":
            self = .assist(
                motion: try fields.decode(Bool.self, forKey: .motion),
                transparency: try fields.decode(Bool.self, forKey: .transparency))
        case "geometry":
            self = .geometry(on: try fields.decode(Bool.self, forKey: .on))
        case "settle": self = .settle
        case "query": self = .query
        case "capture":
            self = .capture(path: try fields.decode(String.self, forKey: .path))
        case "tap":
            self = .tap(
                identifier: try fields.decode(String.self, forKey: .identifier),
                after: try fields.decodeIfPresent(Double.self, forKey: .after))
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
        case "awaitAgent":
            self = .awaitAgent(
                agent: try fields.decode(String.self, forKey: .agent),
                seconds: try fields.decode(Double.self, forKey: .seconds))
        case "screenshot": self = .screenshot
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
        case .connect(let relay, let token, let user, let tier):
            try fields.encode("connect", forKey: .kind)
            try fields.encode(relay, forKey: .relay)
            try fields.encode(token, forKey: .token)
            try fields.encode(user, forKey: .user)
            try fields.encode(tier, forKey: .tier)
        case .awaitReconciled(let seconds):
            try fields.encode("awaitReconciled", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .awaitBackground(let seconds):
            try fields.encode("awaitBackground", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .conversation(let agent):
            try fields.encode("conversation", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
        case .signposts:
            try fields.encode("signposts", forKey: .kind)
        case .condition:
            try fields.encode("condition", forKey: .kind)
        case .measure(let seconds):
            try fields.encode("measure", forKey: .kind)
            try fields.encode(seconds, forKey: .seconds)
        case .appearance(let appearance):
            try fields.encode("appearance", forKey: .kind)
            try fields.encode(appearance, forKey: .appearance)
        case .dynamicType(let size):
            try fields.encode("dynamicType", forKey: .kind)
            try fields.encode(size, forKey: .size)
        case .assist(let motion, let transparency):
            try fields.encode("assist", forKey: .kind)
            try fields.encode(motion, forKey: .motion)
            try fields.encode(transparency, forKey: .transparency)
        case .geometry(let on):
            try fields.encode("geometry", forKey: .kind)
            try fields.encode(on, forKey: .on)
        case .settle:
            try fields.encode("settle", forKey: .kind)
        case .query:
            try fields.encode("query", forKey: .kind)
        case .capture(let path):
            try fields.encode("capture", forKey: .kind)
            try fields.encode(path, forKey: .path)
        case .tap(let identifier, let after):
            try fields.encode("tap", forKey: .kind)
            try fields.encode(identifier, forKey: .identifier)
            try fields.encodeIfPresent(after, forKey: .after)
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
        case .awaitAgent(let agent, let seconds):
            try fields.encode("awaitAgent", forKey: .kind)
            try fields.encode(agent, forKey: .agent)
            try fields.encode(seconds, forKey: .seconds)
        case .screenshot:
            try fields.encode("screenshot", forKey: .kind)
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
        case kind, state, path, width, height, scale, message, marks, measurement
        case host, cloud, store, known, conversation, condition
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
        case "signposts":
            self = .signposts(try fields.decode([SignpostMark].self, forKey: .marks))
        case "measurement":
            self = .measurement(try fields.decode(Measurement.self, forKey: .measurement))
        case "condition":
            self = .condition(try fields.decode(DeviceCondition.self, forKey: .condition))
        case "captured":
            self = .captured(
                path: try fields.decode(String.self, forKey: .path),
                width: try fields.decode(Int.self, forKey: .width),
                height: try fields.decode(Int.self, forKey: .height),
                scale: try fields.decode(Int.self, forKey: .scale))
        case "paired":
            self = .paired(host: try fields.decode(String.self, forKey: .host))
        case "calls":
            self = .calls(
                cloud: try fields.decode([String].self, forKey: .cloud),
                store: try fields.decode([String].self, forKey: .store))
        case "accounts":
            self = .accounts(try fields.decode(AccountsState.self, forKey: .known))
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
        case .signposts(let marks):
            try fields.encode("signposts", forKey: .kind)
            try fields.encode(marks, forKey: .marks)
        case .measurement(let measurement):
            try fields.encode("measurement", forKey: .kind)
            try fields.encode(measurement, forKey: .measurement)
        case .condition(let condition):
            try fields.encode("condition", forKey: .kind)
            try fields.encode(condition, forKey: .condition)
        case .captured(let path, let width, let height, let scale):
            try fields.encode("captured", forKey: .kind)
            try fields.encode(path, forKey: .path)
            try fields.encode(width, forKey: .width)
            try fields.encode(height, forKey: .height)
            try fields.encode(scale, forKey: .scale)
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

    /// `-amux-fresh-start YES`: before anything else, forget everything this
    /// installation kept — its files, its caches and its settings — as an
    /// uninstall would. A driver on a phone starts a run this way rather than
    /// by reinstalling, because an uninstall also takes the permissions a
    /// person granted, and the system then asks for local network access
    /// again with nobody there to answer.
    public static let freshStartArgument = "amux-fresh-start"

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
