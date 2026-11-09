import AmuxMacStories
import AppKit

/// The Mac app's storybook: its screens by name, drawn from values with no
/// daemon. `--capture DIR` draws them to files and exits; otherwise it opens
/// the viewer. `just mac storybook` is how either is started.
@main
enum Storybook {
    @MainActor
    static func main() {
        let arguments = CommandLine.arguments
        func value(_ name: String) -> String? {
            arguments.firstIndex(of: name).flatMap { $0 + 1 < arguments.count ? arguments[$0 + 1] : nil }
        }
        guard let list = value("--list").map({ URL(filePath: $0) }),
            let data = try? Data(contentsOf: list), let listed = try? Stories.listed(in: data)
        else {
            print("--list must name the storybook list, tests/stories.json")
            exit(2)
        }
        let differences = Stories.differences(from: listed)
        if let out = value("--capture") {
            guard differences.isEmpty else {
                differences.forEach { print("FAILED \($0)") }
                exit(1)
            }
            Task { @MainActor in
                exit(await Capture.run(out: URL(filePath: out), only: value("--only"), listed: listed))
            }
            RunLoop.main.run()
            return
        }
        guard let placeFile = value("--place").map({ URL(filePath: $0) }),
            let statusFile = value("--status").map({ URL(filePath: $0) })
        else {
            print("--place and --status must name files")
            exit(2)
        }
        let place = (try? Data(contentsOf: placeFile))
            .flatMap { try? JSONDecoder().decode(Place.self, from: $0) } ?? Place()
        let book = Book(place: place, opening: value("--story"), listed: listed)
        book.status = differences.isEmpty ? nil : differences.joined(separator: "\n")
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        let viewer = Viewer(
            book: book, placeFile: placeFile, statusFile: statusFile,
            background: arguments.contains("--background"))
        app.delegate = viewer
        withExtendedLifetime(viewer) { app.run() }
    }
}
