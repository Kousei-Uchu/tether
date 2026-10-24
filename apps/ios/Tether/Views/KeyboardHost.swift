import SwiftUI
import UIKit

/// An invisible view that owns the iOS keyboard. Text from it goes to the computer as typed
/// text, and Return and Delete go as key presses.
struct KeyboardHost: UIViewRepresentable {
    @Binding var isOpen: Bool
    var onText: (String) -> Void
    var onKey: (String) -> Void

    func makeUIView(context: Context) -> CaptureView {
        let view = CaptureView()
        view.isUserInteractionEnabled = false
        return view
    }

    func updateUIView(_ view: CaptureView, context: Context) {
        view.onText = onText
        view.onKey = onKey
        view.onClosed = { DispatchQueue.main.async { isOpen = false } }
        if isOpen {
            if !view.isFirstResponder { view.becomeFirstResponder() }
        } else if view.isFirstResponder {
            view.resignFirstResponder()
        }
    }

    final class CaptureView: UIView, UIKeyInput {
        var onText: ((String) -> Void)?
        var onKey: ((String) -> Void)?
        var onClosed: (() -> Void)?

        override var canBecomeFirstResponder: Bool { true }
        var hasText: Bool { true }

        // The host's own layout decides what each character means, so turn off everything
        // that would rewrite what the person types.
        var autocorrectionType: UITextAutocorrectionType = .no
        var autocapitalizationType: UITextAutocapitalizationType = .none
        var spellCheckingType: UITextSpellCheckingType = .no
        var smartQuotesType: UITextSmartQuotesType = .no
        var smartDashesType: UITextSmartDashesType = .no
        var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
        var keyboardType: UIKeyboardType = .default

        func insertText(_ text: String) {
            if text == "\n" { onKey?("Enter") } else { onText?(text) }
        }

        func deleteBackward() { onKey?("Backspace") }

        @discardableResult
        override func resignFirstResponder() -> Bool {
            let result = super.resignFirstResponder()
            onClosed?()
            return result
        }
    }
}
