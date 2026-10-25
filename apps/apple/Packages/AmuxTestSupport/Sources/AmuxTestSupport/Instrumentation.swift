import Darwin
import Foundation
import QuartzCore
import UIKit

/// Whether the device is in a state to be measured: the system's thermal
/// state as it names it (`nominal`, `fair`, `serious`, `critical`) and
/// whether Low Power Mode is on, which slows the CPU and holds back work.
public struct DeviceCondition: Codable, Sendable, Equatable {
    public let thermal: String
    public let lowPower: Bool

    public init(thermal: String, lowPower: Bool) {
        self.thermal = thermal
        self.lowPower = lowPower
    }

    public static func now() -> DeviceCondition {
        let process = ProcessInfo.processInfo
        let thermal = switch process.thermalState {
        case .nominal: "nominal"
        case .fair: "fair"
        case .serious: "serious"
        case .critical: "critical"
        @unknown default: "unknown"
        }
        return DeviceCondition(thermal: thermal, lowPower: process.isLowPowerModeEnabled)
    }
}

/// What the door measures over a stretch of time, for the performance
/// suite: how the display kept up, what the main thread cost, what the
/// process holds, and whether the app asked the display for anything while
/// it was supposed to be idle.
public struct Measurement: Codable, Sendable, Equatable {
    /// How long the measurement ran.
    public let seconds: Double
    /// Display refreshes the watch saw.
    public let frames: Int
    /// Milliseconds of missed frame time per second of running: display-link
    /// accounting, a proxy on the simulator for a device's hitch metric.
    public let hitchMsPerS: Double
    /// Percent of one core the main thread used, averaged over the stretch.
    public let mainThreadCpuPercent: Double
    /// The process's footprint at the end, in megabytes.
    public let footprintMB: Double
    /// Display refreshes the app asked for through its own tick, not the
    /// watch's; an idle app asks for none.
    public let idleTicks: Int
    /// Rows a chat on screen took from the runtime; an idle chat takes none.
    public let transcriptCommits: Int

    public init(
        seconds: Double, frames: Int, hitchMsPerS: Double, mainThreadCpuPercent: Double,
        footprintMB: Double, idleTicks: Int, transcriptCommits: Int
    ) {
        self.seconds = seconds
        self.frames = frames
        self.hitchMsPerS = hitchMsPerS
        self.mainThreadCpuPercent = mainThreadCpuPercent
        self.footprintMB = footprintMB
        self.idleTicks = idleTicks
        self.transcriptCommits = transcriptCommits
    }
}

/// Missed-frame accounting over a stretch of time.
///
/// A hitch is a frame that took longer than the display gave it. Summing the
/// overruns and dividing by how long the run lasted gives the milliseconds of
/// hitch per second the streaming budget is written in. On a device this is
/// `XCTHitchMetric`'s job; on the simulator it is a proxy, because the frames
/// are composited by the Mac's display and not the phone's.
@MainActor
public final class FrameWatch {
    private var link: CADisplayLink?
    private var previous: CFTimeInterval?
    private var hitchSeconds: CFTimeInterval = 0
    private var startedAt: CFTimeInterval = 0
    public private(set) var frames = 0

    public init() {}

    public func start() {
        hitchSeconds = 0
        frames = 0
        previous = nil
        startedAt = CACurrentMediaTime()
        let link = CADisplayLink(target: self, selector: #selector(fired))
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    /// Milliseconds of hitch per second of running.
    @discardableResult
    public func stop() -> Double {
        link?.invalidate()
        link = nil
        let elapsed = max(CACurrentMediaTime() - startedAt, 0.001)
        return hitchSeconds * 1_000 / elapsed
    }

    @objc private func fired(_ link: CADisplayLink) {
        // A newly registered display link may first report the timestamp of a
        // frame presented before `start()`. Comparing the next callback with
        // that stale frame invents a missed interval at the beginning of the
        // measurement. Establish the first post-start frame as the origin.
        guard link.timestamp >= startedAt else {
            previous = nil
            return
        }
        frames += 1
        let expected = max(link.targetTimestamp - link.timestamp, 0.001)
        if let previous {
            let actual = link.timestamp - previous
            if actual > expected {
                hitchSeconds += actual - expected
            }
        }
        previous = link.timestamp
    }
}

/// How much of one core the main thread used between two readings.
public struct CPUWatch: Sendable {
    private let thread: CFTimeInterval
    private let wall: CFTimeInterval

    /// Taken on the main thread, so the reading is the main thread's own.
    public init() {
        thread = CPUWatch.callingThreadSeconds()
        wall = CACurrentMediaTime()
    }

    /// Percent of one core, averaged over the interval; read on the same
    /// thread the watch was made on.
    public func percent() -> Double {
        let used = CPUWatch.callingThreadSeconds() - thread
        let elapsed = max(CACurrentMediaTime() - wall, 0.001)
        return used / elapsed * 100
    }

    /// The calling thread's user plus system time.
    private static func callingThreadSeconds() -> CFTimeInterval {
        var info = thread_basic_info()
        // The C macro that names this size is not imported into Swift.
        var count = mach_msg_type_number_t(
            MemoryLayout<thread_basic_info_data_t>.size / MemoryLayout<integer_t>.size)
        let read = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                thread_info(mach_thread_self(), thread_flavor_t(THREAD_BASIC_INFO), $0, &count)
            }
        }
        guard read == KERN_SUCCESS else { return 0 }
        let user = Double(info.user_time.seconds) + Double(info.user_time.microseconds) / 1e6
        let system = Double(info.system_time.seconds) + Double(info.system_time.microseconds) / 1e6
        return user + system
    }
}

/// What the process is holding, as the system accounts for it: the number a
/// jetsam decision is made on, not the resident size.
public enum Footprint {
    public static func megabytes() -> Double {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(
            MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
        let read = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        guard read == KERN_SUCCESS else { return 0 }
        return Double(info.phys_footprint) / 1_048_576
    }
}
