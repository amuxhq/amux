import AmuxCore
import AmuxFeatures
import Foundation
import SwiftUI

/// A long chat with no runtime behind it, for a UI test to move with a real
/// finger. Only XCUITest makes the touches UIKit treats as a drag, and it
/// drives another process, so the chat has to be on screen when the app
/// launches rather than be set up from inside the test.
///
/// The rows are chosen to differ from the height an unmeasured row is
/// assumed to stand at: most are folded and draw nothing, so the rows just
/// above the screen shrink the list by hundreds of points as they come near.
@MainActor
enum FingerStage {
    static var requested: Bool {
        ProcessInfo.processInfo.environment["AMUX_FINGER_STAGE"] == "1"
    }

    private static let model: ChatModel = {
        let rows = (0..<200).map { index -> Row in
            let order = UInt64(index)
            let id = "row-\(index)"
            switch index % 5 {
            case 0:
                let words = "message \(index): the agent reports progress on its task"
                    + String(repeating: " and then some more of it", count: (index % 7) * 4)
                return ScriptedChat.row(id, order, prose(words))
            case 1:
                return ScriptedChat.row(id, order, prose("short \(index)"))
            default:
                return ScriptedChat.row(id, order, prose("folded \(index)"), collapsed: true)
            }
        }
        return ChatModel(source: ScriptedChat(rows: rows, frame: ScriptedChat.frame(hasOlder: false)))
    }()

    private static func prose(_ words: String) -> RowKind {
        .prose(text: [.text(words)], streaming: false, workingNote: false)
    }

    static var screen: some View {
        ChatScreen(
            model: model,
            subject: ChatSubject(name: "stage", host: "desk", directory: "~/src", reach: .online(.direct))
        ) { _ in }
    }
}
