import AmuxApp
import Foundation

// A bare Swift executable linking the driving bridge: it starts the runtime,
// pairs with one served machine by the link that machine printed, and reads
// back the machine and its agents the way the app's stores read them. Nothing
// here is the app; what it proves is that the bridge links, starts and talks
// to a real daemon from Swift on the simulator.

private func fail(_ code: Int, _ message: String) -> NSError {
    NSError(domain: "LoopbackSmoke", code: code, userInfo: [NSLocalizedDescriptionKey: message])
}

/// One callback's answer, waited for on a condition.
private final class Answer: @unchecked Sendable {
    let condition = NSCondition()
    var json: String?

    func wait(_ what: String) throws -> Any {
        condition.lock()
        defer { condition.unlock() }
        let deadline = Date().addingTimeInterval(30)
        while json == nil {
            guard condition.wait(until: deadline) else { throw fail(6, "no answer to \(what) within 30 seconds") }
        }
        return try JSONSerialization.jsonObject(with: Data(json!.utf8), options: [.fragmentsAllowed])
    }
}

/// The runtime wakes its host on every change; this smoke reads snapshots
/// instead, so a wake has nothing to do.
private func woke(_ context: UnsafeMutableRawPointer?, _ chat: UInt64) {}

private func answered(_ context: UnsafeMutableRawPointer?, _ json: UnsafePointer<CChar>?) {
    guard let context, let json else { return }
    let answer = Unmanaged<Answer>.fromOpaque(context).takeRetainedValue()
    answer.condition.lock()
    answer.json = String(cString: json)
    answer.condition.broadcast()
    answer.condition.unlock()
}

/// Calls one bridge entry that answers through a callback and returns what
/// its `Ok` carried, or throws what its `Err` said.
private func ask(_ what: String, _ call: (AmuxCallback, UnsafeMutableRawPointer) -> Void) throws -> Any {
    let answer = Answer()
    call(answered, Unmanaged.passRetained(answer).toOpaque())
    let reply = try answer.wait(what)
    guard let result = reply as? [String: Any], let ok = result["Ok"] else {
        throw fail(7, "\(what) was refused: \(reply)")
    }
    return ok
}

private func read(_ bytes: UnsafeMutablePointer<CChar>?) throws -> Any {
    guard let bytes else { throw fail(4, "the bridge answered nothing") }
    defer { amux_string_free(bytes) }
    return try JSONSerialization.jsonObject(with: Data(String(cString: bytes).utf8), options: [.fragmentsAllowed])
}

private func json(_ value: Any) -> String {
    String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed, .sortedKeys]), as: UTF8.self)
}

/// Reads until `found` answers something, for at most thirty seconds. The
/// runtime wakes its host on every change; polling a snapshot here is bounded
/// test observation, not how the app reads.
private func until<T>(_ what: String, _ found: () throws -> T?) throws -> T {
    let deadline = Date().addingTimeInterval(30)
    while Date() < deadline {
        if let value = try found() { return value }
        Thread.sleep(forTimeInterval: 0.05)
    }
    throw fail(1, "\(what) did not happen within 30 seconds")
}

private func smoke() throws {
    let arguments = CommandLine.arguments
    guard arguments.count == 4 else {
        throw fail(2, "Expected a pairing link, the machine's name and one of its agents' names")
    }
    let (link, machine, agent) = (arguments[1], arguments[2], arguments[3])
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("amux-loopback-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: root) }
    let config: [String: Any] = [
        "data_dir": root.appendingPathComponent("data").path,
        "log_path": root.appendingPathComponent("amux.log").path,
        "device_name": "simulator-loopback",
        "discovery_scope": "loopback-smoke-\(UUID().uuidString)",
        "lan_bind": "127.0.0.1:0",
    ]
    var error: UnsafeMutablePointer<CChar>?
    guard let runtime = json(config).withCString({ amux_runtime_start($0, woke, nil, &error) }) else {
        let reason = error.map { String(cString: $0) } ?? "no reason"
        if let error { amux_string_free(error) }
        throw fail(3, "the runtime did not start: \(reason)")
    }
    defer { amux_runtime_stop(runtime) }
    // A fresh installation has one profile, nobody signed in on it.
    let listed = try read(amux_runtime_profiles(runtime)) as? [[String: Any]] ?? []
    guard let id = listed.first?["id"] as? String else {
        throw fail(3, "the installation lists no profile")
    }
    guard let profile = id.withCString({ amux_profile_open(runtime, $0, woke, nil, &error) }) else {
        let reason = error.map { String(cString: $0) } ?? "no reason"
        if let error { amux_string_free(error) }
        throw fail(3, "the profile did not open: \(reason)")
    }
    defer { amux_profile_close(profile) }

    let pending = try ask("pairing") { callback, context in
        json(["Link": link]).withCString { amux_profile_begin_pair(profile, $0, callback, context) }
    }
    guard let pending = pending as? [String: Any], let token = pending["token"] else {
        throw fail(5, "pairing answered no token: \(pending)")
    }
    guard pending["name"] as? String == machine else {
        throw fail(5, "the link reached \(pending["name"] ?? "nobody"), not \(machine)")
    }
    let paired = try ask("confirming") { callback, context in
        json(token).withCString { amux_profile_confirm_pair(profile, $0, callback, context) }
    }

    let host = try until("\(machine) online and trusted") { () -> [String: Any]? in
        let hosts = try read(amux_fleet_hosts(profile)) as? [[String: Any]] ?? []
        return hosts.first {
            $0["name"] as? String == machine && $0["trusted"] as? Bool == true
                && ($0["reach"] as? [String: Any])?["Online"] != nil
        }
    }
    let via = (host["reach"] as? [String: Any])?["Online"] ?? "none"
    let names = try until("\(agent) in the fleet") { () -> [String]? in
        let view = try read(amux_fleet_view(profile, nil, nil)) as? [String: Any] ?? [:]
        let sections = view["sections"] as? [[String: Any]] ?? []
        let rows = sections.flatMap { $0["rows"] as? [[String: Any]] ?? [] }
        let names = rows.compactMap { ($0["card"] as? [String: Any])?["name"] as? String }
        return names.contains(agent) ? names.sorted() : nil
    }
    print("paired=\(json(paired))")
    print("host=\(json(["name": host["name"]!, "via": via]))")
    print("agents=\(json(names))")
}

do {
    try smoke()
    print("runtime stopped")
} catch {
    FileHandle.standardError.write(Data("\(error.localizedDescription)\n".utf8))
    exit(1)
}
