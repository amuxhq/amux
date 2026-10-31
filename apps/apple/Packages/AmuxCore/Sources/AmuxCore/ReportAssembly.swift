import Foundation

/// Turns a frozen capture and what somebody wrote about it into what the
/// runtime sends.
///
/// The phone writes `report.json` and hands over the frame;
/// the runtime writes them into a report beside the dump it took when the
/// screen froze, adds what only it knows (the schema, the dump's part), and
/// uploads it the same way the terminal's report and a check's dump go.
public enum ReportAssembly {
    public static let reportFile = "report.json"
    public static let frameFile = "frame.png"

    /// Everything the phone contributes to a report, ready to hand over.
    public static func upload(
        from capture: ReportCapture,
        draft: ReportDraft,
        build: String,
        gitSHA: String = "",
        createdAt: Date = Date(),
        dump: Result<URL, PartAbsent>
    ) -> ReportUpload {
        let path: String
        let dumpAbsent: String?
        switch dump {
        case .success(let directory):
            path = directory.path
            dumpAbsent = nil
        case .failure(let absent):
            path = ""
            dumpAbsent = absent.why
        }
        return ReportUpload(
            header: header(
                capture: capture, draft: draft, build: build, gitSHA: gitSHA,
                createdAt: createdAt, dumpAbsent: dumpAbsent),
            frame: capture.frame.png, path: path)
    }

    /// Where the runtime's dump is, or why there is none.
    public static func dumped(
        _ dump: Task<Result<URL, PartAbsent>, Never>?
    ) async -> Result<URL, PartAbsent> {
        guard let dump else {
            return .failure(PartAbsent("nothing was running, so there was nothing to dump"))
        }
        return await dump.value
    }

    /// `report.json`: the small, self-describing entry point.
    ///
    /// Written by hand rather than through `Codable`, because the readers are
    /// the runtime and the account service, whose field names and enum
    /// spellings are the contract. A synthesised encoding would follow this
    /// app's property names, and the first rename on this side would silently
    /// stop producing reports the service accepts.
    private static func header(
        capture: ReportCapture, draft: ReportDraft, build: String, gitSHA: String,
        createdAt: Date, dumpAbsent: String?
    ) -> Data {
        func absent(_ reason: String) -> Any { ["absent": ["reason": reason]] }
        // The account service asks every report to say whether it carries a
        // trace; the phone records none.
        var declarations: [String: Any] = [
            "frame": "present",
            "trace": absent("the phone records no view trace"),
        ]
        // The runtime declares a dump it holds; one it was never given is the
        // phone's to explain.
        if let dumpAbsent { declarations["dump"] = absent(dumpAbsent) }

        let detail = [capture.route, capture.runtimeFailure].compactMap { $0 }
        let header: [String: Any] = [
            "build": build,
            "git_sha": gitSHA,
            "created_at": rfc3339(createdAt),
            "detail": detail.isEmpty ? NSNull() : detail.joined(separator: "\n") as Any,
            "note": draft.note,
            "marks": draft.marks.map {
                ["x": $0.x, "y": $0.y, "width": $0.width, "height": $0.height, "note": $0.note]
            },
            // A terminal's viewport is cells. This frame has none: how big it
            // was is `image_frame`, in the points its rectangles are measured
            // in, with the scale its pixels were drawn at.
            "viewport": NSNull(),
            "image_frame": [
                "width_pt": capture.frame.width,
                "height_pt": capture.frame.height,
                "scale": Int(capture.frame.scale.rounded()),
            ],
            "parts": declarations,
        ]
        return (try? JSONSerialization.data(
            withJSONObject: header, options: [.sortedKeys, .prettyPrinted])) ?? Data("{}".utf8)
    }

    /// The instant, written the way the account service reads it: UTC, to the
    /// millisecond. Built per call rather than kept, because a formatter is
    /// mutable and one shared across threads is a data race waiting for a
    /// second report.
    private static func rfc3339(_ instant: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter.string(from: instant)
    }
}
