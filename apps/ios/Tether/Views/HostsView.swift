import SwiftUI

/// The list of paired computers. Tap one to connect.
struct HostsView: View {
    @EnvironmentObject var model: AppModel
    @State private var showPairing = false
    @StateObject private var discovery = HostDiscovery()

    var body: some View {
        NavigationStack {
            List {
                if let error = model.startupError {
                    Section {
                        Label("Tether could not start: \(error)", systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.red)
                    }
                }

                statusSection

                if model.hosts.isEmpty {
                    Section {
                        emptyState
                    }
                    .listRowBackground(Color.clear)
                } else {
                    Section("Your computers") {
                        ForEach(model.hosts) { host in
                            Button {
                                Haptics.tap()
                                Task { await model.connect(to: host) }
                            } label: {
                                HostRow(host: host)
                            }
                            .disabled(model.isBusy)
                            .swipeActions {
                                Button("Forget", role: .destructive) { model.forget(host) }
                            }
                        }
                    }
                }

                if !discovery.nearby.isEmpty {
                    Section {
                        ForEach(discovery.nearby) { nearby in
                            nearbyRow(nearby)
                        }
                    } header: {
                        Text("On this network")
                    } footer: {
                        Text("Computers running Tether on the same Wi-Fi. To pair a new one, scan its QR code.")
                    }
                }

                Section {
                    Label(model.phoneName, systemImage: "iphone")
                    Text(model.fingerprint)
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                } header: {
                    Text("This phone")
                } footer: {
                    Text("Computers show this code when you pair, so you can check it is really your phone.")
                }
            }
            .sheet(item: $model.verifyPrompt) { prompt in
                VerifyView(prompt: prompt)
                    .environmentObject(model)
            }
            .onAppear { discovery.start() }
            .onDisappear { discovery.stop() }
            .navigationTitle("Tether")
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        showPairing = true
                    } label: {
                        Label("Add a computer", systemImage: "plus")
                    }
                    .disabled(model.isBusy)
                }
            }
            .sheet(isPresented: $showPairing) {
                PairView(isPresented: $showPairing)
                    .environmentObject(model)
            }
        }
    }

    @ViewBuilder
    private func nearbyRow(_ nearby: NearbyHost) -> some View {
        let saved = model.hosts.first { $0.fingerprint == nearby.fingerprint }
        Button {
            guard var host = saved else { return }
            if let address = nearby.address { host.address = address }
            Haptics.tap()
            Task { await model.connect(to: host) }
        } label: {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(nearby.name)
                        .font(.headline)
                        .foregroundStyle(.primary)
                    Text([nearby.platform, saved == nil ? "Not paired" : "Paired"]
                        .filter { !$0.isEmpty }
                        .joined(separator: " · "))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if saved != nil {
                    Image(systemName: "chevron.right")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(.tertiary)
                }
            }
            .contentShape(Rectangle())
        }
        .disabled(model.isBusy || saved == nil)
    }

    private var emptyState: some View {
        VStack(spacing: 14) {
            Image(systemName: "desktopcomputer")
                .font(.system(size: 46, weight: .regular))
                .foregroundStyle(Theme.accent)
            Text("Control your computers")
                .font(.title2.bold())
            Text("Pair with a computer to use this phone as its trackpad, keyboard and media remote.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button {
                showPairing = true
            } label: {
                Text("Add a computer")
                    .frame(maxWidth: 240)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(model.isBusy)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 28)
    }

    @ViewBuilder
    private var statusSection: some View {
        switch model.phase {
        case .working(let text):
            Section {
                HStack(spacing: 12) {
                    ProgressView()
                    Text(text)
                }
            }
        case .failed(let message):
            Section {
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
            }
        default:
            EmptyView()
        }
    }
}

/// One computer in the list.
private struct HostRow: View {
    let host: SavedHost

    private var initial: String {
        let first = host.name.trimmingCharacters(in: .whitespaces).prefix(1)
        return first.isEmpty ? "?" : first.uppercased()
    }

    var body: some View {
        HStack(spacing: 12) {
            ZStack {
                Circle().fill(Theme.accent.opacity(0.15))
                Text(initial)
                    .font(.headline)
                    .foregroundStyle(Theme.accent)
            }
            .frame(width: 40, height: 40)

            VStack(alignment: .leading, spacing: 2) {
                Text(host.name)
                    .font(.headline)
                    .foregroundStyle(.primary)
                Text(host.address)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Image(systemName: "chevron.right")
                .font(.footnote.weight(.semibold))
                .foregroundStyle(.tertiary)
        }
        .contentShape(Rectangle())
    }
}
