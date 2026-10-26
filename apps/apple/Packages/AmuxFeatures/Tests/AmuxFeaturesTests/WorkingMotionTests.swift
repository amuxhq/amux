import QuartzCore
import UIKit
import XCTest
@testable import AmuxFeatures

/// The animated rule and marks stand where the drawn curves put them: each
/// layer's clock is stopped at instants through a period, and what the
/// presentation shows is compared with the curve at that wall-clock time.
@MainActor
final class WorkingMotionTests: XCTestCase {
    private let ink = CGColor(srgbRed: 0.2, green: 0.3, blue: 0.4, alpha: 0.7)
    private let ring = CGColor(srgbRed: 0.8, green: 0.8, blue: 0.8, alpha: 1)
    /// Instants into the run, in seconds, spread over every period.
    private let instants: [Double] = [0, 0.13, 0.4, 0.71, 1.1, 1.52, 1.9, 2.35, 2.7, 3.05, 4.4]

    func testTheRuleTravelsAsTheLineDrawsIt() {
        let (view, clock) = shown(.rule, size: CGSize(width: 300, height: 1))
        let segment = view.layer.sublayers![0]
        XCTAssertEqual(segment.backgroundColor, ink)
        for instant in instants {
            frozen(view, clock, after: instant)
            let expected = ActivityLine.segment(at: clock.since1970 + instant)
            let shown = segment.presentation()!.frame
            XCTAssertEqual(shown.minX, expected.start * 300, accuracy: 0.5, "start at \(instant) s")
            XCTAssertEqual(shown.maxX, expected.end * 300, accuracy: 0.5, "end at \(instant) s")
        }
    }

    func testTheBreathingMarkSwellsAndFadesAsDrawn() {
        let (view, clock) = shown(.mark(.breathing), size: CGSize(width: 10, height: 10))
        let dot = view.layer.sublayers![0]
        for instant in instants {
            frozen(view, clock, after: instant)
            let phase = MarkMotion.breath(at: clock.sinceReference + instant)
            let shown = dot.presentation()!
            XCTAssertEqual(shown.frame.width, 10 * MarkMotion.breathScale(phase), accuracy: 0.05)
            XCTAssertEqual(Double(shown.opacity), MarkMotion.breathOpacity(phase), accuracy: 0.01)
        }
    }

    func testTheTypingDotsRiseInTurnAsDrawn() {
        let (view, clock) = shown(.mark(.typing), size: CGSize(width: 14, height: 10))
        let center = CGPoint(x: 7, y: 5)
        for instant in instants {
            frozen(view, clock, after: instant)
            for (index, dot) in view.layer.sublayers!.enumerated() {
                let phase = MarkMotion.typing(index, at: clock.sinceReference + instant)
                let shown = dot.presentation()!
                let drawn = MarkMotion.typingDot(index, phase: phase, center: center)
                XCTAssertEqual(shown.frame.minX, drawn.minX, accuracy: 0.01)
                XCTAssertEqual(shown.frame.minY, drawn.minY, accuracy: 0.05)
                XCTAssertEqual(Double(shown.opacity), MarkMotion.typingOpacity(phase), accuracy: 0.02)
            }
        }
    }

    func testTheOrbitTurnsAsDrawn() {
        let (view, clock) = shown(.mark(.orbit), size: CGSize(width: 10, height: 10))
        let arc = view.layer.sublayers![1] as! CAShapeLayer
        XCTAssertEqual(arc.strokeEnd, MarkMotion.orbitArc)
        for instant in instants {
            frozen(view, clock, after: instant)
            let transform = arc.presentation()!.affineTransform()
            let turn = atan2(Double(transform.b), Double(transform.a))
            let expected = MarkMotion.orbitTurn(at: clock.sinceReference + instant)
            XCTAssertEqual(
                remainder(turn - expected, 2 * .pi), 0, accuracy: 0.001, "turn at \(instant) s")
        }
    }

    func testTheFillingRingFillsAndEmptiesAsDrawn() {
        let (view, clock) = shown(.mark(.filling), size: CGSize(width: 10, height: 10))
        let arc = view.layer.sublayers![1] as! CAShapeLayer
        for instant in instants {
            frozen(view, clock, after: instant)
            let expected = MarkMotion.filling(at: clock.sinceReference + instant)
            let shown = arc.presentation() as! CAShapeLayer
            XCTAssertEqual(Double(shown.strokeStart), expected.start, accuracy: 0.01)
            XCTAssertEqual(Double(shown.strokeEnd), expected.end, accuracy: 0.01)
        }
    }

    /// The motion in a window of its own, built.
    private func shown(_ kind: WorkingMotion.Kind, size: CGSize) -> (WorkingMotionView, MotionClock) {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 100))
        let view = WorkingMotionView()
        view.frame = CGRect(origin: .zero, size: size)
        window.addSubview(view)
        window.isHidden = false
        view.show(kind, ink: ink, ring: ring)
        view.layoutIfNeeded()
        addTeardownBlock { window.isHidden = true }
        return (view, view.builtAt!)
    }

    /// Stops the view's layers at `instant` seconds after the animations
    /// were anchored.
    @discardableResult
    private func frozen(_ view: UIView, _ clock: MotionClock, after instant: Double) -> CFTimeInterval {
        view.layer.speed = 0
        view.layer.timeOffset = clock.media + instant
        CATransaction.flush()
        return view.layer.timeOffset
    }
}
