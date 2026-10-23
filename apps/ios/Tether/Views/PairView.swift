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
            QRScannerView { text in
                isPresented = false
                Task { await model.pair(fromLink: text) }
            } onFailure: { message in
                scanError = message
            }
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .padding(.horizontal)

            if let scanError {
                Text(scanError).foregroundStyle(.red).padding(.horizontal)
            } else {
                Text("On the computer, start accepting connections and point the camera at the QR code.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal)
            }
        }
    }

    private var form: some View {
        Form {
            Section("Computer") {
                TextField("Address, for example 192.168.1.20", text: $address)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
            }
            Section("Pairing code") {
                TextField("XXXX-XXXX-XXXX-XXXX", text: $code)
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                    .font(.system(.body, design: .monospaced))
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
