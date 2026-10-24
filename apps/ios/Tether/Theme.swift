import SwiftUI
import UIKit

/// The app's look in one place.
enum Theme {
    /// The brand green. The asset catalog has the same colour (with a lighter one for dark
    /// mode) as the app's accent; this is the fallback used in code.
    static let accent = Color(red: 0.165, green: 0.490, blue: 0.412)

    /// Green when the link is quick, amber when it is noticeable, red when it drags.
    static func latencyColor(_ ms: Double) -> Color {
        if ms < 40 { return .green }
        if ms < 120 { return .orange }
        return .red
    }
}

/// Small taps of feedback, so a button press or a click feels real.
@MainActor
enum Haptics {
    static func tap() {
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }

    static func click() {
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
    }

    static func success() {
        UINotificationFeedbackGenerator().notificationOccurred(.success)
    }

    static func failure() {
        UINotificationFeedbackGenerator().notificationOccurred(.error)
    }
}
