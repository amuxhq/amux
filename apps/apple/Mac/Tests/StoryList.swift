import AmuxMacStories
import Foundation
import XCTest

/// The Mac's stories are the ones `tests/stories.json` gives it, by the
/// list's names and in its order, so the clients name a screen alike.
final class StoryList: XCTestCase {
    func testTheStoriesAreTheListsMacStoriesInItsOrder() throws {
        let list = ScratchDaemon.repository.appending(path: "tests/stories.json")
        let listed = try Stories.listed(in: Data(contentsOf: list))
        XCTAssertEqual(Stories.differences(from: listed), [])
        XCTAssertFalse(listed.isEmpty)
    }
}
