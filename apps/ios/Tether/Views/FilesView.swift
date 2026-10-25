import SwiftUI
import UniformTypeIdentifiers

/// Browse the shared folder on the computer, download from it and upload into it.
struct FilesView: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.dismiss) private var dismiss

    @State private var path = ""
    @State private var entries: [RemoteFile] = []
    @State private var message: String?
    @State private var isError = false
    @State private var progress: TransferProgress?
    @State private var working = false
    @State private var importing = false
    @State private var received: URL?

    var body: some View {
        NavigationStack {
            List {
                if !path.isEmpty {
                    Button {
                        path = path.split(separator: "/").dropLast().joined(separator: "/")
                        Task { await load() }
                    } label: {
                        Label("Up one folder", systemImage: "arrow.up.left")
                    }
                    .disabled(working)
                }
                ForEach(entries, id: \.name) { entry in
                    if entry.dir {
                        Button {
                            path = join(path, entry.name)
                            Task { await load() }
                        } label: {
                            Label(entry.name, systemImage: "folder")
                        }
                        .disabled(working)
                    } else {
                        Button {
                            Task { await download(entry) }
                        } label: {
                            HStack {
                                Label(entry.name, systemImage: "doc")
                                Spacer()
                                Text(ByteCountFormatter.string(fromByteCount: Int64(entry.size), countStyle: .file))
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .disabled(working)
                    }
                }

                if let progress, working {
                    VStack(alignment: .leading, spacing: 6) {
                        ProgressView(value: Double(progress.done), total: Double(max(progress.total, 1)))
                        HStack {
                            Text("\(progress.upload ? "Sending" : "Receiving") \(progress.name)")
                                .font(.footnote)
                                .lineLimit(1)
                            Spacer()
                            Button("Cancel") { model.cancelTransfer() }
                                .font(.footnote)
                        }
                    }
                }
                if let received {
                    ShareLink(item: received) {
                        Label("Save or share \(received.lastPathComponent)", systemImage: "square.and.arrow.up")
                    }
                }
                if let message {
                    Text(message)
                        .font(.footnote)
                        .foregroundStyle(isError ? Color.red : Color.secondary)
                }
            }
            .navigationTitle(path.isEmpty ? "Files" : (path.split(separator: "/").last.map(String.init) ?? "Files"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        importing = true
                    } label: {
                        Label("Upload", systemImage: "square.and.arrow.up.on.square")
                    }
                    .disabled(working)
                }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
                if case let .success(url) = result {
                    Task { await upload(url) }
                }
            }
            .task { await load() }
        }
    }

    private func join(_ dir: String, _ name: String) -> String {
        dir.isEmpty ? name : "\(dir)/\(name)"
    }

    private func load() async {
        do {
            entries = try await model.listFiles(path)
            message = entries.isEmpty ? "This folder is empty." : nil
            isError = false
        } catch {
            entries = []
            message = AppModel.readable(error)
            isError = true
        }
    }

    /// Runs a transfer while showing how far it has got.
    private func transfer(_ run: () async throws -> String?) async {
        working = true
        received = nil
        message = nil
        let watcher = Task { @MainActor in
            while !Task.isCancelled {
                progress = model.transferProgress()
                try? await Task.sleep(nanoseconds: 250_000_000)
            }
        }
        defer {
            watcher.cancel()
            working = false
            progress = nil
        }
        do {
            if let done = try await run() {
                message = done
                isError = false
            }
        } catch {
            message = AppModel.readable(error)
            isError = true
        }
    }

    private func download(_ entry: RemoteFile) async {
        let remote = join(path, entry.name)
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let dest = folder.appendingPathComponent(entry.name)
        await transfer {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            _ = try await model.downloadFile(remote, to: dest)
            received = dest
            Haptics.success()
            return "Received \(entry.name)."
        }
    }

    private func upload(_ url: URL) async {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let remote = join(path, url.lastPathComponent)
        await transfer {
            let saved = try await model.uploadFile(url, as: remote)
            Haptics.success()
            await load()
            return "Sent. Saved there as \(saved)."
        }
    }
}
