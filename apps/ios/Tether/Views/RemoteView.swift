import SwiftUI

/// The controller: a trackpad, a keyboard, and media keys.
struct RemoteView: View {
    @EnvironmentObject var model: AppModel
    @State private var keyboardOpen = false
    @State private var showFiles = false
    @State private var sticky: Set<String> = []

    private let modifierKeys: [(label: String, code: String)] = [
        ("Ctrl", "ControlLeft"), ("Alt", "AltLeft"), ("Cmd/Win", "MetaLeft"), ("Shift", "ShiftLeft"),
    ]

    var body: some View {
        VStack(spacing: 12) {
            header
            if let notice = model.notice {
                Label(notice, systemImage: "info.circle.fill")
                    .font(.footnote)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.accent.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
                    .onTapGesture { model.notice = nil }
            }

            ZStack {
                TrackpadView()
                    .opacity(model.mayControl ? 1 : 0.35)
                    .allowsHitTesting(model.mayControl)
                if model.mayControl {
                    Text("Drag to move\nTap to click\nTwo fingers to scroll")
                        .font(.footnote)
                        .foregroundStyle(.tertiary)
                        .multilineTextAlignment(.center)
                        .allowsHitTesting(false)
                } else {
                    VStack(spacing: 8) {
                        Image(systemName: "hand.raised.slash")
                            .font(.title)
                        Text("This phone is not allowed to control \(model.active?.name ?? "this computer"). Ask the host to give it the input permission.")
                            .multilineTextAlignment(.center)
                    }
                    .foregroundStyle(.secondary)
                    .padding()
                }
            }

            keysRow
            mediaRow
        }
        .padding(.horizontal, 14)
        .sheet(isPresented: $showFiles) {
            FilesView().environmentObject(model)
        }
        .padding(.bottom, 8)
        .background(KeyboardHost(isOpen: $keyboardOpen) { text in
            model.type(text, modifiers: activeModifiers())
            sticky.removeAll()
        } onKey: { code in
            model.tap(key: code, modifiers: activeModifiers())
            sticky.removeAll()
        })
    }

    private func activeModifiers() -> [String] {
        modifierKeys.map(\.code).filter { sticky.contains($0) }
    }

    private var header: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(model.active?.name ?? "")
                    .font(.headline)
                if let rtt = model.rttMs {
                    HStack(spacing: 6) {
                        Circle()
                            .fill(Theme.latencyColor(rtt))
                            .frame(width: 8, height: 8)
                        Text(String(format: "%.0f ms", rtt))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                } else {
                    Text("Connected")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            if model.mayFiles {
                Button {
                    Haptics.tap()
                    showFiles = true
                } label: {
                    Image(systemName: "folder")
                        .frame(width: 22, height: 22)
                }
                .buttonStyle(.bordered)
                .accessibilityLabel("Files")
            }
            Button {
                Haptics.tap()
                keyboardOpen.toggle()
            } label: {
                Image(systemName: keyboardOpen ? "keyboard.chevron.compact.down" : "keyboard")
                    .frame(width: 22, height: 22)
            }
            .buttonStyle(.bordered)
            .buttonBorderShape(.capsule)
            .disabled(!model.mayControl)
            .accessibilityLabel(keyboardOpen ? "Hide keyboard" : "Show keyboard")

            Button(role: .destructive) {
                Haptics.tap()
                model.disconnect()
            } label: {
                Image(systemName: "power")
                    .frame(width: 22, height: 22)
            }
            .buttonStyle(.bordered)
            .buttonBorderShape(.capsule)
            .accessibilityLabel("Disconnect")
        }
        .padding(.top, 8)
    }

    private var keysRow: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                keyButton("Esc", "Escape")
                keyButton("Tab", "Tab")
                ForEach(modifierKeys, id: \.code) { m in
                    Button(m.label) {
                        Haptics.tap()
                        if sticky.contains(m.code) { sticky.remove(m.code) } else { sticky.insert(m.code) }
                    }
                    .buttonStyle(.bordered)
                    .buttonBorderShape(.capsule)
                    .tint(sticky.contains(m.code) ? Theme.accent : .gray)
                }
                keyButton("Left", "ArrowLeft")
                keyButton("Up", "ArrowUp")
                keyButton("Down", "ArrowDown")
                keyButton("Right", "ArrowRight")
                keyButton("Del", "Delete")
                keyButton("Home", "Home")
                keyButton("End", "End")
            }
            .padding(.vertical, 2)
        }
        .disabled(!model.mayControl)
    }

    private func keyButton(_ label: String, _ code: String) -> some View {
        Button(label) {
            Haptics.tap()
            model.tap(key: code, modifiers: activeModifiers())
            sticky.removeAll()
        }
        .buttonStyle(.bordered)
        .buttonBorderShape(.capsule)
    }

    private var mediaRow: some View {
        HStack(spacing: 10) {
            mediaButton("backward.fill", "MediaTrackPrevious", "Previous track")
            mediaButton("playpause.fill", "MediaPlayPause", "Play or pause")
            mediaButton("forward.fill", "MediaTrackNext", "Next track")
            Spacer()
            mediaButton("speaker.slash.fill", "AudioVolumeMute", "Mute")
            mediaButton("speaker.minus.fill", "AudioVolumeDown", "Volume down")
            mediaButton("speaker.plus.fill", "AudioVolumeUp", "Volume up")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 18))
        .disabled(!model.mayUseMedia)
        .opacity(model.mayUseMedia ? 1 : 0.35)
    }

    private func mediaButton(_ symbol: String, _ code: String, _ name: String) -> some View {
        Button {
            Haptics.tap()
            model.tap(key: code)
        } label: {
            Image(systemName: symbol).frame(width: 34, height: 34)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(name)
    }
}
