import AmuxValues
import AmuxWords
import SwiftUI

/// The tool window's Review tab: the open agent's changes, to read and
/// comment on line by line. The tab and the toggle that closes it are on
/// the title row above.
///
/// Each time it shows it opens the agent's review for a look, and draws
/// that page while it is open. Where the agent's host offers no reviews,
/// the page says so.
public struct ChangesPane: View {
    let chat: any ChatSurface
    /// The review has been asked to open, so its page is drawn.
    @State private var opened = false

    public init(chat: any ChatSurface) {
        self.chat = chat
    }

    public var body: some View {
        Group {
            if opened, let page = chat.reviewPage {
                ReviewPagePane(review: page)
            } else {
                Color.clear
            }
        }
        .onAppear {
            chat.openReviewPage()
            opened = true
        }
        .onDisappear { chat.closeReviewPage() }
        // Another agent's chat is another review: the page shut here opens
        // there.
        .id(ObjectIdentifier(chat))
    }
}

/// A file's name in the text colour after its folder in a quieter one, so
/// the eye finds the file; its old path first when it moved.
struct FileName: View {
    let path: String
    let from: String?

    var body: some View {
        let folder = (path as NSString).deletingLastPathComponent
        let name = (path as NSString).lastPathComponent
        (Text(from.map { "\($0) → " } ?? "").foregroundStyle(Palette.textTertiary)
            + Text(folder.isEmpty ? "" : folder + "/").foregroundStyle(Palette.textTertiary)
            + Text(name).foregroundStyle(.primary).fontWeight(.medium))
            .font(.ui(.meta))
            .lineLimit(1)
            .truncationMode(.head)
    }
}
