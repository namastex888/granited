import AppKit
import SwiftUI

enum Pane: String, CaseIterable, Identifiable, Hashable {
    case general, models, devices, nodes, connect, advanced

    var id: String { rawValue }

    var title: String {
        switch self {
        case .general: "General"
        case .models: "Models"
        case .devices: "Devices"
        case .nodes: "Nodes"
        case .connect: "Connect"
        case .advanced: "Advanced"
        }
    }

    var symbol: String {
        switch self {
        case .general: "gearshape.fill"
        case .models: "square.stack.3d.up.fill"
        case .devices: "memorychip.fill"
        case .nodes: "network"
        case .connect: "link"
        case .advanced: "wrench.and.screwdriver.fill"
        }
    }

    var tint: Color {
        switch self {
        case .general: .gray
        case .models: .blue
        case .devices: .green
        case .nodes: .indigo
        case .connect: .orange
        case .advanced: Color(nsColor: .darkGray)
        }
    }
}

@MainActor
final class PreferencesNavigation: ObservableObject {
    @Published var selection: Pane? = .general
}

/// Owns the Preferences window. An LSUIElement app has no Dock icon or menu bar of its own, so
/// while the window is open the app switches to the regular activation policy (the window can
/// come to the front and appear in ⌘-Tab) and goes back to accessory when it closes.
@MainActor
final class PreferencesController: NSObject, NSWindowDelegate {
    static let shared = PreferencesController()

    private var window: NSWindow?
    private let navigation = PreferencesNavigation()

    func show(pane: String? = nil) {
        if let pane, let p = Pane(rawValue: pane.lowercased()) { navigation.selection = p }
        let window = self.window ?? makeWindow()
        self.window = window
        AppModel.shared.preferencesVisible = true
        NSApp.setActivationPolicy(.regular)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        if LaunchOptions.current.openPreferences {
            // For scripted checks: the CGWindowID that `screencapture -l` takes.
            print("preferences window \(window.windowNumber) pane \(navigation.selection?.rawValue ?? "-")")
            fflush(stdout)
        }
    }

    private func makeWindow() -> NSWindow {
        let root = PreferencesView(state: AppModel.shared, navigation: navigation)
        let hosting = NSHostingController(rootView: root)
        hosting.sceneBridgingOptions = [.toolbars, .title]
        let window = NSWindow(contentViewController: hosting)
        window.styleMask = [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView]
        window.toolbarStyle = .unified
        window.titlebarSeparatorStyle = .automatic
        window.isReleasedWhenClosed = false
        window.setContentSize(NSSize(width: 760, height: 580))
        window.contentMinSize = NSSize(width: 640, height: 420)
        window.setFrameAutosaveName("GraniteBarPreferences")
        window.center()
        window.delegate = self
        return window
    }

    func windowWillClose(_ notification: Notification) {
        AppModel.shared.preferencesVisible = false
        NSApp.setActivationPolicy(.accessory)
    }
}

struct PreferencesView: View {
    @ObservedObject var state: AppState
    @ObservedObject var navigation: PreferencesNavigation

    var body: some View {
        NavigationSplitView {
            List(selection: $navigation.selection) {
                EngineSidebarCard(state: state)
                    .tag(Pane.general)
                    .listRowSeparator(.hidden)
                Section {
                    ForEach(Pane.allCases) { pane in
                        Label {
                            Text(pane.title)
                        } icon: {
                            SettingsIcon(symbol: pane.symbol, tint: pane.tint)
                        }
                        .tag(pane)
                    }
                }
            }
            .listStyle(.sidebar)
            .navigationSplitViewColumnWidth(min: 200, ideal: 215, max: 260)
        } detail: {
            let pane = navigation.selection ?? .general
            Group {
                switch pane {
                case .general: GeneralPane(state: state)
                case .models: ModelsPane(state: state)
                case .devices: DevicesPane(state: state)
                case .nodes: NodesPane(state: state)
                case .connect: ConnectPane(state: state)
                case .advanced: AdvancedPane(state: state)
                }
            }
            .formStyle(.grouped)
            .navigationTitle(pane.title)
        }
        .frame(minWidth: 640, minHeight: 420)
    }
}

/// The System Settings sidebar glyph: a white SF Symbol on a colored rounded square.
struct SettingsIcon: View {
    let symbol: String
    let tint: Color
    var size: CGFloat = 20

    var body: some View {
        RoundedRectangle(cornerRadius: size * 0.27, style: .continuous)
            .fill(tint.gradient)
            .frame(width: size, height: size)
            .overlay {
                Image(systemName: symbol)
                    .font(.system(size: size * 0.55, weight: .semibold))
                    .foregroundStyle(.white)
            }
    }
}

/// The account-style card at the top of the sidebar: the engine and its state.
struct EngineSidebarCard: View {
    @ObservedObject var state: AppState

    var body: some View {
        HStack(spacing: 10) {
            SettingsIcon(symbol: "cpu", tint: .accentColor, size: 34)
            VStack(alignment: .leading, spacing: 1) {
                Text("granited").font(.headline)
                Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .padding(.vertical, 6)
    }

    private var subtitle: String {
        switch state.engine {
        case .running: return state.hostID.map { "Running on \($0)" } ?? "Running"
        case .stopped: return "Stopped"
        case .unreachable: return "Not reachable"
        case .unknown: return "Checking…"
        }
    }
}

/// A colored dot plus text, as System Settings shows connection state.
struct StatusText: View {
    let text: String
    let color: Color

    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(color).frame(width: 8, height: 8)
            Text(text)
        }
    }
}

extension AppState.Engine {
    var label: String {
        switch self {
        case .unknown: "Checking…"
        case .running: "Running"
        case .stopped: "Stopped"
        case .unreachable: "Not reachable"
        }
    }

    var color: Color {
        switch self {
        case .running: .green
        case .stopped: .secondary
        case .unreachable: .red
        case .unknown: .gray
        }
    }
}

/// The "Restart the engine to apply" row shown after a profile edit.
struct RestartBanner: View {
    @ObservedObject var state: AppState

    var body: some View {
        if state.restartRequired {
            Section {
                HStack {
                    Image(systemName: "arrow.clockwise.circle.fill").foregroundStyle(.orange).imageScale(.large)
                    VStack(alignment: .leading) {
                        Text("Restart the engine to apply")
                        Text("The profile changed; the running engine still uses the previous settings.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Restart") { state.service(.restart) }.disabled(state.busy)
                }
            }
        }
    }
}

/// The result of the last action, as a section footer line.
struct LastActionFooter: View {
    @ObservedObject var state: AppState

    var body: some View {
        if let last = state.lastAction {
            Text(last).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
        }
    }
}

/// Monospaced text that scrolls to its end as it grows.
struct LogView: View {
    let text: String

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                Text(text.isEmpty ? " " : text)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                Color.clear.frame(height: 1).id("end")
            }
            .background(Color(nsColor: .textBackgroundColor))
            .clipShape(RoundedRectangle(cornerRadius: 6))
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(.separator))
            .onChange(of: text) { proxy.scrollTo("end", anchor: .bottom) }
        }
    }
}

/// A sheet that runs one StreamingCommand and shows its output live.
struct CommandSheet: View {
    @ObservedObject var command: StreamingCommand
    var onClose: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(command.title).font(.headline)
            if command.dryRun {
                Label("Dry run: the command is printed, not executed.", systemImage: "info.circle")
                    .font(.caption).foregroundStyle(.secondary)
            }
            LogView(text: command.log).frame(minHeight: 220)
            HStack {
                if command.running { ProgressView().controlSize(.small); Text("Running…").foregroundStyle(.secondary) }
                Spacer()
                if command.running {
                    Button("Cancel", role: .cancel) { command.cancel() }
                } else {
                    Button("Done") { onClose() }.keyboardShortcut(.defaultAction)
                }
            }
        }
        .padding(20)
        .frame(width: 560)
        .onAppear { command.start() }
    }
}
