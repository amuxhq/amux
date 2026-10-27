import Foundation

/// The UIKit views this app is allowed to contain.
///
/// Everything else in this package is SwiftUI and platform-neutral, so a
/// screen is a function of its state and can be captured, replayed and — when
/// there is a Mac app — drawn again without being rewritten. A UIKit view is
/// the exception, and an exception is only worth having where a measurement
/// says SwiftUI cannot do the job: a transcript that scrolls at the display's
/// cadence, a field that lays out attachment tokens inline, a diff that
/// selects a range of lines under a finger.
///
/// A leaf is one file under `Leaves/`, wrapped in one representable, named
/// here, and justified by a written measurement. `apps/apple/Tools/feature-lint.sh`
/// refuses UIKit anywhere else in this package, so the list here is the whole
/// of it and nobody has to go looking.
///
/// A case with no file behind it is a candidate, not a leaf: it is what the
/// measurement is about to be taken for.
public enum RegisteredLeaves: String, CaseIterable, Sendable {
    /// Built, and measured by the phone's performance suite: a SwiftUI
    /// stack of the drawn rows placed every one of them again on every
    /// arrival, about 60 ms a frame under a stream, and a lazy stack could
    /// not hold the reader's place. The leaf is a collection view that lays
    /// out the rows in view, keeps the place itself when rows land above
    /// the reader, and hosts the same SwiftUI rows. `docs/IOS.md` has the
    /// numbers.
    case transcriptList
    /// Answered without a leaf: the composer keeps a draft's attachments as
    /// chips beside the field rather than tokens inside its text, so the field
    /// is SwiftUI's own and nothing needs a run of text to be atomic. Inline
    /// tokens would reopen it.
    case tokenTextField
    /// Measured by argument rather than by a workload, and answered the same
    /// way: rows report their own frames as they lay out and a drag looks a
    /// point up among them, so nothing here needs a second layout system.
    /// `docs/IOS.md` has the reasoning and what would reopen it.
    case diffSelection
    /// Built, and measured by the live accessibility audit rather than a
    /// frame time: SwiftUI's `Menu` presents through a UIKit button that
    /// reaches VoiceOver without the menu's name or identifier. The leaf owns
    /// that button so it carries both. `docs/IOS.md` has the detail.
    case menuButton
    /// Built, and measured by the phone's performance suite: the chat's
    /// activity line and the working marks, animated by SwiftUI, held an
    /// iPhone 14 Pro's main thread about 16% busy with nothing arriving
    /// (31% before they stopped re-laying out the composer), and froze
    /// whenever that thread was busy, as when a chat opens. The leaf runs the
    /// same curves as layer animations in the render server.
    /// `docs/IOS.md` has the numbers.
    case workingMotion
}
