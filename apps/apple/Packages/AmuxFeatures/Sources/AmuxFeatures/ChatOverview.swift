import AmuxCore
import AmuxDesign
import SwiftUI

/// The agent's own page, as a chat app's contact page is the person's: its
/// name and where it runs, then what runs around the chat, as the terminal's
/// overview pane lists it: the background jobs, the changed files by folder for the chosen comparison,
/// the tool servers that failed and the usage windows near a limit. The
/// task list is not repeated here; it docks above the composer.
///
/// Opened by tapping the chat's title, or from its menu. It asks for the changed
/// files while it is on screen, and again when the comparison or the
/// agent's totals move.
public struct ChatOverview: View {
    @Environment(\.design) private var design
    let model: ChatModel
    let subject: ChatSubject
    let now: Date
    /// The review page for the chosen comparison.
    let review: () -> Void

    public init(model: ChatModel, subject: ChatSubject, now: Date, review: @escaping () -> Void) {
        self.model = model
        self.subject = subject
        self.now = now
        self.review = review
    }

    public var body: some View {
        let overview = model.overview
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                place
                if let overview, !overview.jobs.isEmpty { background(overview.jobs) }
                changes(overview?.changes)
                if let overview, !overview.failedServers.isEmpty { servers(overview.failedServers) }
                if let usage = overview?.usageNearLimit { self.usage(usage) }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .onAppear { model.showOverview(true) }
        .onDisappear { model.showOverview(false) }
        .identified("chat.overview")
    }

    // MARK: Where it runs

    /// The name, and the host, folder and address in full: the title pill
    /// shortens all three to fit the bar. The branch is said only here: a
    /// fleet row has no room for it beside the name it would crowd.
    private var place: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            Text(subject.name)
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: Space.m, verticalSpacing: Space.s) {
                if !subject.host.isEmpty { fact(String(localized: "Host"), subject.host) }
                if !subject.directory.isEmpty { fact(String(localized: "Directory"), subject.directory) }
                if let branch = subject.branch, !branch.isEmpty {
                    fact(String(localized: "Branch"), branch, identifier: "chat.overview.branch")
                }
                fact(String(localized: "Address"), subject.address)
            }
        }
        .identified("chat.place", label: subject.address)
    }

    private func fact(_ label: String, _ value: String, identifier: String? = nil) -> some View {
        SwiftUI.GridRow {
            Text(label)
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
            let shown = Text(value)
                .designFont(.monoSmall, design)
                .foregroundStyle(design.inkMuted.color)
                .textSelection(.enabled)
            if let identifier {
                shown.identified(identifier, label: label, value: value)
            } else {
                shown
            }
        }
    }

    // MARK: Background

    private func background(_ jobs: [JobRow]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHead(title: String(localized: "Background"), trailing: "\(jobs.count)")
            ForEach(Array(jobs.enumerated()), id: \.offset) { index, job in
                let ran = ChatWords.ran(since: job.startedAtMs, now: now)
                HStack(spacing: 10) {
                    Text(verbatim: ChatWords.firstLine(job.command))
                        .designFont(.mono, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Spacer(minLength: 6)
                    Text(verbatim: ran)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkFaint.color)
                        // How long a job has run moves with the clock.
                        .reported("chat.overview.job.\(index).age.volatile")
                }
                .accessibilityElement(children: .combine)
                .identified("chat.overview.job.\(index)", label: job.command, value: ran)
            }
        }
    }

    // MARK: Changes

    @ViewBuilder
    private func changes(_ changes: Changes?) -> some View {
        let comparisons = model.comparisons
        let totals = model.changes
        VStack(alignment: .leading, spacing: 10) {
            SectionHead(
                title: String(localized: "Changes"),
                // What is listed, once it is; until then the totals the
                // frame carries.
                trailing: (changes?.totals ?? totals).map { ChatWords.files(Int($0.files)) })
            if comparisons.count > 1 {
                Picker(
                    String(localized: "Compare"),
                    selection: Binding(get: { model.comparison }, set: { model.compare($0) })
                ) {
                    ForEach(comparisons, id: \.self) { comparison in
                        Text(ChatWords.comparison(comparison, base: model.base)).tag(comparison)
                    }
                }
                .pickerStyle(.segmented)
                .identified("chat.overview.comparison", value: model.comparison.rawValue)
            }
            if let changes, changes.totals.files > 0 {
                ForEach(Array(changes.folders.enumerated()), id: \.offset) { _, folder in
                    self.folder(folder)
                }
                Button(action: review) {
                    Text("Review changes")
                        .designFont(.bodyEmphasis, design)
                        .foregroundStyle(design.accent.color)
                        .frame(minHeight: 44)
                }
                .buttonStyle(.amuxControl)
                .identified("chat.overview.review", label: String(localized: "Review changes"))
            } else {
                // A listing that came back empty is the answer, even while
                // the last turn's totals still count files the folder has
                // since dropped.
                Text(changes == nil && totals != nil
                    ? String(localized: "Listing the changed files…") : String(localized: "Nothing changed"))
                    .designFont(.detail, design)
                    .foregroundStyle(design.inkFaint.color)
                    .identified("chat.overview.changes.none")
            }
        }
    }

    /// A folder's path, then its files under it; files at the root stand
    /// alone.
    private func folder(_ folder: Folder) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if !folder.path.isEmpty {
                Text(verbatim: folder.path)
                    .designFont(.monoSmall, design)
                    .foregroundStyle(design.inkFaint.color)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            ForEach(folder.files, id: \.path) { file in
                HStack(spacing: 10) {
                    Text(verbatim: file.name)
                        .designFont(.mono, design)
                        .foregroundStyle(file.status == .deleted ? design.inkFaint.color : design.ink.color)
                        .strikethrough(file.status == .deleted)
                        .lineLimit(1)
                        .truncationMode(.head)
                    Spacer(minLength: 6)
                    HStack(spacing: 5) {
                        if file.added > 0 {
                            Text(verbatim: "+\(file.added)").foregroundStyle(design.added.color)
                        }
                        if file.removed > 0 {
                            Text(verbatim: "\u{2212}\(file.removed)").foregroundStyle(design.removed.color)
                        }
                    }
                    .designFont(.monoSmall, design)
                }
                .padding(.leading, folder.path.isEmpty ? 0 : 14)
                .accessibilityElement(children: .combine)
                .identified(
                    "chat.overview.file.\(file.path)", label: file.path,
                    value: ChatWords.counts(added: file.added, removed: file.removed))
            }
        }
    }

    // MARK: Tool servers

    private func servers(_ servers: [ServerView]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHead(title: String(localized: "Tool servers"), trailing: "\(servers.count)")
            ForEach(servers, id: \.name) { server in
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: ChatWords.failed(server))
                        .designFont(.body, design)
                        .foregroundStyle(design.accent.color)
                    if !server.error.isEmpty {
                        Text(verbatim: ChatWords.firstLine(server.error))
                            .designFont(.caption, design)
                            .foregroundStyle(design.inkFaint.color)
                            .lineLimit(2)
                    }
                }
                .accessibilityElement(children: .combine)
                .identified("chat.overview.server.\(server.name)", label: ChatWords.failed(server))
            }
        }
    }

    // MARK: Usage

    private func usage(_ usage: UsageView) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionHead(
                title: String(localized: "Usage"),
                trailing: usage.blocked ? String(localized: "limit reached") : String(localized: "near a limit"))
            ForEach(Array(usage.windows.enumerated()), id: \.offset) { index, window in
                let label = ChatWords.usageLabel(window.label)
                let detail = ChatWords.usageDetail(window, now: now)
                let loud = window.state == .blocked || window.state == .nearLimit
                VStack(alignment: .leading, spacing: 2) {
                    HStack {
                        Text(verbatim: label)
                        Spacer(minLength: 6)
                        Text(verbatim: ChatWords.used(window))
                    }
                    .designFont(.body, design)
                    .foregroundStyle(window.state == .blocked ? design.accent.color
                                     : loud ? design.ink.color : design.inkMuted.color)
                    if let detail {
                        Text(verbatim: detail)
                            .designFont(.caption, design)
                            .foregroundStyle(design.inkFaint.color)
                            // When a window resets is read against the clock.
                            .reported("chat.overview.usage.\(index).detail.volatile")
                    }
                }
                .accessibilityElement(children: .combine)
                .identified(
                    "chat.overview.usage.\(index)", label: label, value: ChatWords.used(window))
            }
            if let credits = usage.credits {
                Text(verbatim: String(localized: "Credits \(credits)"))
                    .designFont(.caption, design)
                    .foregroundStyle(design.inkFaint.color)
            }
        }
    }
}
