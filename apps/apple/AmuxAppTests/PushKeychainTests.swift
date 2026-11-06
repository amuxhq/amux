import AmuxPush
import Foundation
import XCTest

/// The keychain itself, from inside the app, whose bundle names the group and
/// whose entitlements grant it.
final class PushKeychainTests: XCTestCase {
    func testAKeyKeptIsTheKeyReadBackUntilItIsRemoved() throws {
        let keychain = PushKeychain()
        let host = UUID().uuidString.lowercased()
        let absent = try keychain.key(for: host)
        XCTAssertNil(absent)
        let made = try keychain.makeKey(for: host)
        let read = try keychain.key(for: host.uppercased())
        XCTAssertEqual(read?.rawRepresentation, made.rawRepresentation)
        try keychain.removeKey(for: host)
        let gone = try keychain.key(for: host)
        XCTAssertNil(gone)
        try keychain.removeKey(for: host)
    }
}
