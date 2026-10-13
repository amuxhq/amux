import Darwin
import Foundation

/// The driving door itself: one JSON request per line in, one JSON reply per
/// line out, over loopback.
///
/// Loopback and not a file or a pasteboard, because the simulator shares the
/// Mac's network stack: a driver on the Mac connects to the port the app
/// prints and needs nothing installed on either side. The port is chosen by
/// the kernel and written to the readiness path, so nothing waits on a fixed
/// port and nothing guesses how long a launch takes.
enum DoorServer {
    /// Opens the door when the launch arguments ask for it, and answers
    /// nothing at all when they do not.
    @MainActor
    static func startIfRequested() {
        let defaults = UserDefaults.standard
        let asked = defaults.string(forKey: Door.readyArgument)
        let port = defaults.string(forKey: Door.portArgument).flatMap { UInt16($0) }
        guard asked != nil || port != nil else { return }
        do {
            let listener = try DoorListener(port: port ?? 0)
            if let asked { try listener.announce(to: asked) }
            listener.serve()
        } catch {
            // A build that was told to open the door and could not is a
            // harness fault, not a screen to explore by hand.
            fatalError("the driving door could not open: \(error)")
        }
    }
}

enum DoorFailure: Error, CustomStringConvertible {
    case socket(Int32)
    case bind(Int32)
    case listen(Int32)
    case address(Int32)

    var description: String {
        switch self {
        case .socket(let code): "socket() failed with errno \(code)"
        case .bind(let code): "bind() failed with errno \(code)"
        case .listen(let code): "listen() failed with errno \(code)"
        case .address(let code): "getsockname() failed with errno \(code)"
        }
    }
}

/// The listening socket and its accept loop. Each connection is read on a
/// thread of its own, so one carrying a long request (a large attachment's
/// bytes) does not hold up a query that arrives meanwhile; every request is
/// still answered on the main actor.
private final class DoorListener: @unchecked Sendable {
    private let descriptor: Int32
    let port: UInt16

    init(port wanted: UInt16) throws {
        let descriptor = socket(AF_INET, SOCK_STREAM, 0)
        guard descriptor >= 0 else { throw DoorFailure.socket(errno) }
        var reuse: Int32 = 1
        setsockopt(
            descriptor, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))

        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        // Port zero unless a launch named one: the kernel picks one that is
        // free, and the readiness file tells the driver which.
        address.sin_port = wanted.bigEndian
        address.sin_addr = in_addr(s_addr: UInt32(0x7f00_0001).bigEndian)
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0 else { throw DoorFailure.bind(errno) }
        guard listen(descriptor, 16) == 0 else { throw DoorFailure.listen(errno) }

        var assigned = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &assigned) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(descriptor, $0, &length)
            }
        }
        guard named == 0 else { throw DoorFailure.address(errno) }
        self.descriptor = descriptor
        port = assigned.sin_port.bigEndian
    }

    /// Writes the readiness file last, so its existence means the socket is
    /// already accepting.
    func announce(to path: String) throws {
        let ready = Door.Ready(port: port, pid: getpid())
        let data = try JSONEncoder().encode(ready)
        try data.write(to: URL(fileURLWithPath: path), options: .atomic)
    }

    func serve() {
        let queue = DispatchQueue(label: "sh.amux.door")
        queue.async { [self] in
            while true {
                let connection = accept(descriptor, nil, nil)
                if connection < 0 {
                    if errno == EINTR { continue }
                    return
                }
                // A driver that hangs up mid-reply must not take the app
                // down with a broken pipe.
                var one: Int32 = 1
                setsockopt(
                    connection, SOL_SOCKET, SO_NOSIGPIPE, &one,
                    socklen_t(MemoryLayout<Int32>.size))
                Thread.detachNewThread { [self] in
                    converse(on: connection)
                    close(connection)
                }
            }
        }
    }

    /// Reads requests a line at a time. Only the bytes that arrived since
    /// the last read are searched for the line's end, so a request of many
    /// megabytes costs its length once rather than once per read.
    private func converse(on connection: Int32) {
        var pending = Data()
        var searched = 0
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let read = buffer.withUnsafeMutableBytes {
                Darwin.read(connection, $0.baseAddress, $0.count)
            }
            if read <= 0 { return }
            pending.append(contentsOf: buffer[0..<read])
            while let newline = pending[(pending.startIndex + searched)...].firstIndex(of: 0x0A) {
                let line = pending.subdata(in: pending.startIndex..<newline)
                pending = pending.subdata(in: pending.index(after: newline)..<pending.endIndex)
                searched = 0
                guard !line.isEmpty else { continue }
                if answer(to: line, on: connection) == .stop { return }
            }
            searched = pending.count
        }
    }

    private enum Continuation { case carryOn, stop }

    private func answer(to line: Data, on connection: Int32) -> Continuation {
        let request: DoorRequest?
        do {
            request = try JSONDecoder().decode(DoorRequest.self, from: line)
        } catch {
            write(.error("unreadable request: \(error)"), to: connection)
            return .carryOn
        }
        guard let request else { return .carryOn }
        write(DoorAnswering.answer(request), to: connection)
        guard case .shutdown = request else { return .carryOn }
        // The reply is in the kernel's hands before the process leaves, so the
        // driver always sees the acknowledgement it asked for.
        close(connection)
        exit(0)
    }

    private func write(_ reply: DoorReply, to connection: Int32) {
        guard var data = try? JSONEncoder().encode(reply) else { return }
        data.append(0x0A)
        data.withUnsafeBytes { bytes in
            var written = 0
            while written < bytes.count {
                let sent = Darwin.write(connection, bytes.baseAddress! + written, bytes.count - written)
                if sent <= 0 { return }
                written += sent
            }
        }
    }
}

/// Carries one request across to the main actor and waits for its answer.
///
/// Each connection's thread blocks here while the app answers on the actor
/// that owns the screen. Requests from two connections share that actor: one
/// whose answer waits (for frames, a pairing) lets the other run meanwhile.
private enum DoorAnswering {
    static func answer(_ request: DoorRequest) -> DoorReply {
        let answered = Answer()
        let done = DispatchSemaphore(value: 0)
        Task { @MainActor in
            answered.reply = await DoorHost.shared.handle(request)
            done.signal()
        }
        done.wait()
        return answered.reply
    }

    private final class Answer: @unchecked Sendable {
        private let lock = NSLock()
        private var value: DoorReply = .error("the door did not answer")

        var reply: DoorReply {
            get { lock.withLock { value } }
            set { lock.withLock { value = newValue } }
        }
    }
}
