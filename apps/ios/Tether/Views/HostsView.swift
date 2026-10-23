import SwiftUI

/// The list of paired computers. Tap one to connect.
struct HostsView: View {
    @EnvironmentObject var model: AppModel
    @State private var showPairing = false

    var body: some View {
        NavigationStack {
            List {
                if let error = model.startupError {
                    Section {
                        Text("Tether could not start: \(error)")
                            .foregroundStyle(.red)
                    }
                }

                statusSection

                Section("Your computers") {
                    if model.hosts.isEmpty {
                        Text("Nothing paired yet. Tap Add a computer, then scan the QR code it shows.")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(model.hosts) { host in
                        Button {
                            Task { await model.connect(to: host) }
                        } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(host.name).font(.headline)
                                Text(host.address).font(.footnote).foregroundStyle(.secondary)
                            }
                        }
                        .swipeActions {
                            Button("Forget", role: .destructive) { model.forget(host) }
                        }
                    }
                }

                Section {
                    Button("Add a computer") { showPairing = true }
                }

                Section("This phone") {
                    LabeledContent("Name", value: model.phoneName)
                    Text(model.fingerprint)
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
            }
            .navigationTitle("Tether")
            .sheet(isPresented: $showPairing) {
                PairView(isPresented: $showPairing)
                    .environmentObject(model)
            }
        }
    }

    @ViewBuilder
    private var statusSection: some View {
        switch model.phase {
        case .working(let text):
            Section { HStack { ProgressView(); Text(text).padding(.leading, 8) } }
        case .failed(let message):
            Section { Text(message).foregroundStyle(.red) }
        default:
            EmptyView()
        }
    }
}
