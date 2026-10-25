import Foundation
import SwiftUI
import UIKit

/// A computer this phone has paired with, and where to find it.
struct SavedHost: Codable, Identifiable, Hashable {
    var fingerprint: String
    var name: String
    var address: String
    var id: String { fingerprint }
}

enum Phase: Equatable {
    case idle
    case working(String)
    case connected
    case failed(String)
}

/// Everything the screens share: the paired computers, the open session, and the input
/// pipeline. The Rust side (TetherFFI) does the networking and cryptography.
@MainActor
final class AppModel: ObservableObject {
    @Published var hosts: [SavedHost] = []
    @Published var phase: Phase = .idle
    @Published var active: SavedHost?
    @Published var permissions: [String] = []
    @Published var rttMs: Double?
    @Published var notice: String?
    @Published var startupError: String?
    /// Set while pairing asks the person to compare symbols with the computer.
    @Published var verifyPrompt: VerifyPrompt?
    private var promptTask: Task<Void, Never>?

    private var device: Device?
    private let discovery = HostDiscovery()
    private var connection: Connection?
    private var keepAlive: Task<Void, Never>?
    private var inputTask: Task<Void, Never>?
    private var inputSink: AsyncStream<InputAction>.Continuation?
    private var nonce: UInt64 = 0

    // Pointer movement is merged before sending so a fast swipe is a handful of messages.
    private var pendingDx = 0
    private var pendingDy = 0
    private var flushScheduled = false

    init() {
        openDevice()
    }

    var phoneName: String { device?.name() ?? "this phone" }
    var fingerprint: String { device?.fingerprint() ?? "" }
    /// True while pairing or connecting, so a second tap cannot start a second session.
    var isBusy: Bool {
        if case .working = phase { return true }
        return false
    }

    /// The text to show for an error. The generated `TetherError` describes itself as a raw
    /// enum dump, but the Rust side already wrote `message` for the person, so use that.
    static func readable(_ error: Error) -> String {
        if case TetherError.Failed(let message) = error { return message }
        return error.localizedDescription
    }

    var mayControl: Bool { permissions.contains("input") }
    var mayUseMedia: Bool { permissions.contains("media") }

    // MARK: - Setup

    private var supportDirectory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("Tether", isDirectory: true)
    }

    private var hostsFile: URL { supportDirectory.appendingPathComponent("hosts.json") }

    private func openDevice() {
        do {
            try FileManager.default.createDirectory(at: supportDirectory, withIntermediateDirectories: true)
            device = try Device(home: supportDirectory.path, name: UIDevice.current.name)
            loadHosts()
        } catch {
            startupError = AppModel.readable(error)
        }
    }

    private func loadHosts() {
        guard let data = try? Data(contentsOf: hostsFile),
              let saved = try? JSONDecoder().decode([SavedHost].self, from: data) else { return }
        // Only keep hosts the Rust side still trusts.
        let trusted = Set((try? device?.peers().map(\.fingerprint)) ?? [])
        hosts = saved.filter { trusted.contains($0.fingerprint) }
    }

    private func saveHosts() {
        if let data = try? JSONEncoder().encode(hosts) {
            try? data.write(to: hostsFile, options: .atomic)
        }
    }

    // MARK: - Pairing

    /// Pairs from a scanned QR code or a pasted link, then connects.
    func pair(fromLink text: String) async {
        do {
            let info = try parsePairingLink(link: text)
            await pair(address: info.address, code: info.code)
        } catch {
            phase = .failed(AppModel.readable(error))
        }
    }

    func pair(address: String, code: String) async {
        guard let device else { return }
        phase = .working("Pairing...")
        // Pairing waits for both people to compare symbols, so watch for the question.
        promptTask = Task { [weak self] in
            while !Task.isCancelled {
                if let prompt = self?.device?.pendingPrompt(), self?.verifyPrompt?.id != prompt.id {
                    self?.verifyPrompt = prompt
                }
                try? await Task.sleep(nanoseconds: 250_000_000)
            }
        }
        defer {
            promptTask?.cancel()
            promptTask = nil
            verifyPrompt = nil
        }
        do {
            let peer = try await device.pairVerified(address: address.trimmingCharacters(in: .whitespaces), code: code)
            let host = SavedHost(fingerprint: peer.fingerprint, name: peer.name, address: address.trimmingCharacters(in: .whitespaces))
            hosts.removeAll { $0.fingerprint == host.fingerprint }
            hosts.append(host)
            saveHosts()
            await connect(to: host)
        } catch {
            phase = .failed(AppModel.readable(error))
        }
    }

    /// The person's answer to the symbol comparison.
    func answerVerify(_ matches: Bool) {
        if let prompt = verifyPrompt {
            _ = device?.answerPrompt(id: prompt.id, accept: matches)
        }
        verifyPrompt = nil
    }

    func forget(_ host: SavedHost) {
        _ = try? device?.remove(query: host.fingerprint)
        hosts.removeAll { $0.id == host.id }
        saveHosts()
    }

    // MARK: - Session

    func connect(to saved: SavedHost) async {
        guard let device else { return }
        var host = saved
        disconnect()
        phase = .working("Connecting to \(host.name)...")
        do {
            let c: Connection
            do {
                c = try await device.connect(address: host.address)
            } catch {
                // The saved address may be out of date, so look for the computer on the
                // network. If it is not there, report the original failure.
                phase = .working("Looking for \(host.name) on the network...")
                guard let found = await discovery.find(fingerprint: host.fingerprint, timeout: 5),
                      found != host.address else { throw error }
                c = try await device.connect(address: found)
                host.address = found
            }
            guard c.peer().fingerprint == host.fingerprint else {
                try? await c.close()
                phase = .failed("That is not the computer you paired with.")
                return
            }
            if let i = hosts.firstIndex(where: { $0.id == host.id }), hosts[i].address != host.address {
                hosts[i] = host
                saveHosts()
            }
            connection = c
            active = host
            permissions = c.permissions()
            rttMs = nil
            notice = nil
            phase = .connected
            startPipeline(for: c)
        } catch {
            phase = .failed(AppModel.readable(error))
        }
    }

    func disconnect() {
        keepAlive?.cancel()
        keepAlive = nil
        inputSink?.finish()
        inputSink = nil
        inputTask = nil
        if let c = connection {
            connection = nil
            Task { try? await c.close() }
        }
        active = nil
        permissions = []
        rttMs = nil
        if phase == .connected { phase = .idle }
    }

    private func startPipeline(for c: Connection) {
        // Pings keep the host from dropping an idle session, and carry permission changes and
        // the host's notices back to the phone.
        keepAlive = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                do {
                    self.nonce &+= 1
                    let rtt = try await c.ping(nonce: self.nonce)
                    self.rttMs = rtt
                    self.permissions = c.permissions()
                    if let latest = await c.takeNotices().last { self.notice = latest }
                } catch {
                    self.lost(error, from: c)
                    return
                }
                try? await Task.sleep(nanoseconds: 3_000_000_000)
            }
        }

        // One consumer sends actions in the order they were made, so a button press can never
        // overtake the move before it.
        let (stream, sink) = AsyncStream<InputAction>.makeStream(bufferingPolicy: .unbounded)
        inputSink = sink
        inputTask = Task { [weak self] in
            for await action in stream {
                do {
                    try await c.sendInput(action: action)
                } catch {
                    self?.lost(error, from: c)
                    return
                }
            }
        }
    }

    /// The session `c` failed. Only acts if `c` is still the open one: a late error from a
    /// session that was already replaced must not tear down its successor.
    private func lost(_ error: Error, from c: Connection) {
        guard phase == .connected, connection === c else { return }
        disconnect()
        phase = .failed(AppModel.readable(error))
    }

    // MARK: - Input

    func send(_ action: InputAction) {
        inputSink?.yield(action)
    }

    func click(_ button: String) {
        flushPointer()
        send(.button(button: button, down: true))
        send(.button(button: button, down: false))
    }

    func buttonDown(_ button: String) {
        flushPointer()
        send(.button(button: button, down: true))
    }

    func buttonUp(_ button: String) {
        flushPointer()
        send(.button(button: button, down: false))
    }

    func moveBy(dx: Int, dy: Int) {
        pendingDx += dx
        pendingDy += dy
        guard !flushScheduled else { return }
        flushScheduled = true
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: 8_000_000)
            self?.flushPointer()
        }
    }

    private func flushPointer() {
        flushScheduled = false
        guard pendingDx != 0 || pendingDy != 0 else { return }
        send(.delta(dx: Int32(clamping: pendingDx), dy: Int32(clamping: pendingDy)))
        pendingDx = 0
        pendingDy = 0
    }

    func scroll(lines: Int, horizontal: Int = 0) {
        flushPointer()
        send(.scroll(dx: Int32(clamping: horizontal), dy: Int32(clamping: lines)))
    }

    /// Presses and releases one key, for example `Enter` or `MediaPlayPause`.
    func tap(key code: String, modifiers: [String] = []) {
        flushPointer()
        for m in modifiers { send(.key(code: m, down: true)) }
        send(.key(code: code, down: true))
        send(.key(code: code, down: false))
        for m in modifiers.reversed() { send(.key(code: m, down: false)) }
    }

    /// Types what the on-screen keyboard produced. Letters and digits become real key presses
    /// when a modifier is held, so shortcuts like Ctrl+C work. Everything else is typed text.
    func type(_ text: String, modifiers: [String] = []) {
        flushPointer()
        if modifiers.isEmpty {
            send(.text(text: text))
            return
        }
        for character in text {
            if let code = AppModel.physicalKey(for: character) {
                tap(key: code, modifiers: modifiers)
            } else {
                send(.text(text: String(character)))
            }
        }
    }

    static func physicalKey(for character: Character) -> String? {
        guard character.isASCII else { return nil }
        if character.isLetter { return "Key" + character.uppercased() }
        if character.isNumber { return "Digit" + String(character) }
        if character == " " { return "Space" }
        return nil
    }
}
