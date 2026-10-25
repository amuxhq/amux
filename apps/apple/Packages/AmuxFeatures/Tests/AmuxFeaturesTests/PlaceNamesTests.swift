import XCTest
@testable import AmuxFeatures

/// One way of writing where an agent runs, for every line that has room for
/// only one.
final class PlaceNamesTests: XCTestCase {
    func testTheLocalSuffixIsDropped() {
        XCTAssertEqual(PlaceNames.host("Jordans-MacBook-Pro.local"), "Jordans-MacBook-Pro")
        XCTAssertEqual(PlaceNames.host("studio.LOCAL"), "studio")
        XCTAssertEqual(PlaceNames.host("studio"), "studio")
        XCTAssertEqual(PlaceNames.host(".local"), ".local")
    }

    func testDirectoriesAreWrittenTheWayAShellPromptWritesThem() {
        XCTAssertEqual(PlaceNames.directory("/Users/ada/source/amux"), "~/s/amux")
        XCTAssertEqual(PlaceNames.directory("/home/ada/work/api/server"), "~/w/a/server")
        XCTAssertEqual(PlaceNames.directory("~/src/amux"), "~/s/amux")
        XCTAssertEqual(PlaceNames.directory("/Users/ada"), "~")
        XCTAssertEqual(PlaceNames.directory("/Users/Shared/tally"), "/U/S/tally")
        XCTAssertEqual(PlaceNames.directory("/Users/ada/.config/amux"), "~/.c/amux")
        XCTAssertEqual(PlaceNames.directory("/opt/build/amux"), "/o/b/amux")
        XCTAssertEqual(PlaceNames.directory("amux"), "amux")
    }

    func testAPathReadWholeWritesOnlyItsHomeAsATilde() {
        XCTAssertEqual(PlaceNames.path("/Users/ada/source/amux"), "~/source/amux")
        XCTAssertEqual(PlaceNames.path("/home/ada/work/api"), "~/work/api")
        XCTAssertEqual(PlaceNames.path("/Users/ada"), "~")
        XCTAssertEqual(PlaceNames.path("/Users/Shared/tally"), "/Users/Shared/tally")
        XCTAssertEqual(PlaceNames.path("/opt/build/amux"), "/opt/build/amux")
    }

    func testThePlaceLeadsWithTheDirectory() {
        XCTAssertEqual(
            PlaceNames.place(host: "studio.local", directory: "/Users/ada/source/amux"),
            "~/s/amux · studio")
        XCTAssertEqual(PlaceNames.place(host: nil, directory: "/Users/ada/source/amux"), "~/s/amux")
        XCTAssertEqual(PlaceNames.place(host: "studio", directory: ""), "studio")
    }
}
