import SwiftUI

/// Pairing: scan the QR code a computer shows, or type its address and code.
struct PairView: View {
    @EnvironmentObject var model: AppModel
    @Binding var isPresented: Bool
    @State private var mode = 0
    @State private var address = ""
    @State private var code = ""
    @State private var scanError: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                Picker("How", selection: $mode) {
                    Text("Scan QR code").tag(0)
                    Text("Type it").tag(1)
                }
                .pickerStyle(.segmented)
                .padding(.horizontal)

                if mode == 0 {
                    scanner
                } else {
                    form
                }
                Spacer()
            }
            .navigationTitle("Add a computer")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { isPresented = false }
                }
            }
        }
    }

    private var scanner: some View {
        VStack(spacing: 12) {
            ZStack {
                QRScannerView { text in
                    isPresented = false
                    Task { @MainActor in
                        Haptics.success()
                        await model.pair(fromLink: text)
                    }
                } onFailure: { message in
                    scanError = message
                }
                // A frame to aim with, so it is clear where the code goes.
                RoundedRectangle(cornerRadius: 26, style: .continuous)
                    .strokeBorder(.white.opacity(0.9), lineWidth: 3)
                    .frame(width: 230, height: 230)
                    .allowsHitTesting(false)
            }
            .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
            .padding(.horizontal)

            if let scanError {
                Label(scanError, systemImage: "exclamationmark.triangle.fill")
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .padding(.horizontal)
            } else {
                Text("On the computer, open This computer and start sharing, then point the camera at the QR code.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 24)
            }
        }
    }

    private var form: some View {
        Form {
            Section {
                TextField("Address, for example 192.168.1.20", text: $address)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
            } header: {
                Text("Computer")
            } footer: {
                Text("The computer's address on your network, for example 192.168.1.20, or its Tailscale name.")
            }
            Section {
                TextField("XXXX-XXXX-XXXX-XXXX", text: $code)
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                    .font(.system(.body, design: .monospaced))
            } header: {
                Text("Pairing code")
            } footer: {
                Text("You can also paste a pairing link here.")
            }
            Section {
                Button("Pair") {
                    isPresented = false
                    Task {
                        // A pasted link works in either box.
                        if code.hasPrefix("tether://") {
                            await model.pair(fromLink: code)
                        } else {
                            await model.pair(address: address, code: code)
                        }
                    }
                }
                .disabled(code.isEmpty || (address.isEmpty && !code.hasPrefix("tether://")))
            }
        }
    }
}
