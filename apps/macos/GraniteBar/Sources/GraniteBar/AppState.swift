import AppKit
import Foundation

/// Everything the menu and Preferences show. One cancellable Task checks /healthz every 5 s.
/// The heavier endpoints (/admin/models, /internal/hosts, /internal/hardware, /internal/metrics,
/// /v1/models) refresh every 30 s, or every 5 s while Preferences is visible. All network I/O is
/// async URLSession with short timeouts and never blocks the main thread.
@MainActor
final class AppState: ObservableObject {
    enum Engine: Equatable {
        case unknown
        case running(ms: Double)
        case stopped
        case unreachable(String)

        var isRunning: Bool { if case .running = self { return true }; return false }
    }

    /// What the menu-bar symbol shows.
    enum Glance: Equatable { case stopped, idle, serving, loading, attention }

    @Published private(set) var config: AppConfig = .defaults
    @Published private(set) var configNote: String?
    @Published private(set) var engine: Engine = .unknown
    @Published private(set) var registry: [RegistryModel] = []
    @Published private(set) var workers: [Worker] = []
    @Published private(set) var peers: [Peer] = []
    @Published private(set) var modelsError: String?
    @Published private(set) var hosts: [HostStatus] = []
    @Published private(set) var hostsError: String?
    @Published private(set) var hardware: Hardware?
    @Published private(set) var hardwareError: String?
    @Published private(set) var uptime: Double?
    @Published private(set) var publicModels: [PublicModel] = []
    @Published private(set) var publicModelsError: String?
    @Published private(set) var profile: EngineProfile?
    @Published private(set) var profileError: String?
    @Published private(set) var startAtLogin: Bool?
    @Published private(set) var scan: ScanResult?
    @Published private(set) var scanError: String?
    @Published private(set) var scanning = false
    @Published private(set) var lastAction: String?
    @Published private(set) var busy = false
    /// Set after a profile edit; cleared by a successful restart.
    @Published private(set) var restartRequired = false
    /// The latest load/unload operation per model.
    @Published private(set) var operations: [String: Operation] = [:]
    @Published private(set) var lastDetails: Date?

    /// True while the Preferences window is on screen: details refresh every 5 s.
    var preferencesVisible = false {
        didSet { if preferencesVisible && !oldValue { Task { await refresh() } } }
    }

    static let healthInterval: Duration = .seconds(5)
    static let detailsInterval: Duration = .seconds(30)
    static let preferencesInterval: Duration = .seconds(5)

    private var pollTask: Task<Void, Never>?
    private var keys: EngineKeys?
    private var lastDetailsAt: ContinuousClock.Instant?
    private var detailsInFlight = false

    init() {
        reloadConfig()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.tick()
                try? await Task.sleep(for: AppState.healthInterval)
            }
        }
    }

    deinit { pollTask?.cancel() }

    var client: GraniteClient { GraniteClient(baseURL: config.url, adminKey: keys?.admin, apiKey: keys?.api) }
    var launchctl: Launchctl { Launchctl(label: config.label, plistPath: config.plist, dryRun: config.dryRun) }
    var hasAdminKey: Bool { keys?.admin != nil }
    var hasAPIKey: Bool { keys?.api != nil }

    /// The consumer API key, read on demand for Reveal / Copy only.
    func apiKey() -> String? { keys?.api }

    // MARK: Derived state

    var glance: Glance {
        guard engine.isRunning else { return .stopped }
        if needsAttention { return .attention }
        if workers.contains(where: { $0.state == "loading" })
            || operations.values.contains(where: { $0.isRunning && $0.action == "load" }) { return .loading }
        if activeRequests > 0 { return .serving }
        return .idle
    }

    var needsAttention: Bool { !attentionReasons.isEmpty }

    var attentionReasons: [String] {
        guard engine.isRunning else { return [] }
        var r: [String] = []
        if let stall = hardware?.memory_stall_percent, stall > 5 { r.append(String(format: "Memory stall %.0f%%", stall)) }
        for h in hosts where h.reachable == false { r.append("\(h.host) unreachable") }
        for w in workers where w.state == "failed" { r.append("\(w.model) failed") }
        return r
    }

    var readyCount: Int { workers.filter { $0.state == "ready" }.count }
    var activeRequests: Int { workers.reduce(0) { $0 + ($1.active_requests ?? 0) } }

    /// "Running · 2 models ready · 1 request".
    var statusLine: String {
        switch engine {
        case .unknown: return "Checking…"
        case .stopped: return "Stopped"
        case .unreachable: return "Not reachable"
        case .running:
            var parts = ["Running", plural(readyCount, "model") + " ready"]
            let loading = workers.filter { $0.state == "loading" }.count
            if loading > 0 { parts.append("\(loading) loading") }
            parts.append(plural(activeRequests, "request"))
            return parts.joined(separator: " · ")
        }
    }

    func family(of model: String) -> String? { registry.first(where: { $0.id == model })?.family }

    /// The model the Connect snippets use: first servable chat model, else granite-4.2-8b.
    var snippetModel: String {
        publicModels.first(where: { $0.servable && $0.canChat })?.id ?? "granite-4.2-8b"
    }

    var hostID: String? { hardware?.host_id ?? profile?.host_id ?? hosts.first?.host }

    // MARK: Refresh

    /// Re-reads app.json, the keys file, the profile and the LaunchAgent each cycle so an
    /// installer can drop them in without restarting the app.
    func reloadConfig() {
        let result = AppConfig.load()
        config = result.config
        switch result {
        case .loaded: configNote = nil
        case .missing(_, let path): configNote = "Not configured: create \(path)"
        case .invalid(_, let path, let reason): configNote = "Config ignored (\(reason)): \(path)"
        }
        switch EngineKeys.load(from: config.keys) {
        case .success(let k):
            keys = k
            if let w = k.permissionWarning { configNote = (configNote.map { $0 + "; " } ?? "") + w }
        case .failure:
            keys = nil
        }
        if let path = config.profile {
            switch EngineProfile.read(path) {
            case .success(let p): profile = p; profileError = nil
            case .failure(let e): profile = nil; profileError = e.description
            }
        } else {
            profile = nil
            profileError = "No profile configured (set `profile` in app.json)"
        }
        startAtLogin = launchctl.runAtLoad()
    }

    private func tick() async {
        reloadConfig()
        await refreshHealth()
        let interval = preferencesVisible ? Self.preferencesInterval : Self.detailsInterval
        if isDue(interval) { await refreshDetails() }
    }

    /// Everything, now (menu "Refresh", after actions, when Preferences opens).
    func refresh() async {
        reloadConfig()
        await refreshHealth()
        await refreshDetails()
    }

    private func isDue(_ interval: Duration) -> Bool {
        guard let last = lastDetailsAt else { return true }
        return ContinuousClock.now - last >= interval - .milliseconds(250)
    }

    private func refreshHealth() async {
        do { engine = .running(ms: try await client.health()) } catch let e as ClientError {
            engine = e.isConnectionRefused ? .stopped : .unreachable(e.description)
        } catch {
            engine = .unreachable(String(describing: error))
        }
    }

    private func refreshDetails() async {
        guard !detailsInFlight else { return }
        detailsInFlight = true
        defer { detailsInFlight = false }
        lastDetailsAt = .now
        guard engine.isRunning else {
            workers = []; hosts = []; hardware = nil; uptime = nil; publicModels = []
            let why = "engine not running"
            modelsError = why; hostsError = why; hardwareError = why; publicModelsError = why
            return
        }
        let client = self.client
        async let m = Result { try await client.adminModels() }
        async let hs = Result { try await client.hosts() }
        async let hw = Result { try await client.hardware() }
        async let mt = Result { try await client.metrics() }
        async let pm = Result { try await client.publicModels() }
        let (mr, hsr, hwr, mtr, pmr) = await (m, hs, hw, mt, pm)

        switch mr {
        case .success(let r):
            registry = r.models ?? []; workers = r.workers ?? []; peers = r.peers ?? []; modelsError = nil
        case .failure(let e): workers = []; modelsError = String(describing: e)
        }
        switch hsr {
        case .success(let list): hosts = list; hostsError = nil
        case .failure(let e): hosts = []; hostsError = String(describing: e)
        }
        switch hwr {
        case .success(let h): hardware = h; hardwareError = nil
        case .failure(let e): hardware = nil; hardwareError = String(describing: e)
        }
        if case .success(let metrics) = mtr { uptime = metrics.uptime_seconds } else { uptime = nil }
        switch pmr {
        case .success(let list): publicModels = list; publicModelsError = nil
        case .failure(let e): publicModels = []; publicModelsError = String(describing: e)
        }
        lastDetails = Date()
    }

    // MARK: Engine (launchctl)

    func service(_ action: Launchctl.Action) {
        let lc = launchctl
        let cmd = lc.commandLine(lc.arguments(for: action))
        if action != .start {
            guard confirm("\(action.rawValue.capitalized) the engine?",
                          detail: "Runs: \(cmd)\nConnected consumers will be interrupted.",
                          button: action == .stop ? "Stop Engine" : "Restart Engine") else { return }
        }
        busy = true
        Task {
            let outcome = await lc.run(action)
            if outcome.dryRun {
                lastAction = outcome.output
            } else if outcome.status == 0 {
                lastAction = "\(action.rawValue.capitalized): ok"
                if action != .stop { restartRequired = false }
            } else {
                lastAction = "\(action.rawValue.capitalized): exit \(outcome.status)"
                    + (outcome.output.isEmpty ? "" : " — \(outcome.output.prefix(160))")
            }
            busy = false
            await refresh()
        }
    }

    func setStartAtLogin(_ on: Bool) {
        let lc = launchctl
        let cmd = lc.commandLine(on ? lc.arguments(for: .start) : lc.arguments(for: .stop))
        let detail = on
            ? "Sets RunAtLoad in \(config.plist) and registers the agent:\n\(cmd)"
            : "Sets RunAtLoad to false in \(config.plist) and unregisters the agent:\n\(cmd)\nThis stops a running engine and interrupts connected consumers."
        guard confirm(on ? "Start the engine at login?" : "Stop starting the engine at login?",
                      detail: detail, button: on ? "Turn On" : "Turn Off", destructive: !on) else {
            objectWillChange.send()
            return
        }
        busy = true
        Task {
            let outcome = await lc.setStartAtLogin(on)
            lastAction = outcome.dryRun ? outcome.output
                : (outcome.status == 0 ? "Start at login \(on ? "on" : "off")" : "Start at login: exit \(outcome.status) \(outcome.output.prefix(160))")
            busy = false
            await refresh()
        }
    }

    // MARK: Models

    func modelAction(_ action: GraniteClient.ModelAction, model: String) {
        if action == .unload {
            guard confirm("Unload \(model)?",
                          detail: "Sends POST /admin/models/\(model)/unload to \(config.url). Requests in flight finish first; new ones load it again on demand.",
                          button: "Unload") else { return }
        }
        let client = self.client
        Task {
            do {
                let op = try await client.modelAction(action, model: model)
                operations[model] = op
                await refresh()
                let done = try await client.waitForOperation(op.id)
                operations[model] = done
                lastAction = done.summary
            } catch {
                operations[model] = Operation(id: "-", model: model, action: action.rawValue, status: "failed",
                                              error: ServerError(code: nil, message: String(describing: error)))
                lastAction = "\(action.rawValue) \(model): \(error)"
            }
            await refresh()
        }
    }

    func setKeepWarm(_ model: String, _ on: Bool) {
        guard let path = config.profile else { return }
        do {
            let outcome = try ProfileEditor.edit(path) { try $0.setKeepWarm(model: model, on) }
            if outcome == .changed { restartRequired = true }
            lastAction = "Keep warm \(on ? "on" : "off") for \(model)" + (outcome == .changed ? " — restart to apply" : " (unchanged)")
        } catch {
            lastAction = "Keep warm: \(error)"
        }
        reloadConfig()
    }

    func setRamReserve(gib: Int) {
        guard let path = config.profile else { return }
        let bytes = Int64(gib) * 1_073_741_824
        do {
            let outcome = try ProfileEditor.edit(path) { try $0.setRamReserve(bytes: bytes) }
            if outcome == .changed { restartRequired = true }
            lastAction = "Memory reserve \(gib) GiB" + (outcome == .changed ? " — restart to apply" : "")
        } catch {
            lastAction = "Memory reserve: \(error)"
        }
        reloadConfig()
    }

    // MARK: Hardware plan

    func runScan() {
        guard !scanning else { return }
        let tool: Granitectl
        do { tool = try Granitectl(config: config) } catch {
            scanError = String(describing: error); return
        }
        scanning = true
        Task {
            // `scan` is read-only (scripts/probe.py starts and installs nothing), so it runs even
            // in dry-run mode.
            let r = await ProcessCapture.run(tool.scan())
            if r.status == 0, let data = r.stdout.data(using: .utf8),
               let result = try? JSONDecoder().decode(ScanResult.self, from: data) {
                scan = result; scanError = nil
            } else {
                scanError = r.status == 0 ? "scan printed unexpected output"
                    : "scan exited \(r.status): " + String(r.stderr.split(separator: "\n").last ?? "")
            }
            scanning = false
        }
    }

    // MARK: Commands that change the installation

    func granitectl() throws -> Granitectl { try Granitectl(config: config) }

    func makeCommand(title: String, spec: CommandSpec) -> StreamingCommand {
        StreamingCommand(title: title, spec: spec, dryRun: config.dryRun)
    }

    // MARK: Finder

    func showLogs() {
        guard let logs = config.logs else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: logs, isDirectory: true)])
    }

    func reveal(_ path: String?) {
        guard let path else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    func open(_ path: String?) {
        guard let path else { return }
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    func copy(_ s: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
    }

    // MARK: Confirmation

    func confirm(_ title: String, detail: String, button: String = "Continue", destructive: Bool = false) -> Bool {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = detail + (config.dryRun ? "\n\nDry run: the command is shown, not executed." : "")
        alert.alertStyle = destructive ? .critical : .warning
        let ok = alert.addButton(withTitle: button)
        ok.hasDestructiveAction = destructive
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }
}

func plural(_ n: Int, _ word: String) -> String { "\(n) \(word)\(n == 1 ? "" : "s")" }

enum Format {
    static func bytes(_ b: Int64?) -> String {
        guard let b else { return "—" }
        return ByteCountFormatter.string(fromByteCount: b, countStyle: .memory)
    }

    static func duration(_ seconds: Double?) -> String {
        guard let seconds else { return "—" }
        let f = DateComponentsFormatter()
        f.allowedUnits = seconds >= 86_400 ? [.day, .hour, .minute] : seconds >= 3600 ? [.hour, .minute] : [.minute, .second]
        f.unitsStyle = .abbreviated
        return f.string(from: seconds) ?? "\(Int(seconds)) s"
    }

    static func ms(_ ms: Double?) -> String {
        guard let ms else { return "—" }
        return ms >= 1000 ? String(format: "%.1f s", ms / 1000) : String(format: "%.0f ms", ms)
    }
}
