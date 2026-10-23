import Foundation

/// Turns a frozen capture and what somebody wrote about it into the bundle the
/// cloud stores.
///
/// `report.json` names every part as present or absent-with-a-reason, and the
/// payloads sit beside it under the names it uses. The runtime's dump goes in
/// whole under `dump/`, laid out as `amux dump` lays out a desktop one, so the
/// same reading applies to both.
public enum ReportAssembly {
    /// The layout version this app writes. It is the version the account
    /// service reads; a bundle at any other version is refused rather than
    /// guessed at.
    public static let schemaVersion = 2

    public static let reportFile = "report.json"
    public static let frameFile = "frame.png"
    public static let traceFile = "trace.jsonl"
    /// Where the runtime's dump goes in a report: each of its files under this
    /// directory, by its path inside the dump.
    public static let dumpDirectory = "dump"
    /// Why no log rides beside the dump: the dump already carries the
    /// runtime's log, redacted, and a copy read raw off the file would not
    /// be.
    static let logAbsent = "the runtime's log is in the dump, redacted"

    /// Everything a report is, ready to go.
    ///
    /// `report.json` is built last and first in the list, because what it says
    /// about the other parts has to be what the other parts actually are: the
    /// account service refuses a bundle whose declarations and whose files
    /// disagree, and it is right to.
    public static func bundle(
        from capture: ReportCapture,
        draft: ReportDraft,
        build: String,
        gitSHA: String = "",
        createdAt: Date = Date(),
        dump: Result<[ReportPart], PartAbsent>
    ) -> ReportBundle {
        var parts: [ReportPart] = []
        parts.append(ReportPart(name: frameFile, data: capture.frame.png))
        parts.append(part(
            traceFile, capture.trace.map { Data($0.utf8) },
            absent: capture.traceAbsent ?? "the view-state recording was not captured"))
        let dumped: String?
        switch dump {
        case .success(let files):
            parts += files
            dumped = nil
        case .failure(let absent):
            dumped = absent.why
        }
        let header = self.header(
            capture: capture, draft: draft, build: build, gitSHA: gitSHA,
            createdAt: createdAt, parts: parts, dumpAbsent: dumped)
        return ReportBundle(parts: [ReportPart(name: reportFile, data: header)] + parts)
    }

    /// The files of the runtime's dump, each a part under `dump/`, or why
    /// there are none.
    public static func dumpParts(
        _ dump: Task<Result<URL, PartAbsent>, Never>?
    ) async -> Result<[ReportPart], PartAbsent> {
        guard let dump else {
            return .failure(PartAbsent("nothing was running, so there was nothing to dump"))
        }
        switch await dump.value {
        case .failure(let absent): return .failure(absent)
        case .success(let directory): return files(under: directory)
        }
    }

    static func files(under directory: URL) -> Result<[ReportPart], PartAbsent> {
        let root = directory.standardizedFileURL.path
        guard let walk = FileManager.default.enumerator(
            at: directory, includingPropertiesForKeys: [.isRegularFileKey]) else {
            return .failure(PartAbsent("the dump could not be read"))
        }
        var parts: [ReportPart] = []
        for case let file as URL in walk {
            guard (try? file.resourceValues(forKeys: [.isRegularFileKey]))?.isRegularFile == true,
                  let data = try? Data(contentsOf: file) else { continue }
            let relative = String(file.standardizedFileURL.path.dropFirst(root.count))
                .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            parts.append(ReportPart(name: "\(dumpDirectory)/\(relative)", data: data))
        }
        guard !parts.isEmpty else { return .failure(PartAbsent("the dump was empty")) }
        return .success(parts.sorted { $0.name < $1.name })
    }

    private static func part(_ name: String, _ data: Data?, absent: String) -> ReportPart {
        data.map { ReportPart(name: name, data: $0) }
            ?? ReportPart(name: name, absenceReason: absent)
    }

    /// `report.json`: the small, self-describing entry point.
    ///
    /// Written by hand rather than through `Codable`, because the reader is
    /// the account service, whose field names and enum spellings are the
    /// contract. A synthesised encoding would follow this app's property
    /// names, and the first rename on this side would silently stop producing
    /// bundles the service accepts.
    private static func header(
        capture: ReportCapture, draft: ReportDraft, build: String, gitSHA: String,
        createdAt: Date, parts: [ReportPart], dumpAbsent: String?
    ) -> Data {
        func declared(_ name: String) -> Any {
            guard let part = parts.first(where: { $0.name == name }) else {
                return ["absent": ["reason": "the app did not consider this part"]]
            }
            if part.present { return "present" }
            return ["absent": ["reason": part.absenceReason ?? "not captured"]]
        }

        var declarations: [String: Any] = [
            "frame": declared(frameFile),
            "trace": declared(traceFile),
            "dump": dumpAbsent.map { ["absent": ["reason": $0]] as Any } ?? "present",
            "log": ["absent": ["reason": logAbsent]],
        ]
        // The recorder that made the trace, named only when there is a trace
        // to name it for. What made it decides where it can be replayed: this
        // one came from a native view, so a terminal cannot put it back.
        if parts.first(where: { $0.name == traceFile })?.present == true {
            declarations["trace_kind"] = "native_view"
        }

        let detail = [capture.route, capture.runtimeFailure].compactMap { $0 }
        let header: [String: Any] = [
            "schema_version": schemaVersion,
            "build": build,
            "git_sha": gitSHA,
            "created_at": rfc3339(createdAt),
            "stamp": stamp(createdAt),
            // Everything written on a phone is a bug report. The terminal's
            // flow asks bug-or-tweak first; the phone's starts from a
            // screenshot of something that looked wrong, and asking somebody
            // to classify it before they have said what it is would be a
            // question in front of the answer.
            "kind": "bug",
            "status": "open",
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
            // Nothing has checked this bundle against the screen it was
            // taken from, and the phone says so rather than claiming a
            // verdict it did not reach.
            "replay": "unchecked",
        ]
        return (try? JSONSerialization.data(
            withJSONObject: header, options: [.sortedKeys, .prettyPrinted])) ?? Data("{}".utf8)
    }

    /// The name this capture would have had on disk: when it happened, to the
    /// millisecond, and something to tell two in the same one apart.
    private static func stamp(_ createdAt: Date) -> String {
        let millis = Int((createdAt.timeIntervalSince1970 * 1000).rounded())
        return "\(millis)-\(Int.random(in: 10000..<100000))"
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
