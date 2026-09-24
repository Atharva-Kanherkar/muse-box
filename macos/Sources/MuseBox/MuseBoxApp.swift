import AppKit
import MuseBoxCore
import SwiftUI

@main
enum Launcher {
    static func main() {
        Typeface.register()
        if let index = CommandLine.arguments.firstIndex(of: "--snapshot") {
            let directory = CommandLine.arguments.dropFirst(index + 1).first ?? "."
            MainActor.assumeIsolated { Stills.render(to: URL(fileURLWithPath: directory)) }
            return
        }
        MuseBoxApp.main()
    }
}

struct MuseBoxApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @ObservedObject private var model = AppModel.shared

    var body: some Scene {
        Window("muse-box", id: "player") {
            PlayerView()
                .environmentObject(model)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)
        .defaultSize(width: 1180, height: 760)
        .commands { PlaybackCommands(model: model) }

        MenuBarExtra {
            MenuBarPlayer()
                .environmentObject(model)
        } label: {
            Image(nsImage: MenuBarIcon.image)
                .accessibilityLabel("muse-box")
        }
        .menuBarExtraStyle(.window)
    }
}

private struct PlaybackCommands: Commands {
    @ObservedObject var model: AppModel

    var body: some Commands {
        CommandMenu("Playback") {
            Button(model.isPlaying ? "Pause" : "Play") { model.playPause() }
                .keyboardShortcut(.space, modifiers: [])
            Button("Next Track") { model.next() }
                .keyboardShortcut(.rightArrow, modifiers: .command)
            Button("Previous Track") { model.previous() }
                .keyboardShortcut(.leftArrow, modifiers: .command)
        }
        CommandGroup(after: .toolbar) {
            Toggle("Lyrics", isOn: $model.showLyrics)
                .keyboardShortcut("l", modifiers: [])
            Toggle("1-bit Panel Face", isOn: $model.panelFace)
                .keyboardShortcut("b", modifiers: [])
            Picker("Dither", selection: $model.dither) {
                Text("Bayer").tag(DitherMode.bayer)
                Text("Atkinson").tag(DitherMode.atkinson)
            }
            Button("Room Light: \(model.roomLight.next.title)") { model.roomLight = model.roomLight.next }
                .keyboardShortcut("r", modifiers: [])
            Toggle("Glass Window", isOn: $model.glassWindow)
            Divider()
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var roomLight: RoomLightController?
    private var demo: Demo?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let model = AppModel.shared
        if CommandLine.arguments.contains("--demo") {
            let demo = Demo()
            demo.start(model)
            self.demo = demo
        } else {
            model.start()
        }
        let roomLight = RoomLightController(model: model)
        roomLight.start()
        self.roomLight = roomLight
    }

    /// Closing the window keeps the room lit; the menu bar still runs it.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
}
