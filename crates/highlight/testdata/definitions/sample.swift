import Foundation

let maxBlobs = 1024

/// A store of blobs, by name.
final class Store {
    static let limit = maxBlobs
    private var blobs: [String: Data] = [:]

    init(root: URL) {
        self.blobs = [:]
    }

    func put(_ name: String, blob: Data) throws {
        let check = { (n: Int) in n < Store.limit }
        guard check(blobs.count) else { throw StoreError.full }
        blobs[name] = blob
    }

    struct Entry {
        var name: String

        func describe() -> String {
            "Entry \(name)"
        }
    }
}

enum StoreError: Error {
    case full
    case missing(String)
}

protocol Backend {
    func read(_ name: String) -> Data?
}

extension Store: Backend {
    func read(_ name: String) -> Data? {
        blobs[name]
    }
}

actor Cache {
    func warm() {}
}

typealias Blobs = [String: Data]

func openStore(at root: URL) -> Store {
    Store(root: root)
}
