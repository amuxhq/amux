import AmuxCore
import Foundation
import UIKit

/// Photographs the screen and starts the dump a report carries.
///
/// Both at the same moment: the picture is what somebody saw, and the dump
/// is what the runtime held when they saw it, so the two describe one instant
/// rather than the moment the report was finally sent.
@MainActor
final class ReportFreeze: ReportFreezing {
    private let window: () -> UIWindow?
    private let route: () -> String?
    private let dump: () -> Task<Result<URL, PartAbsent>, Never>?
    private let runtimeFailure: () -> String?

    init(
        window: @escaping () -> UIWindow? = { ReportFreeze.foreground },
        route: @escaping () -> String? = { nil },
        dump: @escaping () -> Task<Result<URL, PartAbsent>, Never>? = { nil },
        runtimeFailure: @escaping () -> String? = { nil }
    ) {
        self.window = window
        self.route = route
        self.dump = dump
        self.runtimeFailure = runtimeFailure
    }

    func freeze() -> ReportCapture? {
        guard let window = window(), let frame = Self.photograph(window) else { return nil }
        return ReportCapture(
            frame: frame, dump: dump(), route: route(), runtimeFailure: runtimeFailure())
    }

    static var foreground: UIWindow? {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .first { $0.activationState == .foregroundActive || $0.activationState == .foregroundInactive }?
            .windows.first { $0.isKeyWindow }
    }

    /// The window as drawn, without the status bar: the report is about the
    /// app, and the clock and battery above it say nothing about the app.
    private static func photograph(_ window: UIWindow) -> FrozenFrame? {
        let bounds = window.bounds
        guard bounds.width > 0, bounds.height > 0 else { return nil }
        let format = UIGraphicsImageRendererFormat()
        format.scale = window.screen.scale
        format.opaque = true
        let image = UIGraphicsImageRenderer(bounds: bounds, format: format).image { _ in
            window.drawHierarchy(in: bounds, afterScreenUpdates: false)
        }
        guard let png = image.pngData() else { return nil }
        return FrozenFrame(
            png: png, width: bounds.width, height: bounds.height,
            scale: Double(format.scale))
    }
}
