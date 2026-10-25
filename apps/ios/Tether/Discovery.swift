import Foundation
import Network

/// A Tether computer seen on the local network. Nothing about it is authenticated: the
/// fingerprint comes from its advertisement, and connecting still checks the real key.
struct NearbyHost: Identifiable, Hashable {
    var fingerprint: String
    var name: String
    var platform: String
    /// `ip:port`, filled in once the Bonjour name has been resolved.
    var address: String?
    var id: String { fingerprint }
}

/// Guards a continuation that several callbacks might try to finish. Everything runs on
/// the main queue, so a plain flag is enough.
private final class Once: @unchecked Sendable {
    var done = false
}

/// Finds Tether computers through Bonjour (`_tether._tcp`). The Rust core does its own
/// discovery on desktops, but iOS only allows raw multicast with a special entitlement, so
/// the phone uses the system API and reads the same TXT record (`fp`, `name`, `plat`).
@MainActor
final class HostDiscovery: ObservableObject {
    @Published private(set) var nearby: [NearbyHost] = []

    private var browser: NWBrowser?
    private var endpoints: [String: NWEndpoint] = [:]
    private var resolving: Set<String> = []

    func start() {
        guard browser == nil else { return }
        let b = NWBrowser(for: .bonjourWithTXTRecord(type: "_tether._tcp", domain: nil), using: .tcp)
        b.browseResultsChangedHandler = { [weak self] results, _ in
            Task { @MainActor in self?.update(results) }
        }
        b.start(queue: .main)
        browser = b
    }

    func stop() {
        browser?.cancel()
        browser = nil
        nearby = []
        endpoints = [:]
        resolving = []
    }

    /// Looks for the computer with this fingerprint and returns its `ip:port`.
    func find(fingerprint: String, timeout: TimeInterval) async -> String? {
        let wasRunning = browser != nil
        start()
        defer { if !wasRunning { stop() } }
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let address = nearby.first(where: { $0.fingerprint == fingerprint })?.address {
                return address
            }
            try? await Task.sleep(nanoseconds: 250_000_000)
        }
        return nil
    }

    private func update(_ results: Set<NWBrowser.Result>) {
        var list: [NearbyHost] = []
        var found: [String: NWEndpoint] = [:]
        for result in results {
            guard case let .bonjour(txt) = result.metadata, let fp = txt["fp"] else { continue }
            found[fp] = result.endpoint
            let known = nearby.first { $0.fingerprint == fp }
            list.append(NearbyHost(
                fingerprint: fp,
                name: txt["name"] ?? "Computer",
                platform: txt["plat"] ?? "",
                address: known?.address
            ))
        }
        nearby = list.sorted { $0.name < $1.name }
        endpoints = found
        for host in list where host.address == nil {
            Task { await resolve(host.fingerprint) }
        }
    }

    private func resolve(_ fingerprint: String) async {
        guard !resolving.contains(fingerprint), let endpoint = endpoints[fingerprint] else { return }
        resolving.insert(fingerprint)
        defer { resolving.remove(fingerprint) }
        guard let address = await HostDiscovery.address(of: endpoint),
              let i = nearby.firstIndex(where: { $0.fingerprint == fingerprint }) else { return }
        nearby[i].address = address
    }

    /// Opens a throwaway connection to learn which IPv4 address the name resolves to.
    private static func address(of endpoint: NWEndpoint) async -> String? {
        let params = NWParameters.tcp
        if let ip = params.defaultProtocolStack.internetProtocol as? NWProtocolIP.Options {
            ip.version = .v4
        }
        let connection = NWConnection(to: endpoint, using: params)
        return await withCheckedContinuation { continuation in
            let once = Once()
            let finish: @Sendable (String?) -> Void = { value in
                guard !once.done else { return }
                once.done = true
                connection.cancel()
                continuation.resume(returning: value)
            }
            connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    if case let .hostPort(host, port)? = connection.currentPath?.remoteEndpoint {
                        var text = "\(host)"
                        if let percent = text.firstIndex(of: "%") { text = String(text[..<percent]) }
                        finish("\(text):\(port.rawValue)")
                    } else {
                        finish(nil)
                    }
                case .failed, .cancelled:
                    finish(nil)
                default:
                    break
                }
            }
            connection.start(queue: .main)
            DispatchQueue.main.asyncAfter(deadline: .now() + 4) { finish(nil) }
        }
    }
}
