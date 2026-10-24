import SwiftUI

@main
struct TetherApp: App {
    @StateObject private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(model)
                .tint(Theme.accent)
        }
        .onChange(of: scenePhase) { _, phase in
            // iOS suspends network connections in the background, so end the session
            // cleanly instead of coming back to a dead one.
            if phase == .background {
                model.disconnect()
            }
        }
    }
}
