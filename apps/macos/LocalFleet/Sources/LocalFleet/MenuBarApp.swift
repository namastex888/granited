import AppKit
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        // No Dock icon even when run as a bare binary (the .app also sets LSUIElement).
        // Preferences switches to .regular while its window is open.
        NSApp.setActivationPolicy(.accessory)
        // The notch monitor follows its saved switch (on by default); the menu item stays the fallback.
        NotchController.shared.setEnabled(NotchController.shared.isEnabled)
        if LaunchOptions.current.openPreferences {
            DispatchQueue.main.async { PreferencesController.shared.show(pane: LaunchOptions.current.pane) }
        }
    }
}

struct LocalFleetApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var state = AppModel.shared

    var body: some Scene {
        MenuBarExtra {
            MenuContent(state: state)
        } label: {
            Image(nsImage: StatusIcon.image(for: state.glance))
                .accessibilityLabel((["LocalFleet: \(state.statusLine)"] + state.attentionReasons).joined(separator: "; "))
        }
        .menuBarExtraStyle(.menu)
        .commands {
            // While Settings is open the app is a regular app with a menu bar; give it the
            // standard LocalFleet > Settings… (⌘,) item every Mac app has.
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") { PreferencesController.shared.show() }
                    .keyboardShortcut(",")
            }
        }
    }
}

/// The single AppState shared by the menu and the Preferences window.
@MainActor
enum AppModel {
    static let shared = AppState()
}

/// The LocalFleet mark as a template image (Resources/MenuBarMark.png + @2x, copied into the .app by
/// build-app.sh), so the item follows the menu bar's light/dark appearance. A bare binary without the
/// bundle falls back to the `cpu` SF Symbol. Stopped: the mark dimmed. Idle: the mark. Serving, loading
/// and attention add a small cut-out badge (dot / arrow / exclamation mark) in the lower right corner.
enum StatusIcon {
    static let size = NSSize(width: 20, height: 16)

    static func image(for glance: AppState.Glance) -> NSImage {
        let badgeName: String? = switch glance {
        case .serving: "circle.fill"
        case .loading: "arrow.down.circle.fill"
        case .attention: "exclamationmark.triangle.fill"
        default: nil
        }
        let alpha: CGFloat = glance == .stopped ? 0.35 : 1
        let base = Bundle.main.image(forResource: "MenuBarMark") ?? symbol("cpu", points: 14, weight: .regular)
        let badge = badgeName.flatMap { symbol($0, points: $0 == "circle.fill" ? 6 : 8, weight: .bold) }

        let image = NSImage(size: size, flipped: false) { rect in
            guard let base else { return false }
            let bs = base.size
            let baseRect = NSRect(x: (rect.width - bs.width) / 2 - (badge == nil ? 0 : 1.5),
                                  y: (rect.height - bs.height) / 2, width: bs.width, height: bs.height)
            base.draw(in: baseRect, from: .zero, operation: .sourceOver, fraction: alpha)
            if let badge {
                let s = badge.size
                let r = NSRect(x: rect.maxX - s.width, y: rect.minY, width: s.width, height: s.height)
                // Clear a ring around the badge so it reads against the base glyph.
                NSGraphicsContext.current?.compositingOperation = .destinationOut
                NSBezierPath(ovalIn: r.insetBy(dx: -1.5, dy: -1.5)).fill()
                NSGraphicsContext.current?.compositingOperation = .sourceOver
                badge.draw(in: r, from: .zero, operation: .sourceOver, fraction: 1)
            }
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "LocalFleet"
        return image
    }

    private static func symbol(_ name: String, points: CGFloat, weight: NSFont.Weight) -> NSImage? {
        NSImage(systemSymbolName: name, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: points, weight: weight))
    }
}

/// The app icon (Resources/LocalFleet.icns via CFBundleIconFile); the `cpu` settings glyph when run
/// as a bare binary.
struct BrandIcon: View {
    let size: CGFloat

    var body: some View {
        if let icon = Bundle.main.image(forResource: "LocalFleet") {
            Image(nsImage: icon).resizable().interpolation(.high).frame(width: size, height: size)
        } else {
            SettingsIcon(symbol: "cpu", tint: .accentColor, size: size)
        }
    }
}

/// Native `.menu` content: disabled Text items for state, Buttons for actions.
struct MenuContent: View {
    @ObservedObject var state: AppState

    var body: some View {
        Text(state.statusLine)
        if case .unreachable(let why) = state.engine { Text(why) }
        ForEach(state.attentionReasons, id: \.self) { reason in
            Label(reason, systemImage: "exclamationmark.triangle")
        }
        if let note = state.configNote { Text(note) }

        if !state.hosts.isEmpty {
            Divider()
            ForEach(state.hosts) { h in
                Label(hostLine(h), systemImage: (h.reachable ?? false) ? "checkmark.circle" : "xmark.circle")
            }
        }

        Divider()
        Button("Start Engine") { state.service(.start) }
            .disabled(state.busy || state.engine.isRunning)
        Button("Stop Engine") { state.service(.stop) }
            .disabled(state.busy || !state.engine.isRunning)
        Button("Restart Engine") { state.service(.restart) }
            .disabled(state.busy)
        if let last = state.lastAction {
            Text(last)
        }

        Divider()
        Toggle("Show Notch Monitor", isOn: Binding(get: { NotchController.shared.isEnabled },
                                                    set: { NotchController.shared.setEnabled($0) }))
        Button("Settings…") { PreferencesController.shared.show() }
            .keyboardShortcut(",")
        Divider()
        Button("Quit LocalFleet") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }

    private func hostLine(_ h: HostStatus) -> String {
        let local = h.round_trip_ms == nil && (h.reachable ?? false)
        let name = local ? "\(h.host) (this Mac)" : h.host
        guard h.reachable ?? false else { return "\(name) — unreachable" }
        if let rtt = h.round_trip_ms { return "\(name) — reachable, \(Format.ms(rtt))" }
        return "\(name) — reachable"
    }
}
