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
/// The three recordings are frozen together and before anything else happens,
/// which is the whole point of the flow: what is reported is what was on
/// screen when something looked wrong, not what the app looks like once the
/// person has finished describing it. A capture taken after the report UI
/// opened would be a picture of the report.
///
/// The view-state trace is held as the text that goes into the bundle rather
/// than as parsed values. Nothing between the freeze and the send needs to read
/// it, and holding it as text means the bytes written into `trace.jsonl` are
/// the bytes that were frozen — a re-encode in between could differ from what
/// the phone saw.
public struct ReportCapture: Sendable, Equatable {
    public var frame: FrozenFrame
    /// The runtime's dump of this profile, started when the screen froze so
    /// it describes the same moment; its directory, or why there is none.
    public var dump: Task<Result<URL, PartAbsent>, Never>?
    /// The view-state recording, where this build keeps one.
    public var trace: String?
    public var traceAbsent: String?
    /// The page the person was on.
    public var route: String?
    public var runtimeFailure: String?

    public init(
        frame: FrozenFrame,
        dump: Task<Result<URL, PartAbsent>, Never>? = nil,
        trace: String? = nil,
        traceAbsent: String? = nil,
        route: String? = nil,
        runtimeFailure: String? = nil
    ) {
        self.frame = frame
        self.dump = dump
        self.trace = trace
        self.traceAbsent = traceAbsent
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
    public private(set) var uploadBundle: ReportBundle?
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
        uploadBundle = nil
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

    /// Assembles the bundle and hands it to the account service.
    ///
    /// A failure keeps everything. The draft, the rectangles and the frozen
    /// frame are all still here afterwards, so Retry is one press and not a
    /// second report: somebody who wrote three notes about a bug on a train
    /// must not lose them to a tunnel.
    ///
    /// The account is what the report is filed under, and there may not be
    /// one: a phone nobody has signed in on yet can still take a screenshot
    /// and write three notes on it. Pressing Send then is refused in the same
    /// place the account service's own refusals are said, because the one
    /// thing it must not do is nothing at all — a button that quietly does
    /// not work leaves somebody pressing it and waiting.
    public func send(
        with cloud: any CloudService, as account: AccountId?,
        build: String, gitSHA: String = "", now: Date = Date()
    ) async {
        guard capture != nil else { return }
        switch sending {
        case .sending, .sent: return
        case .ready, .failed: break
        }
        guard let account else {
            sending = .failed(
                "Sign in to the account this report is about, then send it again.")
            return
        }
        guard let bundle = await assembled(build: build, gitSHA: gitSHA, now: now)
        else { return }
        uploadBundle = bundle
        let identity = captureID
        sending = .sending
        do {
            let receipt = try await cloud.uploadReport(account, bundle: bundle)
            guard captureID == identity else { return }
            sending = .sent(receipt)
        } catch {
            guard captureID == identity else { return }
            sending = .failed(Self.sentence(for: error))
        }
    }

    /// What the person is told when the cloud would not take it. Said in the
    /// cloud's own words where it gave any, because "something went wrong"
    /// tells nobody whether pressing Retry is worth it.
    static func sentence(for error: CloudError) -> String {
        switch error {
        case .cancelled: "the upload was stopped"
        case .unauthenticated: "amux.sh no longer recognises this account"
        case .refused(let reason): reason
        case .network(let what): what
        case .timeout: "amux.sh did not answer"
        }
    }

    /// What would be sent right now. The screen shows what a report declares
    /// before anybody presses anything, and a test reads the same thing.
    public func assembled(
        build: String, gitSHA: String = "", now: Date = Date()
    ) async -> ReportBundle? {
        if let uploadBundle { return uploadBundle }
        guard let capture else { return nil }
        let dump = await ReportAssembly.dumpParts(capture.dump)
        return ReportAssembly.bundle(
            from: capture, draft: draft, build: build, gitSHA: gitSHA,
            createdAt: now, dump: dump)
    }
}
