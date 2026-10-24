import AmuxApp
import Foundation

// A bare Swift executable linking the shipping bridge, run on the simulator
// to prove the packaged framework links, starts and is the shipping build:
// its version carries no driving marker, an installation starts from it,
// and the entry a driving build alone answers is refused.

private func fail(_ code: Int, _ message: String) -> NSError {
    NSError(domain: "LinkageSmoke", code: code, userInfo: [NSLocalizedDescriptionKey: message])
}

/// Steps until the act under `ticket` is answered, and returns its JSON.
/// This thread is the smoke's one queue: it watches the wake descriptor and
/// steps when it is readable, as the app's core queue does.
private func answer(_ runtime: OpaquePointer, _ what: String, _ ticket: UInt64) throws -> Any {
    guard ticket != 0 else { throw fail(6, "\(what) did not start") }
    let deadline = Date().addingTimeInterval(30)
    var watched = pollfd(fd: amux_runtime_wake_fd(runtime), events: Int16(POLLIN), revents: 0)
    while Date() < deadline {
        guard poll(&watched, 1, 50) > 0 else { continue }
        let stepped = try read(amux_runtime_step(runtime)) as? [String: Any] ?? [:]
        for answer in stepped["answers"] as? [[String: Any]] ?? []
        where (answer["ticket"] as? NSNumber)?.uint64Value == ticket {
            let json = answer["json"] as? String ?? "null"
            return try JSONSerialization.jsonObject(with: Data(json.utf8), options: [.fragmentsAllowed])
        }
    }
    throw fail(6, "no answer to \(what) within 30 seconds")
}

/// Steps until a step hands over the profile list, as one does once the
/// registry has been read, and returns it.
private func profiles(_ runtime: OpaquePointer) throws -> [[String: Any]] {
    let deadline = Date().addingTimeInterval(30)
    var watched = pollfd(fd: amux_runtime_wake_fd(runtime), events: Int16(POLLIN), revents: 0)
    while Date() < deadline {
        guard poll(&watched, 1, 50) > 0 else { continue }
        let stepped = try read(amux_runtime_step(runtime)) as? [String: Any] ?? [:]
        if let listed = stepped["profiles"] as? [[String: Any]] { return listed }
    }
    throw fail(3, "no profile list within 30 seconds")
}

private func read(_ bytes: UnsafeMutablePointer<CChar>?) throws -> Any {
    guard let bytes else { throw fail(4, "the bridge answered nothing") }
    defer { amux_string_free(bytes) }
    return try JSONSerialization.jsonObject(with: Data(String(cString: bytes).utf8), options: [.fragmentsAllowed])
}

private func json(_ value: Any) -> String {
    String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed, .sortedKeys]), as: UTF8.self)
}

private func smoke() throws {
    let version = String(cString: amux_version())
    guard !version.isEmpty else { throw fail(1, "the bridge has no version") }
    guard !version.contains("+debug-tools") else { throw fail(1, "the packaged bridge is a driving build: \(version)") }
    print("amux_version=\(version)")

    let root = FileManager.default.temporaryDirectory.appendingPathComponent("amux-linkage-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: root) }
    // Nothing listens: the smoke proves the library, not the network.
    let config: [String: Any] = [
        "data_dir": root.appendingPathComponent("data").path,
        "log_path": root.appendingPathComponent("amux.log").path,
        "device_name": "simulator-linkage",
        "lan": false,
    ]
    var error: UnsafeMutablePointer<CChar>?
    guard let runtime = json(config).withCString({ amux_runtime_start($0, &error) }) else {
        let reason = error.map { String(cString: $0) } ?? "no reason"
        if let error { amux_string_free(error) }
        throw fail(3, "the runtime did not start: \(reason)")
    }
    defer { amux_runtime_stop(runtime) }
    let listed = try profiles(runtime)
    guard let id = listed.first?["id"] as? String else { throw fail(3, "the installation lists no profile") }
    print("installation started by the shipping mobile library")

    // Only a driving build offers a pairing code of its own.
    let ticket = id.withCString { amux_runtime_offer_pairing(runtime, $0) }
    let reply = try answer(runtime, "offering pairing", ticket)
    guard let result = reply as? [String: Any], result["Err"] != nil else {
        throw fail(5, "the shipping bridge offered pairing: \(reply)")
    }
    print("pairing offer refused by the shipping mobile library")
}

do {
    try smoke()
} catch {
    FileHandle.standardError.write(Data("\(error.localizedDescription)\n".utf8))
    exit(Int32((error as NSError).code))
}
