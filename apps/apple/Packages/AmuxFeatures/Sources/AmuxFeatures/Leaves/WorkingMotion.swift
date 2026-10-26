import AmuxDesign
import SwiftUI
import UIKit

/// Motion that runs while nothing else changes: the chat's activity rule and
/// a working agent's mark, animated by Core Animation.
///
/// Drawn by SwiftUI, every frame of either was an update of the view graph
/// on the main thread, which held a phone's main thread about a sixth busy
/// with nothing arriving, and froze the motion whenever that thread was busy
/// with something else, such as building a chat as it opens. A layer
/// animation runs in the render server: the main thread does nothing between
/// frames, and the motion keeps its pace whatever the app is doing. The
/// curves are the drawn ones (``ActivityLine/segment(at:)``, ``MarkMotion``),
/// sampled at 120 a second over one period and anchored to the same clock,
/// so what moves is what SwiftUI drew.
struct WorkingMotion: UIViewRepresentable {
    enum Kind: Equatable {
        /// The segment travelling along the activity line, the line's width.
        case rule
        case mark(WorkingMark)
    }

    let kind: Kind
    let ink: Ramp
    /// The faint ring behind the orbit and the filling marks.
    var ring: Ramp? = nil
    var inkOpacity: CGFloat = 1

    func makeUIView(context: Context) -> WorkingMotionView { WorkingMotionView() }

    func updateUIView(_ view: WorkingMotionView, context: Context) {
        let appearance: Appearance = context.environment.colorScheme == .dark ? .dark : .light
        view.show(
            kind, ink: Self.color(ink.hex(appearance), inkOpacity),
            ring: ring.map { Self.color($0.hex(appearance), 1) })
    }

    private static func color(_ rgb: UInt32, _ alpha: CGFloat) -> CGColor {
        CGColor(
            srgbRed: CGFloat((rgb >> 16) & 0xFF) / 255, green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255, alpha: alpha)
    }
}

final class WorkingMotionView: UIView {
    private var shown: (kind: WorkingMotion.Kind, ink: CGColor, ring: CGColor?)?
    private var builtFor: CGSize = .zero
    /// The instant the running animations were anchored to.
    private(set) var builtAt: MotionClock?

    init() {
        super.init(frame: .zero)
        isUserInteractionEnabled = false
        isAccessibilityElement = false
        // The system takes a backgrounded app's animations off its layers.
        NotificationCenter.default.addObserver(
            self, selector: #selector(rebuild), name: UIApplication.willEnterForegroundNotification,
            object: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func show(_ kind: WorkingMotion.Kind, ink: CGColor, ring: CGColor?) {
        if let shown, shown.kind == kind, shown.ink == ink, shown.ring == ring { return }
        shown = (kind, ink, ring)
        rebuild()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        if bounds.size != builtFor { rebuild() }
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil { rebuild() }
    }

    @objc private func rebuild() {
        layer.sublayers?.forEach { $0.removeFromSuperlayer() }
        builtFor = bounds.size
        guard let shown, window != nil, bounds.width > 0, bounds.height > 0 else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let clock = MotionClock()
        builtAt = clock
        switch shown.kind {
        case .rule: rule(ink: shown.ink, clock: clock)
        case .mark(let style): mark(style, ink: shown.ink, ring: shown.ring, clock: clock)
        }
        CATransaction.commit()
    }

    private func rule(ink: CGColor, clock: MotionClock) {
        let width = bounds.width
        let segment = CALayer()
        segment.backgroundColor = ink
        segment.anchorPoint = CGPoint(x: 0, y: 0.5)
        segment.frame = CGRect(x: 0, y: 0, width: width, height: bounds.height)
        segment.cornerRadius = bounds.height / 2
        layer.addSublayer(segment)
        let period = 2 * ActivityLine.leg
        let at = clock.since1970
        segment.add(
            clock.endless(
                [
                    keyframes("position.x", period: period, from: at) {
                        ActivityLine.segment(at: $0).start * width
                    },
                    keyframes("bounds.size.width", period: period, from: at) {
                        let segment = ActivityLine.segment(at: $0)
                        return (segment.end - segment.start) * width
                    },
                ], period: period, at: at),
            forKey: "motion")
    }

    private func mark(_ style: WorkingMark, ink: CGColor, ring: CGColor?, clock: MotionClock) {
        let size = bounds.size
        let center = CGPoint(x: size.width / 2, y: size.height / 2)
        let at = clock.sinceReference
        switch style {
        case .breathing:
            let side = min(size.width, size.height)
            let dot = CALayer()
            dot.backgroundColor = ink
            dot.frame = CGRect(x: center.x - side / 2, y: center.y - side / 2, width: side, height: side)
            dot.cornerRadius = side / 2
            layer.addSublayer(dot)
            let period = MarkMotion.breathPeriod
            dot.add(
                clock.endless(
                    [
                        keyframes("transform.scale", period: period, from: at) {
                            MarkMotion.breathScale(MarkMotion.breath(at: $0))
                        },
                        keyframes("opacity", period: period, from: at) {
                            CGFloat(MarkMotion.breathOpacity(MarkMotion.breath(at: $0)))
                        },
                    ], period: period, at: at),
                forKey: "motion")
        case .typing:
            let period = MarkMotion.typingPeriod
            for index in 0..<3 {
                let dot = CALayer()
                dot.backgroundColor = ink
                dot.frame = MarkMotion.typingDot(index, phase: 0, center: center)
                dot.cornerRadius = MarkMotion.typingDiameter / 2
                layer.addSublayer(dot)
                let resting = dot.position.y
                dot.add(
                    clock.endless(
                        [
                            keyframes("position.y", period: period, from: at) {
                                resting - 1.5 * MarkMotion.typing(index, at: $0)
                            },
                            keyframes("opacity", period: period, from: at) {
                                CGFloat(MarkMotion.typingOpacity(MarkMotion.typing(index, at: $0)))
                            },
                        ], period: period, at: at),
                    forKey: "motion")
            }
        case .orbit:
            addRing(ring)
            let arc = arcLayer(ink: ink)
            arc.strokeEnd = MarkMotion.orbitArc
            let period = MarkMotion.orbitPeriod
            // One turn a period, from where the drawn mark stands now.
            let turn = CABasicAnimation(keyPath: "transform.rotation.z")
            let from = MarkMotion.orbitTurn(at: at)
            turn.fromValue = from
            turn.toValue = from + 2 * .pi
            arc.add(clock.endless([turn], period: period, at: at, aligned: false), forKey: "motion")
        case .filling:
            addRing(ring)
            let arc = arcLayer(ink: ink)
            arc.setAffineTransform(CGAffineTransform(rotationAngle: -.pi / 2))
            let period = MarkMotion.fillingPeriod
            arc.add(
                clock.endless(
                    [
                        keyframes("strokeStart", period: period, from: at) {
                            CGFloat(MarkMotion.filling(at: $0).start)
                        },
                        keyframes("strokeEnd", period: period, from: at) {
                            CGFloat(MarkMotion.filling(at: $0).end)
                        },
                    ], period: period, at: at),
                forKey: "motion")
        }
    }

    private func addRing(_ color: CGColor?) {
        let ring = CAShapeLayer()
        ring.frame = bounds
        ring.path = CGPath(ellipseIn: MarkMotion.ringBounds(bounds.size), transform: nil)
        ring.fillColor = nil
        ring.strokeColor = color
        ring.lineWidth = MarkMotion.stroke
        layer.addSublayer(ring)
    }

    private func arcLayer(ink: CGColor) -> CAShapeLayer {
        let arc = CAShapeLayer()
        arc.frame = bounds
        arc.path = CGPath(ellipseIn: MarkMotion.ringBounds(bounds.size), transform: nil)
        arc.fillColor = nil
        arc.strokeColor = ink
        arc.lineWidth = MarkMotion.stroke
        arc.lineCap = .round
        layer.addSublayer(arc)
        return arc
    }

    /// `value` sampled at 120 a second over the period that `at` falls in,
    /// as a linear keyframe animation of `keyPath`. The last keyframe is the
    /// value just before the period ends, so a curve that jumps back at the
    /// turn of a period (the filling mark's) jumps rather than sweeping.
    private func keyframes(
        _ keyPath: String, period: Double, from at: Double, value: (Double) -> CGFloat
    ) -> CAKeyframeAnimation {
        let start = at - at.truncatingRemainder(dividingBy: period)
        let steps = Int((period * 120).rounded())
        let animation = CAKeyframeAnimation(keyPath: keyPath)
        animation.values = (0..<steps).map { value(start + period * Double($0) / Double(steps)) }
            + [value(start + period - 1e-6)]
        animation.calculationMode = .linear
        return animation
    }
}

/// The wall clocks the drawn curves read and the media clock layers run on,
/// read together, so an animation can start part-way through its period.
struct MotionClock {
    let media = CACurrentMediaTime()
    let since1970 = Date().timeIntervalSince1970
    var sinceReference: Double { since1970 - Date.timeIntervalBetween1970AndReferenceDate }

    /// The animations, repeating forever with one period, begun as far into
    /// that period as `at` is. `aligned` animations start at the period's
    /// beginning; others start where `at` is.
    func endless(
        _ animations: [CAAnimation], period: Double, at: Double, aligned: Bool = true
    ) -> CAAnimation {
        let group = CAAnimationGroup()
        group.animations = animations
        for animation in animations { animation.duration = period }
        group.duration = period
        group.repeatCount = .infinity
        group.isRemovedOnCompletion = false
        group.beginTime = aligned ? media - at.truncatingRemainder(dividingBy: period) : media
        return group
    }
}
