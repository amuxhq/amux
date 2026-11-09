import AmuxMacStories
import AppKit
import Observation
import SwiftUI

/// Where the storybook was left, kept between runs.
struct Place: Codable {
    var story: String?
    var appearance: BookAppearance = .system
    var size: StorySize = .regular
}

enum BookAppearance: String, CaseIterable, Codable {
    case system
    case dark
    case light

    var label: String { rawValue.capitalized }

    var appearance: NSAppearance? {
        switch self {
        case .system: nil
        case .dark: NSAppearance(named: .darkAqua)
        case .light: NSAppearance(named: .aqua)
        }
    }
}

/// The stories, which one is on the stage and how it is drawn.
@MainActor
@Observable
final class Book {
    let stories = Stories.all
    let shows: [String: String]
    var query = ""
    /// What the rebuild last said; nil while the app on show is current.
    var status: String?
    var current: String { didSet { changed() } }
    var appearance: BookAppearance { didSet { changed() } }
    var size: StorySize { didSet { changed() } }
    @ObservationIgnored var onChange: () -> Void = {}

    init(place: Place, opening: String?, listed: [ListedStory]) {
        shows = Dictionary(uniqueKeysWithValues: listed.map { ($0.name, $0.shows) })
        let names = Stories.all.map(\.name)
        current = [opening, place.story].compactMap(\.self).first(where: names.contains) ?? names[0]
        appearance = place.appearance
        size = place.size
    }

    var visible: [Story] {
        guard !query.isEmpty else { return stories }
        return stories.filter {
            $0.name.localizedCaseInsensitiveContains(query)
                || (shows[$0.name] ?? "").localizedCaseInsensitiveContains(query)
        }
    }

    var story: Story { stories.first { $0.name == current } ?? stories[0] }

    var place: Place { Place(story: current, appearance: appearance, size: size) }

    func step(by offset: Int) {
        let shown = visible
        guard let at = shown.firstIndex(where: { $0.name == current }) else {
            if let first = shown.first { current = first.name }
            return
        }
        current = shown[(at + offset + shown.count) % shown.count].name
    }

    func cycleAppearance() { appearance = next(after: appearance) }
    func cycleSize() { size = next(after: size) }

    private func next<T: CaseIterable & Equatable>(after value: T) -> T {
        let all = Array(T.allCases)
        return all[(all.firstIndex(of: value)! + 1) % all.count]
    }

    private func changed() { onChange() }
}

/// The window that picks what the stage shows.
struct BookPanel: View {
    @Bindable var book: Book

    var body: some View {
        VStack(spacing: 0) {
            if let status = book.status {
                Text(status)
                    .font(.callout)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .background(.orange.opacity(0.25))
            }
            TextField("Filter", text: $book.query)
                .textFieldStyle(.roundedBorder)
                .padding(10)
            List(selection: selection) {
                ForEach(groups, id: \.self) { group in
                    Section(group) {
                        ForEach(book.visible.filter { $0.group == group }) { story in
                            Text(story.name).tag(story.name)
                        }
                    }
                }
            }
            .listStyle(.sidebar)
            Divider()
            VStack(alignment: .leading, spacing: 10) {
                Text(book.shows[book.current] ?? "")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Picker("Appearance", selection: $book.appearance) {
                    ForEach(BookAppearance.allCases, id: \.self) { Text($0.label) }
                }
                .pickerStyle(.segmented)
                Picker("Size", selection: $book.size) {
                    ForEach(StorySize.allCases, id: \.self) { Text($0.rawValue.capitalized) }
                }
                .pickerStyle(.segmented)
            }
            .padding(10)
        }
    }

    private var selection: Binding<String?> {
        Binding(get: { book.current }, set: { if let name = $0 { book.current = name } })
    }

    private var groups: [String] {
        var seen = Set<String>()
        return book.visible.map(\.group).filter { seen.insert($0).inserted }
    }
}

/// Runs the two windows: the stage, drawn as the app's own window is, and
/// the panel beside it, so nothing of the storybook is in the story's window.
@MainActor
final class Viewer: NSObject, NSApplicationDelegate {
    private let book: Book
    private let placeFile: URL
    private let statusFile: URL
    private let background: Bool
    private let stage = StageWindow()
    private var panel: NSWindow?
    private var poll: Timer?

    init(book: Book, placeFile: URL, statusFile: URL, background: Bool) {
        self.book = book
        self.placeFile = placeFile
        self.statusFile = statusFile
        self.background = background
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        menu()
        let panel = NSWindow(
            contentRect: CGRect(x: 0, y: 0, width: 300, height: 640),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        panel.title = "Stories"
        panel.isReleasedWhenClosed = false
        // Left to size itself, the window grows to hold every row rather than
        // letting the list scroll.
        let host = NSHostingController(rootView: BookPanel(book: book))
        host.sizingOptions = []
        panel.contentViewController = host
        panel.setContentSize(CGSize(width: 300, height: 640))
        panel.contentMinSize = CGSize(width: 260, height: 320)
        panel.contentMaxSize = CGSize(width: 420, height: 640)
        place(panel, name: "Stories")
        self.panel = panel
        place(stage, name: "Stage")
        book.onChange = { [weak self] in self?.draw() }
        draw()
        for window in [stage, panel] {
            // A rebuild comes back without covering what the person is
            // working in.
            if background { window.orderBack(nil) } else { window.orderFront(nil) }
        }
        poll = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.readStatus() }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    private func place(_ window: NSWindow, name: String) {
        if !window.setFrameUsingName(name) { window.center() }
        window.setFrameAutosaveName(name)
        keepWithinLimits(window)
    }

    /// Setting a frame does not apply a window's limits, and a saved frame can
    /// be from before them or from a larger display.
    private func keepWithinLimits(_ window: NSWindow) {
        let content = window.contentRect(forFrameRect: window.frame).size
        let width = min(max(content.width, window.contentMinSize.width), window.contentMaxSize.width)
        let height = min(max(content.height, window.contentMinSize.height), window.contentMaxSize.height)
        let top = window.frame.maxY
        window.setContentSize(CGSize(width: width, height: height))
        var left = window.frame.minX
        if let display = window.screen ?? NSScreen.main {
            left = min(left, display.visibleFrame.maxX - window.frame.width)
        }
        window.setFrameTopLeftPoint(NSPoint(x: left, y: top))
    }

    private func draw() {
        stage.appearance = book.appearance.appearance
        stage.show(book.story, size: book.size)
        if let data = try? JSONEncoder().encode(book.place) { try? data.write(to: placeFile) }
    }

    private func readStatus() {
        let text = (try? String(contentsOf: statusFile, encoding: .utf8))?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let status = text.isEmpty ? nil : text
        if status != book.status { book.status = status }
    }

    private func menu() {
        let main = NSMenu()
        let app = NSMenuItem()
        app.submenu = NSMenu()
        app.submenu?.addItem(withTitle: "Quit Storybook", action: #selector(NSApplication.terminate), keyEquivalent: "q")
        main.addItem(app)
        let view = NSMenuItem()
        let items = NSMenu(title: "Stories")
        for (title, action, key) in [
            ("Previous Story", #selector(previous), "["), ("Next Story", #selector(next), "]"),
            ("Cycle Appearance", #selector(cycleAppearance), "t"), ("Cycle Size", #selector(cycleSize), "s"),
        ] as [(String, Selector, String)] {
            items.addItem(withTitle: title, action: action, keyEquivalent: key).target = self
        }
        view.submenu = items
        main.addItem(view)
        NSApp.mainMenu = main
    }

    @objc private func previous() { book.step(by: -1) }
    @objc private func next() { book.step(by: 1) }
    @objc private func cycleAppearance() { book.cycleAppearance() }
    @objc private func cycleSize() { book.cycleSize() }
}
