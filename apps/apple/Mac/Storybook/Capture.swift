import AmuxMacStories
import AppKit
import Foundation

/// Draws stories to PNG files with nothing on the desktop: each in a real
/// window past the edge of every display, photographed by the window server.
/// Drawing a view into a bitmap leaves its glass and sidebars blank, so only
/// the window server's picture shows the window as it really is.
@MainActor
enum Capture {
    private typealias WindowImage = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?

    /// The deprecated window photograph, looked up at run time because the
    /// SDK hides it from Swift. It takes this process's own windows without
    /// any permission prompt, which an unattended run needs.
    private static let windowImage: WindowImage = unsafeBitCast(
        dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage"), to: WindowImage.self)

    private static let includingWindow: UInt32 = 1 << 3
    private static let ignoringFraming: UInt32 = 1 << 0
    private static let bestResolution: UInt32 = 1 << 3

    private static let appearances: [(name: String, appearance: NSAppearance.Name)] = [
        ("dark", .darkAqua), ("light", .aqua),
    ]

    /// How many pixels a story must differ by from an empty window.
    private static let drawnPixels = 200

    /// Draws every story whose name holds `only` into `out`, with an
    /// `index.md` saying what each shows. Answers the process's exit code.
    static func run(out: URL, only: String?, listed: [ListedStory]) async -> Int32 {
        NSApplication.shared.setActivationPolicy(.prohibited)
        try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
        let stories = Stories.all.filter { story in only.map { story.name.contains($0) } ?? true }
        guard !stories.isEmpty else {
            print("no story's name contains \(only ?? "")")
            return 1
        }
        var failed: [String] = []
        var index = "# Mac storybook\n\n"
        var empty: [String: CGImage] = [:]
        for story in stories {
            let shows = listed.first { $0.name == story.name }?.shows ?? ""
            index += "## \(story.name)\n\n\(shows)\n\n"
            let sizes: [StorySize] = story.window == .settings ? [.regular] : StorySize.allCases
            for size in sizes {
                for (name, appearance) in appearances {
                    let file = story.window == .settings
                        ? "\(story.name).\(name).png" : "\(story.name).\(size.rawValue).\(name).png"
                    let key = "\(story.window)-\(size)-\(name)"
                    if empty[key] == nil {
                        empty[key] = await photograph(nil, story: story, size: size, appearance: appearance)
                    }
                    guard let image = await photograph(story, story: story, size: size, appearance: appearance)
                    else {
                        failed.append("\(file): no picture of the window")
                        continue
                    }
                    if let blank = empty[key], differing(image, blank) < drawnPixels {
                        failed.append("\(file): the window is empty")
                    }
                    write(image, to: out.appending(path: file))
                    index += "![\(file)](\(file))\n\n"
                }
            }
            print("drew \(story.name)")
        }
        try? index.write(to: out.appending(path: "index.md"), atomically: true, encoding: .utf8)
        for failure in failed { print("FAILED \(failure)") }
        return failed.isEmpty ? 0 : 1
    }

    /// A photograph of `drawn` in its window once it has stopped changing,
    /// or of an empty window of `story`'s style when `drawn` is nil.
    private static func photograph(
        _ drawn: Story?, story: Story, size: StorySize, appearance: NSAppearance.Name
    ) async -> CGImage? {
        let stage = StageWindow()
        stage.offDisplay = true
        defer {
            stage.contentViewController = nil
            stage.orderOut(nil)
            stage.close()
        }
        stage.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        stage.appearance = NSAppearance(named: appearance)
        if let drawn {
            stage.show(drawn, size: size)
        } else {
            stage.show(story, size: size)
            stage.contentViewController = nil
            stage.contentView = NSView()
        }
        stage.orderBack(nil)
        return await settled(stage)
    }

    /// Glass finishes on the window server's schedule, so a picture counts
    /// once it has held for half a second. A spinner never holds, and takes
    /// whatever three seconds leave.
    private static func settled(_ window: NSWindow) async -> CGImage? {
        try? await Task.sleep(for: .milliseconds(400))
        let deadline = ContinuousClock.now + .seconds(3)
        var last = image(of: window)
        var held = 0
        while held < 5, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(100))
            let now = image(of: window)
            held = same(last, now) ? held + 1 : 0
            last = now
        }
        return last
    }

    private static func image(of window: NSWindow) -> CGImage? {
        windowImage(.null, includingWindow, UInt32(window.windowNumber), ignoringFraming | bestResolution)?
            .takeRetainedValue()
    }

    private static func same(_ one: CGImage?, _ other: CGImage?) -> Bool {
        guard let one, let other, let a = one.dataProvider?.data, let b = other.dataProvider?.data else {
            return false
        }
        return one.width == other.width && one.height == other.height && a as Data == b as Data
    }

    private static func differing(_ one: CGImage, _ other: CGImage) -> Int {
        guard one.width == other.width, one.height == other.height, one.bytesPerRow == other.bytesPerRow,
            let a = one.dataProvider?.data as Data?, let b = other.dataProvider?.data as Data?, a.count == b.count
        else { return Int.max }
        let step = one.bitsPerPixel / 8
        return stride(from: 0, to: a.count - step, by: step).reduce(0) { count, at in
            count + (a[at..<(at + step)] == b[at..<(at + step)] ? 0 : 1)
        }
    }

    private static func write(_ image: CGImage, to url: URL) {
        try? NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])?.write(to: url)
    }
}
