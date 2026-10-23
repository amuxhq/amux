import Foundation
import Network

/// What tells the runtime the phone's network changed under it: the
/// system's path monitor in the app, a fake in tests.
///
/// Every path update after the first, whatever its status, is a change:
/// Wi-Fi to cellular, another Wi-Fi network, the network going or coming
/// back. The first only says what the network is when the watch starts.
public protocol NetworkPathWatching: AnyObject {
    /// Calls `onChange`, on any queue, for every change from now on.
    func start(_ onChange: @escaping @Sendable () -> Void)
    /// Calls nothing more until started again.
    func stop()
}

/// The system's path monitor, on a queue of its own.
///
/// A handover arrives as a burst of updates, so they are coalesced: the
/// first update of a burst is told `spacing` later, once, with whatever
/// followed it inside that time.
public final class SystemNetworkWatch: NetworkPathWatching, @unchecked Sendable {
    /// The least time between two changes told.
    public static let spacing: TimeInterval = 0.25

    private let queue = DispatchQueue(label: "sh.amux.network-watch", qos: .utility)
    // Everything below is touched on `queue` only.
    private var monitor: NWPathMonitor?
    private var onChange: (@Sendable () -> Void)?
    /// Which start the monitor's updates belong to; a stop moves it on, so
    /// an update or a coalesced change of an earlier start tells nothing.
    private var generation = 0
    private var seeded = false
    private var pending = false

    public init() {}

    public func start(_ onChange: @escaping @Sendable () -> Void) {
        queue.async { [self] in
            stopped()
            let mine = generation
            self.onChange = onChange
            // A cancelled monitor cannot start again.
            let monitor = NWPathMonitor()
            monitor.pathUpdateHandler = { [weak self] _ in self?.updated(mine) }
            self.monitor = monitor
            monitor.start(queue: queue)
        }
    }

    public func stop() {
        queue.async { [self] in stopped() }
    }

    private func stopped() {
        monitor?.cancel()
        monitor = nil
        onChange = nil
        generation += 1
        seeded = false
        pending = false
    }

    private func updated(_ mine: Int) {
        guard mine == generation else { return }
        guard seeded else {
            seeded = true
            return
        }
        guard !pending else { return }
        pending = true
        queue.asyncAfter(deadline: .now() + Self.spacing) { [weak self] in
            guard let self, mine == generation else { return }
            pending = false
            onChange?()
        }
    }
}

/// A watch a test drives by hand: each `change()` while started is one
/// change, told at once on the caller's thread.
public final class FakeNetworkWatch: NetworkPathWatching, @unchecked Sendable {
    private let lock = NSLock()
    private var onChange: (@Sendable () -> Void)?

    public init() {}

    /// Whether the coordinator is watching.
    public var started: Bool { lock.withLock { onChange != nil } }

    public func start(_ onChange: @escaping @Sendable () -> Void) {
        lock.withLock { self.onChange = onChange }
    }

    public func stop() {
        lock.withLock { onChange = nil }
    }

    /// The network changed; nothing hears it unless the watch is started.
    public func change() {
        let told = lock.withLock { onChange }
        told?()
    }
}
