import SwiftUI
import UIKit

/// A touch surface that works like a laptop trackpad:
///   one finger drag      moves the pointer
///   tap                  left click
///   two finger tap       right click
///   two finger drag      scrolls
///   tap, then hold+drag  drags (press stays down until you lift)
struct TrackpadView: UIViewRepresentable {
    @EnvironmentObject var model: AppModel

    func makeCoordinator() -> Coordinator { Coordinator(model: model) }

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.backgroundColor = UIColor.secondarySystemBackground
        view.layer.cornerRadius = 14
        view.isMultipleTouchEnabled = true

        let c = context.coordinator

        let pan = UIPanGestureRecognizer(target: c, action: #selector(Coordinator.pan(_:)))
        pan.minimumNumberOfTouches = 1
        pan.maximumNumberOfTouches = 1

        let scroll = UIPanGestureRecognizer(target: c, action: #selector(Coordinator.scroll(_:)))
        scroll.minimumNumberOfTouches = 2
        scroll.maximumNumberOfTouches = 2

        let tap = UITapGestureRecognizer(target: c, action: #selector(Coordinator.tap))
        let twoFingerTap = UITapGestureRecognizer(target: c, action: #selector(Coordinator.twoFingerTap))
        twoFingerTap.numberOfTouchesRequired = 2

        let hold = UILongPressGestureRecognizer(target: c, action: #selector(Coordinator.hold(_:)))
        hold.minimumPressDuration = 0.35
        hold.allowableMovement = 10_000
        hold.numberOfTouchesRequired = 1

        pan.delegate = c
        hold.delegate = c
        for g in [pan, scroll, tap, twoFingerTap, hold] as [UIGestureRecognizer] {
            view.addGestureRecognizer(g)
        }
        return view
    }

    func updateUIView(_ view: UIView, context: Context) {
        context.coordinator.model = model
    }

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        var model: AppModel
        private var dragging = false
        private var lastHold: CGPoint = .zero
        private var carryX: CGFloat = 0
        private var carryY: CGFloat = 0
        private var scrollCarry: CGFloat = 0

        /// Points of finger travel per line of scrolling.
        private let pointsPerLine: CGFloat = 14
        private let naturalScrolling = true

        init(model: AppModel) { self.model = model }

        func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool {
            true
        }

        /// Faster swipes travel further, like a real trackpad. Fractions carry over so slow,
        /// small movements are not lost to rounding.
        private func move(_ delta: CGPoint) {
            let speed = hypot(delta.x, delta.y)
            let gain = 1.2 + min(speed / 18, 2.0)
            carryX += delta.x * gain
            carryY += delta.y * gain
            let dx = Int(carryX.rounded(.towardZero))
            let dy = Int(carryY.rounded(.towardZero))
            carryX -= CGFloat(dx)
            carryY -= CGFloat(dy)
            if dx != 0 || dy != 0 { Task { @MainActor in self.model.moveBy(dx: dx, dy: dy) } }
        }

        @objc func pan(_ g: UIPanGestureRecognizer) {
            // While a drag is held, the long press recognizer supplies the movement.
            guard !dragging, g.state == .changed else { return }
            let t = g.translation(in: g.view)
            g.setTranslation(.zero, in: g.view)
            move(t)
        }

        @objc func hold(_ g: UILongPressGestureRecognizer) {
            let p = g.location(in: g.view)
            switch g.state {
            case .began:
                dragging = true
                lastHold = p
                Task { @MainActor in self.model.buttonDown("left") }
            case .changed:
                move(CGPoint(x: p.x - lastHold.x, y: p.y - lastHold.y))
                lastHold = p
            case .ended, .cancelled, .failed:
                if dragging {
                    dragging = false
                    Task { @MainActor in self.model.buttonUp("left") }
                }
            default:
                break
            }
        }

        @objc func scroll(_ g: UIPanGestureRecognizer) {
            guard g.state == .changed else { return }
            let t = g.translation(in: g.view)
            g.setTranslation(.zero, in: g.view)
            // Dragging fingers up scrolls the page down when natural scrolling is on.
            scrollCarry += (naturalScrolling ? -t.y : t.y) / pointsPerLine
            let lines = Int(scrollCarry.rounded(.towardZero))
            scrollCarry -= CGFloat(lines)
            if lines != 0 { Task { @MainActor in self.model.scroll(lines: lines) } }
        }

        @objc func tap() { Task { @MainActor in self.model.click("left") } }
        @objc func twoFingerTap() { Task { @MainActor in self.model.click("right") } }
    }
}
