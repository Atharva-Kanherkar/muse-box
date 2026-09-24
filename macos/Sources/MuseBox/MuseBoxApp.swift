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
                .defaultAppStorage(AppModel.store)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)
        .defaultSize(width: 1180, height: 760)
        .commands { PlaybackCommands(model: model) }

        MenuBarExtra {
            MenuBarPlayer()
                .environmentObject(model)
                .defaultAppStorage(AppModel.store)
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
            Toggle("See-Through Window", isOn: $model.glassWindow)
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
            let capturing = CommandLine.arguments.contains("--capture")
            // For screenshots, start just as a lyric lands.
            demo.start(model, fromMs: capturing ? 12_300 : 0)
            self.demo = demo
            if capturing { Capture.run() }
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

/// `--demo --capture`, for `scripts/screenshots.sh`. Liquid Glass is composited
/// live, so it can only be captured on screen: each surface floats on top for
/// a moment, announces its window number on stdout for `screencapture -l`, and
/// steps back. Then the app quits.
@MainActor
enum Capture {
    static func run() {
        // Launched from the terminal you are typing in, macOS lets this through;
        // under Stage Manager, an inactive app's window is only a thumbnail.
        NSApp.activate()
        after(7) {
            guard let player = NSApp.windows.first(where: { $0.title == "muse-box" && $0.frame.width > 400 }) else {
                return NSApp.terminate(nil)
            }
            player.level = .floating
            player.orderFrontRegardless()
            after(1.5) {
                announce("window", player)
                after(1.5) {
                    player.level = .normal
                    openMenuBarPanel()
                    after(1.5) {
                        if let panel = NSApp.windows.first(where: { $0 !== player && $0.isVisible && (280...400).contains($0.frame.width) }) {
                            announce("panel", panel)
                        }
                        after(1.5) { NSApp.terminate(nil) }
                    }
                }
            }
        }
    }

    private static func announce(_ kind: String, _ window: NSWindow) {
        FileHandle.standardOutput.write(Data("\(kind) \(window.windowNumber)\n".utf8))
    }

    private static func after(_ seconds: Double, _ work: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { MainActor.assumeIsolated(work) }
    }

    /// SwiftUI's MenuBarExtra is an NSStatusItem underneath: click it.
    private static func openMenuBarPanel() {
        func button(in view: NSView?) -> NSStatusBarButton? {
            guard let view else { return nil }
            if let button = view as? NSStatusBarButton { return button }
            return view.subviews.lazy.compactMap { button(in: $0) }.first
        }
        NSApp.windows.lazy.compactMap { button(in: $0.contentView) }.first?.performClick(nil)
    }
}
