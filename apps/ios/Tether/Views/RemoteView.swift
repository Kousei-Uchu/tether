import SwiftUI

/// The controller: a trackpad, a keyboard, and media keys.
struct RemoteView: View {
    @EnvironmentObject var model: AppModel
    @State private var keyboardOpen = false
    @State private var sticky: Set<String> = []

    private let modifierKeys: [(label: String, code: String)] = [
        ("Ctrl", "ControlLeft"), ("Alt", "AltLeft"), ("Cmd/Win", "MetaLeft"), ("Shift", "ShiftLeft"),
    ]

    var body: some View {
        VStack(spacing: 10) {
            header
            if let notice = model.notice {
                Text(notice)
                    .font(.footnote)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.yellow.opacity(0.25), in: RoundedRectangle(cornerRadius: 8))
                    .onTapGesture { model.notice = nil }
            }

            ZStack {
                TrackpadView()
                    .opacity(model.mayControl ? 1 : 0.35)
                    .allowsHitTesting(model.mayControl)
                if !model.mayControl {
                    Text("This phone is not allowed to control \(model.active?.name ?? "this computer"). Ask the host to give it the input permission.")
                        .multilineTextAlignment(.center)
                        .padding()
                }
            }

            keysRow
            mediaRow
        }
        .padding(.horizontal, 12)
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
        HStack {
            VStack(alignment: .leading) {
                Text(model.active?.name ?? "").font(.headline)
                if let rtt = model.rttMs {
                    Text(String(format: "%.0f ms", rtt)).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
            Button(keyboardOpen ? "Hide keyboard" : "Keyboard") { keyboardOpen.toggle() }
                .disabled(!model.mayControl)
            Button("Disconnect", role: .destructive) { model.disconnect() }
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
                        if sticky.contains(m.code) { sticky.remove(m.code) } else { sticky.insert(m.code) }
                    }
                    .buttonStyle(.bordered)
                    .tint(sticky.contains(m.code) ? .accentColor : .gray)
                }
                keyButton("Left", "ArrowLeft")
                keyButton("Up", "ArrowUp")
                keyButton("Down", "ArrowDown")
                keyButton("Right", "ArrowRight")
                keyButton("Del", "Delete")
                keyButton("Home", "Home")
                keyButton("End", "End")
            }
        }
        .disabled(!model.mayControl)
    }

    private func keyButton(_ label: String, _ code: String) -> some View {
        Button(label) {
            model.tap(key: code, modifiers: activeModifiers())
            sticky.removeAll()
        }
        .buttonStyle(.bordered)
    }

    private var mediaRow: some View {
        HStack(spacing: 14) {
            mediaButton("backward.fill", "MediaTrackPrevious")
            mediaButton("playpause.fill", "MediaPlayPause")
            mediaButton("forward.fill", "MediaTrackNext")
            Spacer()
            mediaButton("speaker.slash.fill", "AudioVolumeMute")
            mediaButton("speaker.minus.fill", "AudioVolumeDown")
            mediaButton("speaker.plus.fill", "AudioVolumeUp")
        }
        .disabled(!model.mayUseMedia)
        .opacity(model.mayUseMedia ? 1 : 0.35)
    }

    private func mediaButton(_ symbol: String, _ code: String) -> some View {
        Button {
            model.tap(key: code)
        } label: {
            Image(systemName: symbol).frame(width: 36, height: 36)
        }
        .buttonStyle(.bordered)
    }
}
