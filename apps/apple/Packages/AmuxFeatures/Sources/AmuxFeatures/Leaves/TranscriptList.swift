import AmuxCore
import AmuxDesign
import SwiftUI
import UIKit

/// The chat's rows: a collection view that lays out only the rows in view,
/// keeps the reader's place when rows land above them, and keeps the bottom
/// while they follow the newest row.
///
/// The rows themselves stay SwiftUI: each cell hosts the same row view the
/// screen drew before, under the app's own environment carried in here. What
/// the leaf adds is the list's mechanics, which SwiftUI's stacks could not
/// give at the streaming budget: a plain stack placed every drawn row again
/// on every arrival, and a lazy one forgot the heights above the reader, so
/// no correction could hold their place.
struct TranscriptList: View {
    @Environment(\.design) private var design
    @Environment(\.photographed) private var photographed
    @Environment(\.reducesMotion) private var reducesMotion
    @Environment(\.reducesTransparency) private var reducesTransparency
    @Environment(\.hidesNeedsYouDot) private var hidesNeedsYouDot
    @Environment(\.reportsIdentifiedElements) private var reportsElements
    @Environment(\.reportedIdentifierPrefix) private var reportedPrefix
    @Environment(\.reportsIdentifiedElementGeometry) private var reportsGeometry
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var reported = ReportedElements()
    let model: ChatModel
    /// What stands above the oldest row.
    let notices: [FeedNotice]
    /// This client's prompts on their way, after the newest row.
    var landings: [FeedLanding] = []

    var body: some View {
        // The list fills the space under the header and the standing card
        // and is told their insets, so its content scrolls under both and
        // its bottom is the bottom the reader sees.
        GeometryReader { proxy in
            ListView(
                model: model, items: items, notices: notices, landings: landings,
                environment: CellEnvironment(
                    design: design, photographed: photographed, reducesMotion: reducesMotion,
                    reducesTransparency: reducesTransparency, hidesNeedsYouDot: hidesNeedsYouDot,
                    reportsElements: reportsElements, reportedPrefix: reportedPrefix,
                    reportsGeometry: reportsGeometry, dynamicTypeSize: dynamicTypeSize),
                revision: model.revision, toNewest: model.toNewest,
                insets: proxy.safeAreaInsets, reported: reported)
            .ignoresSafeArea()
        }
        .preference(key: IdentifiedElements.self, value: reported.elements)
    }

    private var items: [ListItem] {
        // `sequence` is what the list watches; `ids` is read, not watched.
        _ = model.sequence
        return notices.map { .notice($0.id) } + model.ids.map { .row($0) }
            + landings.map { .landing($0.id) }
    }
}

/// One thing the list draws.
enum ListItem: Hashable {
    case notice(String)
    case row(String)
    /// A prompt on its way, drawn where it will stand once the agent has it.
    case landing(String)
}

/// The environment the screen's rows read, carried into each hosted cell:
/// a hosted cell sits under the window's traits, not under the screen's
/// environment. A row that starts reading a new key of the app's own has
/// it added here.
struct CellEnvironment: Equatable {
    var design: Design
    var photographed: Bool
    var reducesMotion: Bool
    var reducesTransparency: Bool
    var hidesNeedsYouDot: Bool
    var reportsElements: Bool
    var reportedPrefix: String?
    var reportsGeometry: Bool
    /// Carried in like the rest so that a change of it, in Settings or by
    /// the driving door, compares unequal and has every height measured
    /// again: the rows would redraw at the new size regardless, inside
    /// frames cached for the old one.
    var dynamicTypeSize: DynamicTypeSize
}

private extension View {
    func cellEnvironment(_ environment: CellEnvironment) -> some View {
        self.environment(\.design, environment.design)
            .dynamicTypeSize(environment.dynamicTypeSize)
            .environment(\.photographed, environment.photographed)
            .environment(\.reducesMotion, environment.reducesMotion)
            .environment(\.reducesTransparency, environment.reducesTransparency)
            .environment(\.hidesNeedsYouDot, environment.hidesNeedsYouDot)
            .environment(\.reportsIdentifiedElements, environment.reportsElements)
            .environment(\.reportedIdentifierPrefix, environment.reportedPrefix)
            .environment(\.reportsIdentifiedElementGeometry, environment.reportsGeometry)
    }
}

/// What the hosted cells declared, in row order, for the driving door.
@MainActor
@Observable
final class ReportedElements {
    var elements: [IdentifiedElement] = []
}

private struct ListView: UIViewRepresentable {
    let model: ChatModel
    let items: [ListItem]
    let notices: [FeedNotice]
    let landings: [FeedLanding]
    let environment: CellEnvironment
    let revision: Int
    let toNewest: Int
    let insets: EdgeInsets
    let reported: ReportedElements

    func makeCoordinator() -> FeedCoordinator {
        FeedCoordinator(model: model, environment: environment, reported: reported)
    }

    func makeUIView(context: Context) -> FeedView {
        context.coordinator.view
    }

    func updateUIView(_ view: FeedView, context: Context) {
        context.coordinator.update(
            items: items, notices: notices, landings: landings, environment: environment,
            revision: revision, toNewest: toNewest, insets: insets)
    }
}

/// The scroll view, keeping the reader's place in every layout pass.
///
/// While the reader follows, the bottom is kept: rows arriving, cells
/// taking their measured height, the keyboard or a card changing the
/// space, all land with the newest row in view and nothing to catch up on
/// a frame later. In history, the row at the top of the reader's view is
/// kept where it is on screen: cells above it taking their measured height
/// move it within the rows, and the list scrolls by as much in the same
/// pass.
///
/// The rows' heights are the leaf's own (`FeedLayout`), not UIKit's
/// self-sizing: that measured on demand from estimates, kept what it
/// measured by index path, animated each result, and measured every cell on
/// screen again whenever one moved. Under the stream at the window's cap
/// that cost most of a frame per arrival, and three times a row was drawn
/// in another row's frame. Measured here, a height is known before a row
/// is placed and changes only when its row does.
final class FeedView: UICollectionView {
    /// Whether the reader follows the newest row, read from the model at
    /// each layout rather than handed down through SwiftUI, which would
    /// arrive a turn after a drag left the bottom and pin it again meanwhile.
    var following: () -> Bool = { false }
    /// The animated scroll to the newest row lands there itself.
    var animatingToNewest = false
    /// The list is moving itself to the bottom: not the reader.
    private(set) var pinning = false
    /// The reader's row before the cells are placed, and putting it back.
    var hold: (() -> FeedCoordinator.Held?)?
    var restore: ((FeedCoordinator.Held) -> Void)?
    var laidOut: (() -> Void)?
    /// The row held through a pass that measured rows or moved the list:
    /// the cells on screen stand at their old frames until the next pass
    /// places them, so that pass holds the same row at the same place
    /// rather than reading them.
    var carried: FeedCoordinator.Held?

    /// None, for the rows. The list runs under the status bar, the header
    /// and the home indicator, and a hosted cell passing under them took
    /// their safe area as padding: drawn taller than the height it was
    /// measured and placed at, over the row below. Its insets are the
    /// content insets the screen hands down, never the window's.
    override var safeAreaInsets: UIEdgeInsets { .zero }

    /// The bottom is set before the cells are placed, so the rows at the
    /// top are never displayed on the way to the bottom and never taken
    /// for the reader reaching them. Nobody moving the list means not a
    /// finger, not a deceleration, not the animated scroll.
    override func layoutSubviews() {
        var held: FeedCoordinator.Held?
        if following() {
            carried = nil
            if !isTracking, !isDragging, !isDecelerating, !animatingToNewest {
                let bottom = bottomOffset
                if abs(contentOffset.y - bottom) > 0.5 {
                    pinning = true
                    contentOffset.y = bottom
                    pinning = false
                }
            }
        } else {
            held = carried ?? hold?()
            carried = nil
        }
        super.layoutSubviews()
        // Rows measured while being placed change the heights above or
        // below the screen: placed again before anything is drawn.
        let measured = (collectionViewLayout as? FeedLayout)?.takeMeasuredDuringPass() ?? false
        if measured {
            collectionViewLayout.invalidateLayout()
            setNeedsLayout()
            carried = held
        }
        if let held { restore?(held) }
        laidOut?()
    }

    /// Where the newest row sits at the bottom; the top when the rows fit.
    /// The layout's own size is asked, which needs no cell placed yet.
    var bottomOffset: CGFloat {
        let inset = adjustedContentInset
        let height = collectionViewLayout.collectionViewContentSize.height
        return max(-inset.top, height + inset.bottom - bounds.height)
    }

    /// Whether the bottom is within a thumb's slack of the view.
    var atBottom: Bool {
        contentOffset.y >= bottomOffset - 24
    }
}

/// The rows stacked by their measured heights, one section, full width.
///
/// A row is measured when it first comes within a screen of the visible
/// rect, by the coordinator's measurer, and stands at an estimate until
/// then; a page that lands above the reader is placed from estimates and
/// measured as the reader scrolls up to it, the list holding their place
/// through each correction. A height is forgotten when its row's content
/// changes, when its rail's join to the row below does, when the width
/// changes, or when the environment does.
final class FeedLayout: UICollectionViewLayout {
    /// How tall an unmeasured row stands.
    static let estimate: CGFloat = 60
    /// How far beyond the visible rect rows are measured ahead of the
    /// reader, so a scroll meets measured rows.
    static let margin: CGFloat = 800

    /// Measures a row at the current width, or nil for one that cannot be
    /// measured yet.
    var measure: (ListItem) -> CGFloat? = { _ in nil }
    /// Stacked again as soon as they are set: a batch update asks for the
    /// new frames before any layout pass prepares them.
    var items: [ListItem] = [] {
        didSet { stack() }
    }
    private var heights: [ListItem: CGFloat] = [:]
    private var frames: [CGRect] = []
    private var height: CGFloat = 0
    private var measuredDuringPass = false

    func forget(_ item: ListItem) { heights[item] = nil }
    func forgetAll() { heights = [:] }
    func measured(_ item: ListItem) -> Bool { heights[item] != nil }

    /// Whether rows were measured since the last layout pass began: the
    /// frames below them moved, and the pass is run again.
    func takeMeasuredDuringPass() -> Bool {
        defer { measuredDuringPass = false }
        return measuredDuringPass
    }

    override var collectionViewContentSize: CGSize {
        CGSize(width: collectionView?.bounds.width ?? 0, height: height)
    }

    override func prepare() {
        super.prepare()
        stack()
    }

    private func stack() {
        let width = collectionView?.bounds.width ?? 0
        frames.removeAll(keepingCapacity: true)
        var y: CGFloat = 0
        for item in items {
            let h = heights[item] ?? Self.estimate
            frames.append(CGRect(x: 0, y: y, width: width, height: h))
            y += h
        }
        height = y
    }

    override func shouldInvalidateLayout(forBoundsChange newBounds: CGRect) -> Bool {
        guard let collectionView, newBounds.width != collectionView.bounds.width else { return false }
        heights = [:]
        return true
    }

    override func shouldInvalidateLayout(
        forPreferredLayoutAttributes preferredAttributes: UICollectionViewLayoutAttributes,
        withOriginalAttributes originalAttributes: UICollectionViewLayoutAttributes
    ) -> Bool {
        false
    }

    override func layoutAttributesForElements(in rect: CGRect) -> [UICollectionViewLayoutAttributes]? {
        // Rows near the rect are measured now, and the stack rebuilt, so
        // what is placed in this pass already stands at its height; the
        // rows after them have moved, which the pass after this corrects.
        let near = rect.insetBy(dx: 0, dy: -Self.margin)
        var measured = false
        for (index, frame) in frames.enumerated() where frame.intersects(near) {
            let item = items[index]
            if heights[item] == nil, let h = measure(item) {
                heights[item] = h
                measured = true
            }
        }
        if measured {
            stack()
            measuredDuringPass = true
        }
        return frames.enumerated().compactMap { index, frame in
            guard frame.intersects(rect) else { return nil }
            return attributes(at: index)
        }
    }

    override func layoutAttributesForItem(at indexPath: IndexPath) -> UICollectionViewLayoutAttributes? {
        guard frames.indices.contains(indexPath.item) else { return nil }
        return attributes(at: indexPath.item)
    }

    private func attributes(at index: Int) -> UICollectionViewLayoutAttributes {
        let attributes = UICollectionViewLayoutAttributes(forCellWith: IndexPath(item: index, section: 0))
        attributes.frame = frames[index]
        return attributes
    }
}

@MainActor
final class FeedCoordinator: NSObject, UICollectionViewDelegate {
    /// The row at the top of the reader's view and how far below the top
    /// of the list's bounds it starts.
    struct Held {
        let item: ListItem
        let below: CGFloat
    }
    let model: ChatModel
    let view: FeedView
    private let reported: ReportedElements
    private var dataSource: UICollectionViewDiffableDataSource<Int, ListItem>!
    private var items: [ListItem] = []
    private var notices: [String: FeedNotice] = [:]
    private var landings: [String: FeedLanding] = [:]
    private var environment: CellEnvironment
    private var toNewest: Int?
    private var revision: Int?
    /// The rail join each row was measured with.
    private var joins: [String: RailJoin] = [:]
    private var insets: EdgeInsets?
    private let layout = FeedLayout()
    /// Measures a row's content at the list's width, under the list's
    /// traits: hidden in the list, so a type size or appearance change
    /// reaches it as it reaches the cells.
    private let measurer = UIHostingController<AnyView>(rootView: AnyView(EmptyView()))
    private var measurements = 0
    /// The reader has a finger on the list or it is still moving from one.
    private var userScrolling = false
    /// Whether the bottom was in view at the last scroll: only its coming
    /// into view follows again, so a chat that fits on screen, always at
    /// its bottom, is not followed again by every move of the list.
    private var wasAtBottom = true
    /// The list is being moved by this code, not by the reader.
    private var moving = false
    /// What each cell on screen declared, with the cell, whose place in the
    /// window puts the frames where the door expects them.
    private var declared: [ListItem: (cell: Weak<UICollectionViewCell>, elements: [IdentifiedElement])] = [:]
    /// Where each cell's hosted root was, in the space its elements report in.
    private var roots: [ListItem: CGPoint] = [:]
    /// Something declared changed since the last publish; the layout pass
    /// that follows publishes once, rather than every cell's report doing so.
    private var stale = false

    init(model: ChatModel, environment: CellEnvironment, reported: ReportedElements) {
        self.model = model
        self.environment = environment
        self.reported = reported
        view = FeedView(frame: .zero, collectionViewLayout: layout)
        super.init()
        layout.measure = { [weak self] in self?.measure($0) }
        measurer.view.isHidden = true
        measurer.view.isUserInteractionEnabled = false
        measurer.view.backgroundColor = .clear
        // It scrolls with the content, and where it passes under the status
        // bar or the home indicator the window's safe area would be added to
        // what it measures.
        measurer.safeAreaRegions = []
        view.addSubview(measurer.view)
        view.backgroundColor = .clear
        view.showsVerticalScrollIndicator = false
        view.showsHorizontalScrollIndicator = false
        view.alwaysBounceVertical = true
        view.keyboardDismissMode = .interactive
        view.contentInsetAdjustmentBehavior = .never
        view.topEdgeEffect.style = .soft
        view.selfSizingInvalidation = .disabled
        view.delegate = self
        view.following = { [model] in model.following }
        view.hold = { [weak self] in self?.hold() }
        view.restore = { [weak self] in self?.restore($0) }
        view.laidOut = { [weak self] in self?.publishIfStale() }
        let registration = UICollectionView.CellRegistration<UICollectionViewCell, ListItem> {
            [weak self] cell, _, item in
            guard let self else { return }
            cell.contentConfiguration = self.configuration(for: item, in: cell)
            cell.backgroundConfiguration = .clear()
        }
        dataSource = UICollectionViewDiffableDataSource(collectionView: view) { view, path, item in
            view.dequeueConfiguredReusableCell(using: registration, for: path, item: item)
        }
    }

    /// A row as its cell draws it and as the measurer measures it.
    private func content(for item: ListItem, photographed: Bool? = nil) -> some View {
        var environment = self.environment
        if let photographed { environment.photographed = photographed }
        return CellContent(
            model: model, item: item, notice: notice(for: item), landing: landing(for: item))
            .padding(.horizontal, environment.design.metrics.gutter)
            .cellEnvironment(environment)
    }

    private func measure(_ item: ListItem) -> CGFloat? {
        let width = view.bounds.width
        guard width > 0 else { return nil }
        // Measured as if photographed: a row that parses or animates its
        // way to its final shape takes it at once, which is the height the
        // cell ends up at. The hosting controller takes a new root at its
        // next layout, so it is laid out before it is asked. Each
        // measurement is a fresh view: the same row given again would be
        // judged unchanged and keep the body it drew before, while what it
        // reads from the row below it may have moved since.
        if case .row(let id) = item { joins[id] = join(of: id) }
        measurements += 1
        measurer.rootView = AnyView(content(for: item, photographed: true).id(measurements))
        measurer.view.frame = CGRect(x: 0, y: 0, width: width, height: 0)
        measurer.view.setNeedsLayout()
        measurer.view.layoutIfNeeded()
        return measurer.sizeThatFits(in: CGSize(width: width, height: UIView.layoutFittingExpandedSize.height)).height
    }

    private func configuration(for item: ListItem, in cell: UICollectionViewCell) -> UIContentConfiguration {
        UIHostingConfiguration {
            content(for: item)
                // Hosted content reports its frames in a global space that
                // is the window's as of its own last layout, which goes
                // stale as the cell scrolls; the root's origin from the same
                // pass turns them into positions within the cell.
                .onGeometryChange(for: CGPoint.self) { $0.frame(in: .global).origin } action: {
                    [weak self] origin in self?.rooted(at: origin, for: item)
                }
                .onPreferenceChange(IdentifiedElements.self) { [weak self, weak cell] elements in
                    MainActor.assumeIsolated { self?.declare(elements, for: item, in: cell) }
                }
        }
        .margins(.all, 0)
    }

    private func notice(for item: ListItem) -> FeedNotice? {
        if case .notice(let id) = item { return notices[id] }
        return nil
    }

    private func landing(for item: ListItem) -> FeedLanding? {
        if case .landing(let id) = item { return landings[id] }
        return nil
    }

    // MARK: - What the screen hands down

    func update(
        items: [ListItem], notices: [FeedNotice], landings: [FeedLanding] = [],
        environment: CellEnvironment, revision: Int, toNewest: Int, insets: EdgeInsets
    ) {
        if self.insets != insets {
            self.insets = insets
            moving = true
            view.contentInset = UIEdgeInsets(
                top: insets.top + 8, left: 0, bottom: insets.bottom, right: 0)
            moving = false
            // The keyboard, a notice or a taller card growing the space under
            // the rows: a reader following the newest row keeps it above
            // them, which the next layout pass does.
            if model.following { view.setNeedsLayout() }
        }
        var reconfigure: [ListItem] = []
        var remeasure = false
        if environment != self.environment {
            self.environment = environment
            reconfigure = self.items
            layout.forgetAll()
            remeasure = true
        }
        let byId = Dictionary(uniqueKeysWithValues: notices.map { ($0.id, $0) })
        for (id, notice) in byId where self.notices[id] != nil && self.notices[id] != notice {
            reconfigure.append(.notice(id))
            layout.forget(.notice(id))
            remeasure = true
        }
        self.notices = byId
        let landingById = Dictionary(uniqueKeysWithValues: landings.map { ($0.id, $0) })
        for (id, landing) in landingById where self.landings[id] != nil && self.landings[id] != landing {
            reconfigure.append(.landing(id))
            layout.forget(.landing(id))
            remeasure = true
        }
        self.landings = landingById
        if let last = self.revision, last != revision {
            // Every row read since the last layout, however many wakes
            // that took: the model stamps each cell as it reads it.
            for case .row(let id) in items where model.revision(of: id) > last {
                layout.forget(.row(id))
            }
            remeasure = true
        }
        self.revision = revision
        if remeasure || items != self.items, forgetMovedJoins(items) { remeasure = true }
        if items != self.items { apply(items) }
        if !reconfigure.isEmpty {
            var snapshot = dataSource.snapshot()
            snapshot.reconfigureItems(reconfigure.filter { snapshot.indexOfItem($0) != nil })
            dataSource.apply(snapshot, animatingDifferences: false)
        }
        if remeasure { replace() }
        if let last = self.toNewest, last != toNewest { scrollToNewest() }
        self.toNewest = toNewest
    }

    /// Forgets the height of every measured row whose rail now joins the
    /// row below differently, and says whether there was one. A rail that
    /// runs on draws a shorter gap under its row, and what decides it is the
    /// row below: one landing under the newest row, or going, changes the
    /// height of a row that did not itself change.
    private func forgetMovedJoins(_ items: [ListItem]) -> Bool {
        var moved = false
        var kept: [String: RailJoin] = [:]
        for case .row(let id) in items {
            guard let was = joins[id], layout.measured(.row(id)) else { continue }
            if join(of: id) != was {
                layout.forget(.row(id))
                moved = true
            } else {
                kept[id] = was
            }
        }
        joins = kept
        return moved
    }

    private func join(of id: String) -> RailJoin? {
        model.cell(for: id).row.map { RailJoin.of($0, next: model.row(below: id)) }
    }

    /// Rows whose heights were forgotten are measured and placed again,
    /// the reader's place held through it.
    private func replace() {
        let held = model.following ? nil : hold()
        layout.invalidateLayout()
        view.layoutIfNeeded()
        if let held {
            restore(held)
            view.layoutIfNeeded()
        }
    }

    /// The new sequence, with the row at the top of a reader's view left
    /// where it was on screen: a page landing, a notice coming or going,
    /// the window dropping rows move nothing the reader is looking at. The
    /// rows that landed are measured as they are placed, which each layout
    /// pass holds the row through.
    private func apply(_ items: [ListItem]) {
        let held = model.following ? nil : hold()
        self.items = items
        layout.items = items
        var snapshot = NSDiffableDataSourceSnapshot<Int, ListItem>()
        snapshot.appendSections([0])
        snapshot.appendItems(items)
        dataSource.apply(snapshot, animatingDifferences: false)
        view.layoutIfNeeded()
        if let held {
            restore(held)
            view.layoutIfNeeded()
        }
    }

    /// The first row on screen (the one crossing the top of the view, or
    /// the first below it), and how far below the top of the list's bounds
    /// it starts, read from the cell itself: where it is drawn, whatever
    /// the layout has since been told. Never a notice: a notice stands
    /// above the oldest row, and a page lands between the two.
    private func hold() -> Held? {
        let top = view.contentOffset.y + view.adjustedContentInset.top
        // Only cells drawn within the bounds: after a jump, the cells the
        // list still holds stand where the reader was.
        let drawn = view.indexPathsForVisibleItems.compactMap { path -> (ListItem, CGRect)? in
            guard let item = dataSource.itemIdentifier(for: path), case .row = item,
                  let frame = view.cellForItem(at: path)?.frame, frame.intersects(view.bounds)
            else { return nil }
            return (item, frame)
        }
        guard let (item, frame) = drawn.filter({ $0.1.maxY > top }).min(by: { $0.1.minY < $1.1.minY })
        else { return nil }
        return Held(item: item, below: frame.minY - view.contentOffset.y)
    }

    /// Puts the held row back where it was on screen, if the rows moved it.
    private func restore(_ held: Held) {
        guard let path = dataSource.indexPath(for: held.item),
              let placed = view.layoutAttributesForItem(at: path)
        else { return }
        let target = placed.frame.minY - held.below
        guard abs(view.contentOffset.y - target) > 0.5 else { return }
        move(to: target)
        // The cells stand at their old frames until the next pass places
        // them: it holds this row, not what it reads from them.
        view.carried = held
    }

    private func move(to y: CGFloat) {
        moving = true
        view.contentOffset.y = y
        moving = false
    }

    private func scrollToNewest() {
        guard let last = items.last, let path = dataSource.indexPath(for: last) else { return }
        let animated = !environment.photographed && !environment.reducesMotion
        if animated {
            view.animatingToNewest = true
            view.scrollToItem(at: path, at: .bottom, animated: true)
        } else {
            view.layoutIfNeeded()
            move(to: view.bottomOffset)
        }
    }

    // MARK: - Where the reader is

    func scrollViewWillBeginDragging(_ scrollView: UIScrollView) {
        userScrolling = true
    }

    func scrollViewDidScroll(_ scrollView: UIScrollView) {
        // A scroll is followed by a layout pass, which publishes the moved
        // frames. A move the list made itself (the bottom kept, an inset
        // changing) says nothing about where the reader wants to be.
        stale = true
        guard !moving, !view.pinning else { return }
        let atBottom = view.atBottom
        defer { wasAtBottom = atBottom }
        if userScrolling {
            // A reader dragging away from the newest row stops following
            // at once, so rows arriving meanwhile are held, not drawn.
            if !atBottom { model.reading(atNewest: false) }
        } else if atBottom, !wasAtBottom {
            model.reading(atNewest: true)
        }
    }

    func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate: Bool) {
        if !willDecelerate { cameToRest() }
    }

    func scrollViewDidEndDecelerating(_ scrollView: UIScrollView) {
        cameToRest()
    }

    private func cameToRest() {
        userScrolling = false
        model.reading(atNewest: view.atBottom)
    }

    func scrollViewDidEndScrollingAnimation(_ scrollView: UIScrollView) {
        view.animatingToNewest = false
        view.setNeedsLayout()
    }

    /// How near the oldest row the rows coming on screen are when the
    /// reader has reached the top: a page is asked for before they see it.
    static let nearTop = 20

    /// The oldest rows coming on screen: a reader in history has scrolled
    /// up to them, or the whole chat fits and older rows would show above
    /// it. A follower of a longer chat sees them only on the way to the
    /// bottom, which is not reaching the top.
    func collectionView(
        _ collectionView: UICollectionView, willDisplay cell: UICollectionViewCell, forItemAt path: IndexPath
    ) {
        guard path.item < Self.nearTop else { return }
        let inset = view.adjustedContentInset
        let fits = view.contentSize.height + inset.top + inset.bottom <= view.bounds.height
        if !model.following || fits { model.reachedTop() }
    }

    func collectionView(
        _ collectionView: UICollectionView, didEndDisplaying cell: UICollectionViewCell,
        forItemAt path: IndexPath
    ) {
        for (item, entry) in declared where entry.cell.value === cell { declared[item] = nil }
        stale = true
    }

    // MARK: - What the cells declare

    private func rooted(at origin: CGPoint, for item: ListItem) {
        roots[item] = origin
        stale = true
        view.setNeedsLayout()
    }

    private func declare(_ elements: [IdentifiedElement], for item: ListItem, in cell: UICollectionViewCell?) {
        if let cell, !elements.isEmpty {
            declared[item] = (Weak(cell), elements)
        } else {
            declared[item] = nil
        }
        stale = true
        view.setNeedsLayout()
    }

    private func publishIfStale() {
        guard stale else { return }
        stale = false
        publish()
    }

    /// Every declared element in row order, its frame moved from the cell
    /// to the window: hosted content reports where it is in its cell, and
    /// the cell moves as the list scrolls.
    private func publish() {
        guard !declared.isEmpty || !reported.elements.isEmpty else { return }
        var all: [IdentifiedElement] = []
        for item in items {
            guard let entry = declared[item], let cell = entry.cell.value, cell.window != nil else { continue }
            let root = roots[item] ?? .zero
            for element in entry.elements {
                let frame = element.frame == .zero
                    ? CGRect.zero
                    : cell.contentView.convert(
                        element.frame.offsetBy(dx: -root.x, dy: -root.y), to: nil)
                all.append(IdentifiedElement(
                    identifier: element.identifier, label: element.label, value: element.value,
                    frame: frame, enabled: element.enabled))
            }
        }
        if reported.elements != all { reported.elements = all }
    }
}

private struct CellContent: View {
    let model: ChatModel
    let item: ListItem
    let notice: FeedNotice?
    let landing: FeedLanding?

    var body: some View {
        switch item {
        case .row(let id):
            RowCellView(cell: model.cell(for: id), model: model)
        case .notice:
            if let notice { FeedNoticeView(notice: notice) }
        case .landing:
            if let landing { FeedLandingView(landing: landing) }
        }
    }
}

private final class Weak<T: AnyObject> {
    weak var value: T?
    init(_ value: T) { self.value = value }
}
