import SwiftUI

struct ContentView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Group {
            if model.phase == .connected {
                RemoteView()
            } else {
                HostsView()
            }
        }
        .animation(.default, value: model.phase)
    }
}
