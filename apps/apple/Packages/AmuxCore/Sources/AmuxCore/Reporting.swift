import AmuxValues
import CoreGraphics
import Foundation
import ImageIO

/// The picture of the screen a report is about, and how big it was.
///
/// The image is the composited window, not a drawing the app makes of its own
/// view tree: what somebody is complaining about includes the material, the
/// blur and the system's own status bar, none of which the app can redraw.
///
/// The size is carried beside the bytes because a rectangle drawn on the
/// picture later has to be expressed in the same unit the phone laid the
/// screen out in. Points and scale rather than pixels: a note that said
/// "1206 across" would mean a different place on a phone at a different
/// scale, and the bundle is read on a Mac that has neither.
public struct FrozenFrame: Sendable, Equatable {
    /// The composited screen, as PNG bytes.
    public var png: Data
    /// How wide the screen was, in points.
    public var width: Double
    /// How tall it was, in points.
    public var height: Double
    /// How many pixels the phone drew per point.
    public var scale: Double

    public init(png: Data, width: Double, height: Double, scale: Double) {
        self.png = png
        self.width = width
        self.height = height
        self.scale = scale
    }
}

extension FrozenFrame {
    /// The frozen screen decoded, or nothing when the bytes are not an image
    /// this phone can read.
    ///
    /// The decode lives beside the value rather than in the screen that shows
    /// it: the screens in this app are written to be functions of their state,
    /// and turning bytes back into pixels is not something a screen decides.
    public var image: CGImage? {
        guard let source = CGImageSourceCreateWithData(png as CFData, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }
}

/// Everything a report is made of, taken in one instant.
///
/// The frame and the dump are frozen together and before anything else happens,
/// which is the whole point of the flow: what is reported is what was on
/// screen when something looked wrong, not what the app looks like once the
/// person has finished describing it. A capture taken after the report UI
/// opened would be a picture of the report.
public struct ReportCapture: Sendable, Equatable {
    public var frame: FrozenFrame
    /// The runtime's dump of this profile, started when the screen froze so
    /// it describes the same moment; its directory, or why there is none.
    public var dump: Task<Result<URL, PartAbsent>, Never>?
    /// The page the person was on.
    public var route: String?
    public var runtimeFailure: String?

    public init(
        frame: FrozenFrame,
        dump: Task<Result<URL, PartAbsent>, Never>? = nil,
        route: String? = nil,
        runtimeFailure: String? = nil
    ) {
        self.frame = frame
        self.dump = dump
        self.route = route
        self.runtimeFailure = runtimeFailure
    }
}

/// One rectangle somebody drew on the frozen frame, with what they said about
/// it.
///
/// Measured in the frame's own unit — points on a phone, cells in a terminal —
/// so a reader on a Mac puts it back where it was drawn without knowing what
/// scale the phone rendered at. Fractional, because a finger rarely lands on a
/// whole point.
public struct ReportMark: Sendable, Equatable, Codable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double
    public var note: String

    public init(x: Double, y: Double, width: Double, height: Double, note: String = "") {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
        self.note = note
    }
}

/// What somebody has written about the frame so far: one note about the whole
/// thing, and a note on each rectangle they drew.
public struct ReportDraft: Sendable, Equatable {
    public var note: String
    public var marks: [ReportMark]

    public init(note: String = "", marks: [ReportMark] = []) {
        self.note = note
        self.marks = marks
    }
}

/// Where a report stands with the cloud.
public enum ReportSending: Sendable, Equatable {
    case ready
    case sending
    /// It did not go. What the cloud said is kept, because the person has to
    /// decide whether to try again and the answer is the only thing that tells
    /// them whether it is worth doing.
    case failed(String)
    case sent(ReportReceipt)
}

/// Why a part of a report is not there.
///
/// A part that is missing carries the reason it is missing rather than
/// silently disappearing, so nobody reading a bundle mistakes a part somebody
/// withheld, or one the phone could not produce, for one that went astray.
public struct PartAbsent: Error, Sendable, Equatable {
    public let why: String

    public init(_ why: String) {
        self.why = why
    }
}

/// Where a report was filed: the account service's id for it, and when it
/// took it.
public struct ReportReceipt: Sendable, Equatable, Codable {
    public var id: String
    public var receivedAt: Date

    public init(id: String, receivedAt: Date) {
        self.id = id
        self.receivedAt = receivedAt
    }
}

/// What the phone hands the runtime to send: `report.json`, the frame and
/// the trace, and where the dump is.
///
/// Sealed at the first send, so a retry sends the bytes the person saw go
/// the first time rather than a fresh header with a later time on it.
public struct ReportUpload: Sendable, Equatable {
    /// `report.json` as the phone writes it; the runtime adds the schema and
    /// the dump's declaration.
    public var header: Data
    public var frame: Data
    /// Where the runtime finds the report's dump: the directory it wrote,
    /// empty when there is none, and after the first send the report's own
    /// directory, which a retry names to send that same report again.
    public var path: String

    public init(header: Data, frame: Data, path: String) {
        self.header = header
        self.frame = frame
        self.path = path
    }
}

/// Sends a report: the runtime of the profile on screen, which writes it
/// beside its dump and uploads it to the profile's account.
///
/// A seam so the store can be driven without a runtime; the profile is the
/// only real one.
public protocol ReportSubmitting: Sendable {
    func submitReport(_ upload: ReportUpload) async -> Result<ReportSent, RuntimeFailure>
}

/// Freezes the screen and the recordings behind it.
///
/// A seam rather than a call, for the same reason the cloud and the store are
/// seams: taking a picture of the window is UIKit's, and a screen or a store
/// that reached for it could not be photographed or driven away from a device.
@MainActor
public protocol ReportFreezing: AnyObject {
    /// Everything a report needs, taken now, or nothing when the screen could
    /// not be photographed at all.
    func freeze() -> ReportCapture?
}

/// The one report this phone is in the middle of.
///
/// Its own store because the capture outlives the moment that made it: the
/// system's screenshot preview slides over the app immediately afterwards and
/// the app is put out of the way while somebody looks at it. Coming back has
/// to find the same frozen frame and the same offer, which it cannot if the
/// offer is view state on a screen that went away.
///
/// One at a time. A second screenshot while a report is being written replaces
/// nothing: the person is already describing the first, and swapping the
/// picture under them would lose the notes they had already put on it.
@MainActor
@Observable
public final class ReportStore {
    /// What was frozen, until it is sent or thrown away.
    public private(set) var capture: ReportCapture?
    /// Whether the app is offering to report it. False while the report itself
    /// is open — the offer has been taken by then.
    public private(set) var offering = false
    /// Whether the report is open over the app.
    public private(set) var open = false
    /// What has been written about the frame: one note, and a note per
    /// rectangle. Public to set, because the screen's fields bind to it.
    public var draft = ReportDraft()
    /// Where the report stands with the cloud.
    public private(set) var sending: ReportSending = .ready

    /// The first send seals the bytes so a retry keeps the report's identity.
    public private(set) var sealed: ReportUpload?
    private var captureID = UUID()

    public init() {}

    /// A report already in progress, as a declared state has it.
    ///
    /// A state is written rather than reached here, for the reason every
    /// declared state exists: a report starts from a screenshot the system
    /// takes, and nothing inside the app can make one of those happen.
    public init(
        capture: ReportCapture?, draft: ReportDraft = ReportDraft(),
        sending: ReportSending = .ready, open: Bool = false
    ) {
        self.capture = capture
        self.draft = draft
        self.sending = sending
        self.open = open && capture != nil
    }

    /// Freezes the screen and offers to report it.
    ///
    /// This is the screenshot path. The offer appears only after the capture
    /// is in hand, so there is no window in which the app has drawn something
    /// about reporting and not yet taken the picture.
    @discardableResult
    public func offer(_ freezer: any ReportFreezing) -> Bool {
        guard !open, capture == nil else { return false }
        guard let frozen = freezer.freeze() else { return false }
        capture = frozen
        offering = true
        return true
    }

    /// Freezes the screen and opens the report on it, with no offer in
    /// between. This is the deliberate path, from Help: somebody who went
    /// looking for it has already said yes.
    @discardableResult
    public func begin(_ freezer: any ReportFreezing) -> Bool {
        guard !open else { return false }
        guard let frozen = freezer.freeze() else { return false }
        capture = frozen
        offering = false
        open = true
        return true
    }

    /// Took the offer up. Nothing is captured here: the picture was taken when
    /// the offer was made, which is what makes it a picture of the problem.
    public func accept() {
        guard capture != nil else { return }
        offering = false
        open = true
    }

    /// Turned the offer down, or closed the report. The capture goes with it,
    /// and so does anything written about it: the report is abandoned.
    public func dismiss() {
        offering = false
        open = false
        capture = nil
        draft = ReportDraft()
        sending = .ready
        sealed = nil
        captureID = UUID()
    }

    // MARK: - Writing it

    /// Draws a rectangle around something wrong. It arrives with no note; the
    /// screen then asks for one, which is why an empty note is a legal state
    /// and not a validation failure.
    public func mark(_ mark: ReportMark) {
        draft.marks.append(mark)
    }

    /// What somebody typed about a rectangle they had already drawn.
    public func note(_ text: String, on index: Int) {
        guard draft.marks.indices.contains(index) else { return }
        draft.marks[index].note = text
    }

    /// Takes a rectangle back off the frame, with whatever was said about it.
    public func unmark(_ index: Int) {
        guard draft.marks.indices.contains(index) else { return }
        draft.marks.remove(at: index)
    }

    // MARK: - Sending it

    /// Hands the report to the runtime, which writes it beside its dump and
    /// sends it to the account.
    ///
    /// A failure keeps everything. The draft, the rectangles and the frozen
    /// frame are all still here afterwards, so Retry is one press and not a
    /// second report: somebody who wrote three notes about a bug on a train
    /// must not lose them to a tunnel. The runtime keeps the report on the
    /// phone either way, and a retry names it to send that one again.
    ///
    /// The account is what the report is filed under, and there may not be
    /// one: a phone nobody has signed in on yet can still take a screenshot
    /// and write three notes on it. Pressing Send then is refused in the same
    /// place the account service's own refusals are said, because the one
    /// thing it must not do is nothing at all — a button that quietly does
    /// not work leaves somebody pressing it and waiting.
    public func send(
        with submitter: (any ReportSubmitting)?, as account: AccountId?,
        build: String, gitSHA: String = "", now: Date = Date()
    ) async {
        guard capture != nil else { return }
        switch sending {
        case .sending, .sent: return
        case .ready, .failed: break
        }
        guard account != nil else {
            sending = .failed(
                "Sign in to the account this report is about, then send it again.")
            return
        }
        guard let submitter else {
            sending = .failed("amux is not running on this phone yet. Try again in a moment.")
            return
        }
        guard let upload = await assembled(build: build, gitSHA: gitSHA, now: now)
        else { return }
        sealed = upload
        let identity = captureID
        sending = .sending
        let answer = await submitter.submitReport(upload)
        guard captureID == identity else { return }
        switch answer {
        case .success(let sent):
            sealed?.path = sent.reportPath
            sending = sent.notSent.isEmpty
                ? .sent(ReportReceipt(
                    id: sent.id,
                    receivedAt: Date(timeIntervalSince1970: Double(sent.receivedAtMs) / 1000)))
                : .failed(sent.notSent)
        case .failure(let why):
            sending = .failed(why.description)
        }
    }

    /// What would be sent right now. The screen shows what a report declares
    /// before anybody presses anything, and a test reads the same thing.
    public func assembled(
        build: String, gitSHA: String = "", now: Date = Date()
    ) async -> ReportUpload? {
        if let sealed { return sealed }
        guard let capture else { return nil }
        let dump = await ReportAssembly.dumped(capture.dump)
        return ReportAssembly.upload(
            from: capture, draft: draft, build: build, gitSHA: gitSHA,
            createdAt: now, dump: dump)
    }
}
