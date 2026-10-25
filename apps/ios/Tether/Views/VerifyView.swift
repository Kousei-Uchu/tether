import SwiftUI

extension VerifyPrompt: Identifiable {}

/// Shown while pairing: the computer shows the same symbols, and both people confirm.
struct VerifyView: View {
    @EnvironmentObject var model: AppModel
    let prompt: VerifyPrompt

    private let columns = [GridItem(.adaptive(minimum: 72), spacing: 12)]

    var body: some View {
        VStack(spacing: 20) {
            Text("Check \(prompt.peerName)")
                .font(.title2.bold())
            Text("The computer should show exactly these symbols, in this order. If it does not, someone may be interfering, so choose \u{201C}They don\u{2019}t match\u{201D}.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            LazyVGrid(columns: columns, spacing: 14) {
                ForEach(Array(prompt.emoji.enumerated()), id: \.offset) { _, item in
                    VStack(spacing: 4) {
                        Text(item.symbol).font(.system(size: 40))
                        Text(item.name)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Text("Or as numbers: \(prompt.digits)")
                .font(.footnote.monospacedDigit())
                .foregroundStyle(.secondary)

            Spacer()

            VStack(spacing: 10) {
                Button {
                    Haptics.success()
                    model.answerVerify(true)
                } label: {
                    Text("They match").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)

                Button(role: .destructive) {
                    model.answerVerify(false)
                } label: {
                    Text("They don\u{2019}t match").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
            }
        }
        .padding(24)
        .interactiveDismissDisabled()
    }
}
