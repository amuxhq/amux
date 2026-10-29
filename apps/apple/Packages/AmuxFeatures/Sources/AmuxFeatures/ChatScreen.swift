import AmuxCore
import AmuxDesign
import SwiftUI

/// Who a chat is with and where it runs, from the fleet: the header paints
/// from these before the chat's own rows arrive.
public struct ChatSubject: Equatable, Sendable {
    public let name: String
    public let host: String
    public let directory: String
    /// The branch its directory has checked out, when it is in a repository.
    public let branch: String?
    /// How this phone reaches the agent's host.
    public let reach: Reach
    /// Live agents below this one on every host its host knows of, as its
    /// fleet row counts them: children on hosts this phone is not paired
    /// with included.
    public let descendants: Int
    /// Jobs its provider runs in the background, as its fleet row counts them.
    public let backgroundJobs: Int
    /// A stop of it from this phone is held: its host cannot be reached, and
    /// the stop goes when the host is back.
    public let stopHeld: Bool

    public init(
        name: String, host: String, directory: String, branch: String? = nil, reach: Reach,
        descendants: Int = 0, backgroundJobs: Int = 0, stopHeld: Bool = false
    ) {
        self.name = name
        self.host = host
        self.directory = directory
        self.branch = branch
        self.reach = reach
        self.descendants = descendants
        self.backgroundJobs = backgroundJobs
        self.stopHeld = stopHeld
    }

    public var reachable: Bool { reach.online }

    /// "~/s/amux · Studio", or why the host cannot be reached.
    public var place: String {
        switch reach {
        case .online:
            return PlaceNames.place(host: host.isEmpty ? nil : host, directory: directory)
        case .offline(.revoked): return String(localized: "\(host) · no longer trusts this phone")
        case .offline(.signedOut): return String(localized: "\(host) · this phone is signed out")
        case .away: return String(localized: "\(host) · away on the free plan")
        case .offline(.plain): return String(localized: "\(host) · away")
        }
    }

    /// "refactor-auth/studio": what another agent, a script or a terminal
    /// writes to reach this one.
    public var address: String {
        [name, host.isEmpty ? nil : host.lowercased()].compactMap { $0 }.joined(separator: "/")
    }
}

/// What a chat asks of whoever presented it: anything that leaves the
/// screen, reaches the clipboard, a picker, or the agent's lifecycle.
public enum ChatAction: Equatable, Sendable {
    case back
    case attach(AttachChoice)
    case rename(String)
    /// End the agent's process. Resume starts it again.
    case stopAgent
    /// Take back a stop held while the agent's host cannot be reached.
    case cancelStop
    case delete
    case copyAddress
    /// A parent or child in this chat's family.
    case open(AgentKey)
    /// The agent's uncommitted changes, from the header's changes chip.
    case review
    /// Start or stop dictating into the draft.
    case dictate
    /// Dictation was refused: open the app's page in Settings.
    case dictationSettings
}

/// What the chat has open over itself; one at a time.
public enum ChatOverlay: String, Equatable, Sendable {
    case rename
    /// Stopping the agent, with what the stop cuts named first.
    case stop
    case delete
    /// The model, effort and permission mode.
    case settings
    /// What the plus offers, above the composer.
    case plus
}

/// One agent's chat.
///
/// The rows run full height under a floating header that names the agent
/// and where it runs. Exactly one thing stands at the bottom: a card being
/// asked about the agent itself, the head ask docked where the composer
/// was, a card saying why the agent cannot take a message, or the composer
/// with the queue and the facts strip above it. The draft outlives all of
/// them.
public struct ChatScreen: View {
    @Environment(\.design) private var design
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesTransparency) private var reducesTransparency
    let model: ChatModel
    let subject: ChatSubject
    let family: FamilyHeader?
    let actions: (ChatAction) -> Void
    @State private var showing: ChatOverlay?
    @State private var overviewOpen = false
    /// Bumped to put the keyboard down.
    @State private var putDown = 0
    /// The screen's height and its header's, which bound how tall a docked
    /// card may grow.
    @State private var screenHeight: CGFloat?
    @State private var screenWidth: CGFloat?
    /// How wide the bar's trailing pill is, which the title gives way to.
    @State private var trailingWidth: CGFloat = 44
    @State private var headerHeight: CGFloat = 0

    public init(
        model: ChatModel, subject: ChatSubject, family: FamilyHeader? = nil,
        showing: ChatOverlay? = nil, actions: @escaping (ChatAction) -> Void
    ) {
        self.model = model
        self.subject = subject
        self.family = family
        self.actions = actions
        _showing = State(initialValue: showing)
    }

    /// How much of the chat a docked ask card always leaves showing above
    /// it, so a long one at a large text size still reads as part of the
    /// conversation rather than a page of its own.
    static let feedGlimpse: CGFloat = 48

    /// Everyone below this agent in its family, on any host: a stop or a
    /// delete takes them all with it. The fleet row counts them on hosts this
    /// phone cannot see; the family this phone lists is the floor.
    private var descendants: Int {
        max(subject.descendants, family?.children.reduce(0) { $0 + Int($1.members) } ?? 0)
    }

    public var body: some View {
        ZStack(alignment: .bottom) {
            Ground()
            feed
            if showing == .plus {
                // The plus card is a choice about the composer: the rows step
                // back behind it, and a tap on them puts it away.
                Color.black.opacity(0.22)
                    .ignoresSafeArea()
                    .onTapGesture { showing = nil }
                    .accessibilityHidden(true)
            }
            if model.newActivity {
                Button { model.jumpToNewest() } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "arrow.down")
                        Text("New activity")
                    }
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .frosted(Capsule(), as: .glass)
                }
                .buttonStyle(.amuxControl)
                .padding(.bottom, 10)
                .identified("chat.newActivity", label: String(localized: "New activity"), value: "shown")
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            Group {
                if let parent = family?.parent {
                    FamilyLine(parent: parent) { actions(.open($0)) }
                        .padding(.horizontal, Space.margin)
                        .padding(.bottom, Space.s)
                }
            }
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { headerHeight = $0 }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            ChatStanding(
                model: model, subject: subject, children: family?.children ?? [],
                descendants: descendants, showing: $showing, putDown: putDown,
                askRoom: screenHeight.map { max(0, $0 - headerHeight - Self.feedGlimpse) },
                openOverview: { overviewOpen = true }, actions: actions)
        }
        .onGeometryChange(for: CGSize.self) { $0.size } action: {
            screenHeight = $0.height
            screenWidth = $0.width
        }
        .navigationBarBackButtonHidden(true)
        .toolbarTitleDisplayMode(.inline)
        .toolbar { bar }
        .accessibilityElement(children: .contain)
        .reported("chat", value: subject.name)
        .sheet(isPresented: $overviewOpen) {
            ChatOverview(model: model, subject: subject, now: Date()) {
                overviewOpen = false
                actions(.review)
            }
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
            .surface()
        }
    }

    // MARK: - The header

    /// The title pill's width at most: what the bar has left once the way
    /// back and the trailing pill are drawn, and never more than 220.
    ///
    /// The bar does not shrink an item to fit. Given items wider than
    /// itself, it folds every one of them into its own overflow menu, the
    /// way back with them, so a long name or a long exit reason took the
    /// way back off the screen. It spaces its items further apart when
    /// their shared glass is hidden, as it is with less transparency, so
    /// what it spends besides the pills is counted for that case.
    private var titleWidth: CGFloat {
        guard let screenWidth else { return Self.widestTitle }
        return min(Self.widestTitle, max(Self.narrowestTitle, screenWidth - trailingWidth - Self.barSpends))
    }

    private static let widestTitle: CGFloat = 220
    private static let narrowestTitle: CGFloat = 96
    /// The bar's margins, the way back and the gaps between the three
    /// pieces, measured on a 402-point phone with less transparency.
    private static let barSpends: CGFloat = 160

    private var detached: Bool { model.frame?.waiting == .detached }

    private var placeLine: String {
        if detached, subject.reachable { return String(localized: "\(subject.host) · out of reach") }
        if case .exited(let cause)? = model.frame?.phase { return ChatWords.exited(cause) }
        return subject.place
    }

    /// The place line is a path unless it states why the chat cannot be reached; a path keeps
    /// its ends and a sentence keeps its opening words.
    private var placeIsPath: Bool {
        if !subject.reachable { return false }
        if detached { return false }
        if case .exited? = model.frame?.phase { return false }
        return true
    }

    /// The system's bar, as Slack and Messages use it: the way back in a
    /// circle, who the chat is with in a pill beside it, and the chat's own
    /// actions — its changes when it has any, and the menu — sharing one
    /// pill at the trailing edge. Every pill is the bar's height.
    @ToolbarContentBuilder
    private var bar: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            BarBack(identifier: "chat.back") {
                putDown += 1
                actions(.back)
            }
        }
        .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
        // The way back is a circle of its own; the bar would otherwise set
        // it and the title on one piece of glass.
        ToolbarSpacer(.fixed, placement: .topBarLeading)
        ToolbarItem(placement: .topBarLeading) {
            title
        }
        .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
        ToolbarItem(placement: .topBarTrailing) {
            HStack(spacing: 0) {
                if let changes = model.changes { changesButton(changes) }
                overflow
            }
            .barItemSurface()
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { trailingWidth = $0 }
        }
        .sharedBackgroundVisibility(reducesTransparency ? .hidden : .automatic)
    }

    /// The agent's name over where it runs; tapping it opens the agent's
    /// overview, as tapping a name in a chat app opens its page.
    private var title: some View {
        Button {
            putDown += 1
            overviewOpen = true
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                Text(subject.name)
                    .designFont(.bodyEmphasis, design)
                    .foregroundStyle(design.ink.color)
                    .lineLimit(1)
                HStack(spacing: Space.xs) {
                    if detached || !subject.reachable {
                        Image(systemName: "circle.dashed")
                            .font(.system(size: 9, weight: .bold))
                            .foregroundStyle(design.inkFaint.color)
                    }
                    Text(placeLine)
                        .designFont(.caption, design)
                        .foregroundStyle(design.inkMuted.color)
                        .lineLimit(1)
                        .truncationMode(placeIsPath ? .middle : .tail)
                }
            }
            // The bar does not grow with the text: past the largest ordinary
            // size the pill would no longer fit it.
            .dynamicTypeSize(...DynamicTypeSize.xLarge)
            .padding(.horizontal, Space.m)
            .modifier(Capped(width: titleWidth))
            .frame(height: 36)
            .barItemSurface()
        }
        .accessibilityLabel("\(subject.name), \(placeLine)")
        .accessibilityHint("Opens the overview")
        .identified("chat.title", label: subject.name, value: subject.name)
        .identified(
            "chat.header", label: placeLine,
            value: detached ? "detached" : (subject.reachable ? "live" : "away"))
    }

    /// "+42 −7", which opens the review. What the counts are against is said
    /// on the review page rather than here: a branch's name squeezed the
    /// pill until neither read.
    private func changesButton(_ changes: ChangeTotals) -> some View {
        Button {
            putDown += 1
            actions(.review)
        } label: {
            HStack(spacing: Space.xs) {
                Text(verbatim: "+\(changes.added)").foregroundStyle(design.added.color)
                Text(verbatim: "\u{2212}\(changes.removed)").foregroundStyle(design.removed.color)
            }
            .designFont(.caption, design)
            .monospacedDigit()
            .dynamicTypeSize(...DynamicTypeSize.xLarge)
            // Counts never break across lines: on a narrow phone the title
            // beside them gives way instead.
            .fixedSize()
            .padding(.leading, Space.m)
            .padding(.trailing, Space.xs)
            .frame(height: 36)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(ChatWords.changes(
            files: Int(changes.files), added: changes.added, removed: changes.removed))
        .accessibilityHint("Opens the review")
        .identified(
            "chat.changes", label: "+\(changes.added) \u{2212}\(changes.removed)",
            value: model.comparison.rawValue)
    }

    private var overflow: some View {
        let running = model.frame.map({ if case .exited = $0.phase { false } else { true } }) ?? false
        var items = [
            MenuItem(title: String(localized: "Overview"), systemImage: "list.bullet.rectangle") {
                overviewOpen = true
            },
            MenuItem(title: String(localized: "Rename"), systemImage: "pencil") { showing = .rename },
            MenuItem(title: String(localized: "Copy Address"), systemImage: "doc.on.doc") {
                actions(.copyAddress)
            },
        ]
        // A live agent is stopped, which keeps it to resume; only an exited
        // one is deleted, so nothing live is ever removed from here.
        if running && subject.stopHeld {
            items.append(MenuItem(title: String(localized: "Cancel Stop"), systemImage: "arrow.uturn.backward") {
                actions(.cancelStop)
            })
        } else if running {
            items.append(MenuItem(
                title: String(localized: "Stop Agent"), systemImage: "stop.circle", destructive: true
            ) { showing = .stop })
        } else {
            items.append(MenuItem(
                title: String(localized: "Delete Agent"), systemImage: "trash", destructive: true
            ) { showing = .delete })
        }
        return MenuButton(
            name: String(localized: "More"), identifier: "chat.more", items: items,
            opened: { putDown += 1 }
        ) {
            Image(systemName: "ellipsis")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(design.ink.color)
                .frame(width: 36, height: 36)
        }
    }

    // MARK: - The rows

    /// The list is a UIKit leaf (`Leaves/TranscriptList.swift`): it lays out
    /// the rows in view, keeps the reader's place when rows land above them
    /// and keeps the bottom while they follow. The rows it hosts are the
    /// SwiftUI views below.
    private var feed: some View {
        TranscriptList(model: model, notices: notices, landings: landings)
    }

    /// After the newest row: this client's prompts that land there.
    private var landings: [FeedLanding] {
        model.landingInFeed.enumerated().map { index, prompt in
            FeedLanding(
                id: "chat.landing.\(index)", text: prompt.text,
                state: ChatWords.underway(prompt.underway, host: subject.host))
        }
    }

    /// Above the oldest row: older history on its way or out of reach, or
    /// while the chat is empty, the loading hint or the away notice.
    private var notices: [FeedNotice] {
        var notices: [FeedNotice] = []
        switch model.paging {
        case .fetching:
            notices.append(FeedNotice(id: "chat.paging", text: String(localized: "Loading older messages…"), value: "fetching"))
        case .unreachable:
            notices.append(FeedNotice(
                id: "chat.paging", text: String(localized: "Can’t reach \(subject.host) for older messages"),
                value: "unreachable"))
        case .failed(let reason):
            notices.append(FeedNotice(id: "chat.paging", text: reason, value: "failed"))
        case .idle:
            break
        }
        if model.empty, !(model.frame?.caughtUp ?? false) {
            if !subject.reachable {
                notices.append(FeedNotice(id: "chat.away", text: awayNotice, value: awayCause))
            } else if model.loadingHint {
                notices.append(FeedNotice(
                    id: "chat.loading", text: String(localized: "Loading this chat from \(subject.host)…"),
                    value: "shown"))
            }
        }
        return notices
    }

    private var awayNotice: String {
        switch subject.reach {
        case .offline(.revoked):
            String(localized: "\(subject.host) no longer trusts this phone. Pair again to see this chat.")
        case .offline(.signedOut):
            String(localized: "This phone is signed out, so \(subject.host) is out of reach. This chat fills in when you sign in again.")
        case .away:
            String(localized: "\(subject.host) is away because the free plan does not relay between hosts. This chat fills in once it can be reached.")
        case .online, .offline(.plain):
            String(localized: "\(subject.host) is away. This chat fills in when it is back.")
        }
    }

    /// The notice's cause, as a test reads it.
    private var awayCause: String {
        switch subject.reach {
        case .offline(.revoked): "revoked"
        case .offline(.signedOut): "signedOut"
        case .online, .away, .offline(.plain): "plain"
        }
    }
}

/// A notice above the oldest row, as the list draws it.
struct FeedNotice: Hashable, Sendable {
    let id: String
    let text: String
    let value: String
}

/// A prompt of this client's on its way, drawn at the feed's end as it
/// will stand once the agent has it, with how it is on its way under it.
struct FeedLanding: Hashable, Sendable {
    let id: String
    let text: [Segment]
    let state: String
}

struct FeedLandingView: View {
    @Environment(\.design) private var design
    let landing: FeedLanding

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            PromptBubble(text: landing.text)
            Text(landing.state)
                .designFont(.caption, design)
                .foregroundStyle(design.inkFaint.color)
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .padding(.bottom, RowGrid.prose)
        .accessibilityElement(children: .combine)
        .identified(landing.id, label: ChatWords.text(of: landing.text), value: landing.state)
    }
}

struct FeedNoticeView: View {
    @Environment(\.design) private var design
    let notice: FeedNotice

    var body: some View {
        Text(notice.text)
            .designFont(.detail, design)
            .foregroundStyle(design.inkMuted.color)
            .frame(maxWidth: .infinity, alignment: .center)
            .multilineTextAlignment(.center)
            .padding(.vertical, 18)
            .identified(notice.id, label: notice.text, value: notice.value)
    }
}

/// What stands at the bottom of a chat, one thing at a time: a card about
/// the agent itself, the head ask docked where the composer was, a card
/// saying why the agent cannot take a message, or the composer with the
/// queue and the facts strip above it. The draft outlives all of them.
public struct ChatStanding: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    let model: ChatModel
    let subject: ChatSubject
    /// The agents this one started, for the dock.
    let children: [FleetCard]
    /// Whether the dock opens with its list showing.
    let dockExpanded: Bool
    /// The agents a delete takes with this one.
    let descendants: Int
    @Binding var showing: ChatOverlay?
    let putDown: Int
    /// The most height an ask card docked here may take; past it the card's
    /// content scrolls under its pinned buttons.
    let askRoom: CGFloat?
    /// Opens what runs around the chat, from the facts strip.
    let openOverview: () -> Void
    let actions: (ChatAction) -> Void
    @FocusState private var focused: Bool

    public init(
        model: ChatModel, subject: ChatSubject, children: [FleetCard] = [], dockExpanded: Bool = false,
        descendants: Int = 0, showing: Binding<ChatOverlay?> = .constant(nil), putDown: Int = 0,
        askRoom: CGFloat? = nil,
        openOverview: @escaping () -> Void = {}, actions: @escaping (ChatAction) -> Void = { _ in }
    ) {
        self.model = model
        self.subject = subject
        self.children = children
        self.dockExpanded = dockExpanded
        self.descendants = descendants
        _showing = showing
        self.putDown = putDown
        self.askRoom = askRoom
        self.openOverview = openOverview
        self.actions = actions
    }

    public var body: some View {
        standing
            .onChange(of: putDown) { focused = false }
            // A bare /model, /effort or /rename opens amux's own picker or
            // the rename card, as the menu would.
            .onChange(of: model.asks) { _, asks in
                guard let asks else { return }
                model.asks = nil
                focused = false
                switch asks {
                case .settings: showing = .settings
                case .rename: showing = .rename
                }
            }
    }

    /// A question card's picks live on the chat, like the draft.
    private func keeping(_ ask: AskCard) -> QuestionKeeping {
        QuestionKeeping(kept: model.questionDraft(onAsk: ask.key)) { [model] in
            model.keep($0, onAsk: ask.key)
        }
    }

    /// So are a tool server form's values.
    private func formKeeping(_ ask: AskCard) -> FormKeeping {
        FormKeeping(kept: model.formDraft(onAsk: ask.key)) { [model] in
            model.keep(form: $0, onAsk: ask.key)
        }
    }

    @ViewBuilder
    private var standing: some View {
        VStack(spacing: 8) {
            if subject.stopHeld { heldStop }
            if model.frame?.composer.switchedAway == true { switchedAway }
            if let notice = model.notice {
                // Every notice is something that did not happen, so it reads in
                // the failure ink rather than the accent kept for asks.
                Text(notice)
                    .designFont(.detail, design)
                    .foregroundStyle(design.removed.color)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 6)
                    .identified("chat.notice", label: notice)
            }
            switch showing {
            case .rename?:
                RenameCard(current: subject.name, cancel: { showing = nil }) { name in
                    showing = nil
                    actions(.rename(name))
                }
            case .stop?:
                StopCard(
                    name: subject.name,
                    cuts: ChatWords.stopCuts(
                        name: subject.name, phase: model.frame?.phase,
                        jobs: max(subject.backgroundJobs, model.overview?.jobs.count ?? 0),
                        subAgents: descendants),
                    cancel: { showing = nil }
                ) {
                    showing = nil
                    actions(.stopAgent)
                }
            case .delete?:
                DeleteCard(name: subject.name, descendants: descendants, cancel: { showing = nil }) {
                    showing = nil
                    actions(.delete)
                }
            case .settings?:
                if let settings = model.settings {
                    SettingsCard(
                        view: settings, kind: model.frame?.kind, change: { model.change($0) },
                        close: { showing = nil })
                } else {
                    composerStack
                }
            case .plus?:
                PlusCard(settings: model.settings, attach: { choice in
                    showing = nil
                    focused = false
                    actions(.attach(choice))
                }, openSettings: {
                    focused = false
                    showing = .settings
                })
                composerStack
            case nil:
                if let ask = model.ask, ask.state != .dismissed {
                    HeightCap(cap: askRoom) {
                        AskCardView(card: ask, questions: keeping(ask), form: formKeeping(ask), act: answer)
                    }
                } else {
                    if let ask = model.ask { AskCardView(card: ask, questions: keeping(ask), form: formKeeping(ask), act: answer) }
                    composerStack
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 10)
        .animation(photographed ? nil : Motion.standard, value: showing)
    }

    /// Terminal Claude's own view resumed another conversation: prompts are
    /// refused until this one is back, which one tap does.
    private var switchedAway: some View {
        let words = String(localized: "Claude’s own view switched to another conversation")
        return VStack(alignment: .leading, spacing: 10) {
            Text(words)
                .designFont(.detail, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button { model.switchBack() } label: {
                ActionLabel(String(localized: "Switch Back"), kind: .primary, fill: true)
            }
            .buttonStyle(.amuxControl)
            .identified("chat.switchBack", label: String(localized: "Switch Back"))
        }
        .padding(14)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .identified("chat.switchedAway", label: words)
    }

    /// A stop that has not gone yet, with the way to take it back.
    private var heldStop: some View {
        HStack(spacing: 10) {
            Image(systemName: "clock")
                .font(.system(size: 14))
                .foregroundStyle(design.inkMuted.color)
            Text(String(localized: "Stop held · sends when \(subject.host) is back"))
                .designFont(.detail, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button { actions(.cancelStop) } label: {
                ActionLabel(String(localized: "Cancel"), kind: .outline)
            }
            .buttonStyle(.amuxControl)
            .identified("chat.stopHeld.cancel", label: String(localized: "Cancel"))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .identified("chat.stopHeld", label: String(localized: "Stop held · sends when \(subject.host) is back"))
    }

    @ViewBuilder
    private var composerStack: some View {
        if showing != .plus {
            ChatDock(model: model, children: children, host: subject.host, expanded: dockExpanded) {
                actions(.open($0))
            }
            if let overview = model.overview {
                StripLine(context: model.frame?.context, overview: overview, open: openOverview)
            }
        }
        let matches = model.slashMatches
        if !matches.isEmpty { SlashRows(matches: matches, pick: model.pick) }
        if let signIn = model.frame?.signIn {
            let kind = model.frame?.kind
            FootCard(
                kind: "sign-in", title: ChatWords.needsSignIn(kind),
                detail: [ChatWords.signIn(signIn), signIn.message, ChatWords.signInSteps(kind, host: subject.host)]
                    .filter { !$0.isEmpty }
                    .joined(separator: "\n"))
        } else {
            ComposerBox(
                model: model, placeholder: placeholder, activity: model.frame?.composer.activity,
                activitySubject: activitySubject, host: subject.host, plusOpen: showing == .plus,
                togglePlus: {
                    focused = false
                    showing = showing == .plus ? nil : .plus
                }, dictate: { actions($0) }, openSettings: {
                    focused = false
                    showing = .settings
                }, focused: $focused)
        }
    }

    private var placeholder: String {
        var working = false
        if case .working? = model.frame?.phase { working = true }
        return ChatWords.placeholder(
            model.frame?.composer.mode, agent: subject.name, host: subject.host,
            reach: subject.reach, working: working)
    }

    private var activitySubject: String? {
        guard case .running(let key)? = model.frame?.composer.activity?.kind else { return nil }
        return model.cell(for: key).row.flatMap(ChatWords.subject(of:))
    }

    private func answer(_ action: AskAction) {
        switch action {
        case .choose(let index, let note): model.answer(choice: index, note: note)
        case .respond(let responses): model.answer(responses: responses)
        case .reply(let text, let soFar): model.replyInstead(text, soFar: soFar)
        case .submit(let values): model.submit(form: values)
        case .stop: model.interrupt()
        case .resend: model.resendAnswer()
        case .discard: model.discardAnswer()
        case .cancelHeld: model.cancelHeldAnswer()
        }
    }
}

/// One row cell: it observes its own row, and the row below it for whether
/// its rail runs on.
struct RowCellView: View {
    let cell: RowCell
    let model: ChatModel

    var body: some View {
        if let row = cell.row, model.shows(row) {
            ChatRowView(
                row: row, expanded: model.isExpanded(row.id),
                rail: RailJoin.of(row, next: model.row(below: row.id)),
                placement: RunPlacement.of(
                    row, open: row.run?.open ?? false,
                    counts: { row.run.flatMap(model.counts(of:)) }),
                bytes: model.bytes(of:), toggle: { model.toggle(row.id) },
                toggleRun: { if let run = row.run { model.toggleRun(run) } })
        } else {
            Color.clear.frame(height: 0)
        }
    }
}

/// Who started this agent, one tap from its own chat, with a mark when it
/// needs the person. The agents this one started dock above the composer.
private struct FamilyLine: View {
    @Environment(\.design) private var design
    let parent: FleetCard
    let open: (AgentKey) -> Void

    var body: some View {
        HStack(spacing: 0) {
            Button { open(parent.agent) } label: {
                HStack(spacing: 5) {
                    if parent.familyAttention == .needsYou || parent.attention == .needsYou {
                        NeedsYouDot()
                    } else {
                        Image(systemName: "arrow.up.left")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(design.inkFaint.color)
                    }
                    Text("From \(parent.name)")
                        .designFont(.caption, design)
                        .foregroundStyle(design.ink.color)
                        .lineLimit(1)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .frosted(Capsule(), as: .glass)
            }
            .buttonStyle(.amuxControl)
            .identified(
                "chat.family.\(parent.name)", label: String(localized: "From \(parent.name)"),
                value: parent.attention == .needsYou ? "needs-you" : nil)
            Spacer(minLength: 0)
        }
        .identified("chat.family", value: "parent")
    }
}

/// Renaming the agent, in place of the composer.
private struct RenameCard: View {
    @Environment(\.design) private var design
    let current: String
    let cancel: () -> Void
    let confirm: (String) -> Void
    @State private var name: String
    @FocusState private var focused: Bool

    init(current: String, cancel: @escaping () -> Void, confirm: @escaping (String) -> Void) {
        self.current = current
        self.cancel = cancel
        self.confirm = confirm
        _name = State(initialValue: current)
    }

    private var trimmed: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }
    /// Why the host would refuse the name, said while it is typed.
    private var problem: AgentNameProblem? { Bridge.agentNameProblem(trimmed) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(String(localized: "Rename \(current)"))
                .designFont(.bodyEmphasis, design)
                .foregroundStyle(design.ink.color)
            TextField(String(localized: "Name"), text: $name)
                .designFont(.mono, design)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .focused($focused)
                .submitLabel(.done)
                .onSubmit { if problem == nil { confirm(trimmed) } }
                .padding(10)
                .background {
                    RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
                        .fill(design.sunken.color)
                }
                .identified("chat.rename.field", value: name)
            if let problem, !trimmed.isEmpty {
                Text(ChatWords.nameProblem(problem))
                    .designFont(.detail, design)
                    .foregroundStyle(design.accent.color)
                    .identified("chat.rename.problem", value: problem.rawValue)
            }
            ButtonPair {
                choiceButton(String(localized: "Cancel"), kind: .outline, id: "chat.rename.cancel", action: cancel)
                choiceButton(
                    String(localized: "Rename"), kind: .primary, id: "chat.rename.confirm",
                    enabled: problem == nil
                ) { confirm(trimmed) }
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .onAppear { focused = true }
    }
}

/// Stopping the agent: a stop is always now, so when it cuts a turn,
/// background jobs or subagents the confirm says which, and otherwise asks
/// plainly.
private struct StopCard: View {
    @Environment(\.design) private var design
    let name: String
    /// What the stop cuts, as a question, or nil when it cuts nothing under way.
    let cuts: String?
    let cancel: () -> Void
    let confirm: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(String(localized: "Stop \(name)?"))
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            if let cuts {
                Text(cuts)
                    .designFont(.detail, design)
                    .foregroundStyle(design.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .identified("chat.stop.cuts", label: cuts)
            }
            Consequence("checkmark", kept: true, String(localized: "Its chat and edits stay. Resume starts it again."))
            ButtonPair {
                choiceButton(String(localized: "Cancel"), kind: .outline, id: "chat.stop.cancel", action: cancel)
                choiceButton(String(localized: "Stop"), kind: .destructive, id: "chat.stop.confirm", action: confirm)
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
        .identified("chat.stop.card", label: cuts ?? String(localized: "Stop \(name)?"))
    }
}

/// Deleting an exited agent, with what that does named first.
private struct DeleteCard: View {
    @Environment(\.design) private var design
    let name: String
    let descendants: Int
    let cancel: () -> Void
    let confirm: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(String(localized: "Delete \(name)?"))
                .designFont(.screenTitle, design)
                .foregroundStyle(design.ink.color)
            Consequence("checkmark", kept: true, String(localized: "Its edits stay. Nothing is reverted."))
            Consequence("xmark", kept: false, String(localized: "The chat is deleted on every device."))
            if descendants == 1 {
                Consequence("xmark", kept: false, String(localized: "Its subagent is deleted too."))
            } else if descendants > 1 {
                Consequence("xmark", kept: false, String(localized: "Its \(descendants) subagents are deleted too."))
            }
            ButtonPair {
                choiceButton(String(localized: "Cancel"), kind: .outline, id: "chat.delete.cancel", action: cancel)
                choiceButton(String(localized: "Delete"), kind: .destructive, id: "chat.delete.confirm", action: confirm)
            }
        }
        .padding(16)
        .frosted(RoundedRectangle(cornerRadius: design.metrics.floatRadius, style: .continuous))
    }

}

/// Lays its one subview out at its own height, never taller than `cap`.
/// A frame with a maximum height grows to that maximum when it is offered
/// more and centres what it holds, so a short docked card would float above
/// the bottom with the feed pushed up behind it; this offers the subview at
/// most `cap` and takes only the height it asks for.
struct HeightCap: Layout {
    let cap: CGFloat?

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let subview = subviews.first else { return .zero }
        return subview.sizeThatFits(capped(proposal))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        subviews.first?.place(at: bounds.origin, proposal: ProposedViewSize(bounds.size))
    }

    private func capped(_ proposal: ProposedViewSize) -> ProposedViewSize {
        guard let cap else { return proposal }
        return ProposedViewSize(width: proposal.width, height: min(proposal.height ?? cap, cap))
    }
}

/// Lays its one subview out at its own width, never wider than `cap`.
/// The system's bar asks an item how wide it would like to be without
/// offering a width, and a frame with a maximum width passes that question
/// straight to what it holds, so a pill capped by a frame still reported
/// the full width of its longest line.
struct WidthCap: Layout {
    let cap: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let subview = subviews.first else { return .zero }
        let wanted = proposal.width ?? subview.sizeThatFits(.unspecified).width
        return subview.sizeThatFits(ProposedViewSize(width: min(wanted, cap), height: proposal.height))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        subviews.first?.place(at: bounds.origin, proposal: ProposedViewSize(bounds.size))
    }
}

/// `WidthCap` as a modifier.
struct Capped: ViewModifier {
    let width: CGFloat

    func body(content: Content) -> some View {
        WidthCap(cap: width) { content }
    }
}
