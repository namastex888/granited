import AppKit
import Foundation

/// Headless modes. Each prints a compact summary (never the admin key, never the API key) and
/// exits 0 on success.
///
///   --self-test [--url URL] [--keys PATH] [--config PATH]
///       GET /healthz, /admin/models, /internal/hosts, /internal/hardware, /internal/metrics and
///       /v1/models with the app's client; prints the launchctl and granitectl commands the GUI
///       would run, without running them.
///   --dry-run-node-add DEST [--name N] [--jump J]    print the `granitectl node add` argv
///   --dry-run-node-remove NAME | --dry-run-uninstall print those argv
///   --write-keep-warm MODEL on|off --profile PATH   edit deployments[].keep_warm in PATH
///   --write-ram-reserve GIB --profile PATH          edit ram_reserve_bytes in PATH
///   --write-run-at-load on|off --plist PATH         edit RunAtLoad in a LaunchAgent plist (no launchctl)
///   --scan                                          run `granitectl scan --json` and summarize the plan
///   --exercise-load MODEL --url URL                 POST load, wait, POST unload, wait
enum SelfTest {
    static let modes = ["--self-test", "--dry-run-node-add", "--dry-run-node-remove", "--dry-run-uninstall",
                        "--write-keep-warm", "--write-ram-reserve", "--write-run-at-load", "--scan", "--exercise-load",
                        "--render-icons", "--service"]

    struct Options {
        var url: String?
        var keys: String?
        var config: String?
        var profile: String?
        var plist: String?
        var name: String?
        var jump: String?
        var values: [String: [String]] = [:]
    }

    static func parse(_ args: [String]) -> Options {
        var o = Options()
        var i = 0
        func next(_ n: Int = 1) -> [String] {
            let end = min(args.count, i + 1 + n)
            return i + 1 < end ? Array(args[(i + 1)..<end]) : []
        }
        while i < args.count {
            let a = args[i]
            switch a {
            case "--url": o.url = next().first; i += 1
            case "--keys": o.keys = next().first; i += 1
            case "--config": o.config = next().first; i += 1
            case "--profile": o.profile = next().first; i += 1
            case "--plist": o.plist = next().first; i += 1
            case "--name": o.name = next().first; i += 1
            case "--jump": o.jump = next().first; i += 1
            case "--dry-run-node-add", "--dry-run-node-remove", "--write-ram-reserve", "--exercise-load", "--render-icons",
                 "--service":
                o.values[a] = next(); i += 1
            case "--write-keep-warm", "--write-run-at-load":
                let n = a == "--write-keep-warm" ? 2 : 1
                o.values[a] = next(n); i += n
            case "--dry-run-uninstall", "--self-test", "--scan":
                o.values[a] = []
            default: break
            }
            i += 1
        }
        return o
    }

    @MainActor
    static func run(_ args: [String]) async -> Int32 {
        let opts = parse(args)
        let loaded = AppConfig.load(path: opts.config)
        var config = loaded.config
        if let url = opts.url { config.url = trimSlash(url) }
        if let keys = opts.keys { config.keys = expandTilde(keys) }
        if let profile = opts.profile { config.profile = expandTilde(profile) }

        if let v = opts.values["--dry-run-node-add"] {
            guard let dest = v.first else { return usage("--dry-run-node-add DEST [--name N] [--jump J]") }
            return await dryRun(config) { try $0.nodeAdd(destination: dest, name: opts.name, jump: opts.jump) }
        }
        if let v = opts.values["--dry-run-node-remove"] {
            guard let name = v.first else { return usage("--dry-run-node-remove NAME") }
            return await dryRun(config) { try $0.nodeRemove(name: name) }
        }
        if opts.values["--dry-run-uninstall"] != nil {
            return await dryRun(config) { $0.uninstall() }
        }
        if let v = opts.values["--write-keep-warm"] {
            guard v.count == 2, let on = onOff(v[1]), let path = opts.profile.map(expandTilde) else {
                return usage("--write-keep-warm MODEL on|off --profile PATH")
            }
            return edit(path, "deployments[model=\(v[0])].keep_warm = \(on)") { try $0.setKeepWarm(model: v[0], on) }
        }
        if let v = opts.values["--write-ram-reserve"] {
            guard let gib = v.first.flatMap(Int64.init), gib >= 0, let path = opts.profile.map(expandTilde) else {
                return usage("--write-ram-reserve GIB --profile PATH")
            }
            return edit(path, "ram_reserve_bytes = \(gib * 1_073_741_824)") { try $0.setRamReserve(bytes: gib * 1_073_741_824) }
        }
        if let v = opts.values["--write-run-at-load"] {
            guard let on = v.first.flatMap(onOff), let plist = opts.plist.map(expandTilde) else {
                return usage("--write-run-at-load on|off --plist PATH")
            }
            let lc = Launchctl(label: "-", plistPath: plist, dryRun: true)
            do {
                try lc.writeRunAtLoad(on)
                print("RunAtLoad = \(lc.runAtLoad().map(String.init) ?? "?") in \(plist) (launchctl not run)")
                return 0
            } catch {
                print("FAIL: \(error)"); return 1
            }
        }
        if let v = opts.values["--service"] {
            guard let action = v.first.flatMap(Launchctl.Action.init(rawValue:)) else {
                return usage("--service start|stop|restart [--config PATH]")
            }
            guard config.label != "ai.granited.router" || config.dryRun else {
                print("FAIL: refusing to drive ai.granited.router headless; use the menu"); return 1
            }
            let outcome = await Launchctl(label: config.label, plistPath: config.plist, dryRun: config.dryRun).run(action)
            print("\(action.rawValue) \(config.label): status \(outcome.status) \(outcome.output)")
            return outcome.status == 0 ? 0 : 1
        }
        if opts.values["--scan"] != nil { return await scan(config) }
        if let v = opts.values["--render-icons"] {
            guard let dir = v.first.map(expandTilde) else { return usage("--render-icons DIR") }
            return renderIcons(dir)
        }
        if let v = opts.values["--exercise-load"] {
            guard let model = v.first, opts.url != nil else { return usage("--exercise-load MODEL --url URL") }
            return await exerciseLoad(config, model: model)
        }
        return await selfTest(config, loaded: loaded)
    }

    private static func usage(_ s: String) -> Int32 { print("usage: GraniteBar \(s)"); return 2 }

    private static func onOff(_ s: String) -> Bool? {
        switch s.lowercased() { case "on", "true", "1": true; case "off", "false", "0": false; default: nil }
    }

    // MARK: Modes

    @MainActor
    private static func dryRun(_ config: AppConfig, _ build: (Granitectl) throws -> CommandSpec) async -> Int32 {
        do {
            let spec = try build(try Granitectl(config: config))
            // Same path the GUI sheet uses, with dry run forced on.
            let cmd = StreamingCommand(title: "dry run", spec: spec, dryRun: true)
            cmd.start()
            print(cmd.log, terminator: "")
            return cmd.exitStatus == 0 ? 0 : 1
        } catch {
            print("FAIL: \(error)"); return 1
        }
    }

    private static func edit(_ path: String, _ what: String, _ change: (inout ProfileEditor) throws -> ProfileEditor.Outcome) -> Int32 {
        do {
            let outcome = try ProfileEditor.edit(path, change)
            print("\(what): \(outcome == .changed ? "written" : "unchanged") (\(path))")
            return 0
        } catch {
            print("FAIL: \(error)"); return 1
        }
    }

    @MainActor
    private static func scan(_ config: AppConfig) async -> Int32 {
        do {
            let tool = try Granitectl(config: config)
            print("$ \(tool.scan().commandLine)")
            let r = await ProcessCapture.run(tool.scan())
            guard r.status == 0, let data = r.stdout.data(using: .utf8) else {
                print("FAIL: exit \(r.status) \(r.stderr.suffix(300))"); return 1
            }
            let result = try JSONDecoder().decode(ScanResult.self, from: data)
            let p = result.plan
            print("host     : \(result.inventory?.hostname ?? "?") (\(p.host ?? "?"), \(p.arch ?? "?"))")
            print("llama    : " + (p.llama.map { "\($0.device ?? "?") / \($0.pool ?? "?")" } ?? "none"))
            print("languages: \((p.languages ?? []).joined(separator: ", "))")
            print("native   : \((p.native ?? []).joined(separator: ", "))")
            print("skipped  : \((p.skipped ?? []).isEmpty ? "none" : "")")
            for s in p.skipped ?? [] { print("  - \(s)") }
            for n in p.notes ?? [] { print("note     : \(n)") }
            return 0
        } catch {
            print("FAIL: \(error)"); return 1
        }
    }

    /// Writes the menu-bar icon for every glance state, as black-on-light and white-on-dark PNGs
    /// at 2x (template images take the menu bar's color at runtime).
    private static func renderIcons(_ dir: String) -> Int32 {
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let states: [(String, AppState.Glance)] = [("stopped", .stopped), ("idle", .idle), ("serving", .serving),
                                                   ("loading", .loading), ("attention", .attention)]
        for (name, glance) in states {
            let icon = StatusIcon.image(for: glance)
            for (look, fg, bg) in [("light", NSColor.black, NSColor(white: 0.93, alpha: 1)), ("dark", NSColor.white, NSColor(white: 0.16, alpha: 1))] {
                let scale: CGFloat = 4
                let w = Int(StatusIcon.size.width * scale), h = Int(StatusIcon.size.height * scale)
                guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: w, pixelsHigh: h, bitsPerSample: 8,
                                                 samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                                 colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return 1 }
                rep.size = StatusIcon.size
                NSGraphicsContext.saveGraphicsState()
                NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
                let rect = NSRect(origin: .zero, size: StatusIcon.size)
                // Tint the template the way the menu bar does: draw it, then fill through its alpha.
                let tinted = NSImage(size: StatusIcon.size, flipped: false) { r in
                    icon.draw(in: r)
                    fg.set()
                    r.fill(using: .sourceAtop)
                    return true
                }
                bg.setFill()
                rect.fill()
                tinted.draw(in: rect)
                NSGraphicsContext.restoreGraphicsState()
                let path = "\(dir)/icon-\(name)-\(look).png"
                guard let png = rep.representation(using: .png, properties: [:]),
                      (try? png.write(to: URL(fileURLWithPath: path))) != nil else { print("FAIL: \(path)"); return 1 }
                print(path)
            }
        }
        return 0
    }

    private static func client(_ config: AppConfig) -> (GraniteClient, String) {
        switch EngineKeys.load(from: config.keys) {
        case .success(let k):
            let note = "admin key \(k.admin == nil ? "missing" : "loaded"), API key \(k.api == nil ? "missing" : "loaded") from \(config.keys ?? "?")"
                + (k.permissionWarning.map { " [warning: \($0)]" } ?? "")
            return (GraniteClient(baseURL: config.url, adminKey: k.admin, apiKey: k.api), note)
        case .failure(let e):
            return (GraniteClient(baseURL: config.url, adminKey: nil), "keys not loaded (\(e))")
        }
    }

    private static func exerciseLoad(_ config: AppConfig, model: String) async -> Int32 {
        // The live router on the default port has real consumers: refuse to POST to it.
        if let u = URL(string: config.url), [8310].contains(u.port ?? 80) {
            print("refusing: --exercise-load does not POST to port 8310 (the live router)"); return 2
        }
        let (client, note) = client(config)
        print("url      : \(config.url)")
        print("keys     : \(note)")
        for action in [GraniteClient.ModelAction.load, .unload] {
            do {
                let accepted = try await client.modelAction(action, model: model)
                print("\(action.rawValue.padding(toLength: 9, withPad: " ", startingAt: 0)): POST accepted, operation \(accepted.id) status \(accepted.status ?? "?")")
                let done = try await client.waitForOperation(accepted.id)
                print("           GET /admin/operations/\(done.id) → \(done.summary)")
                let workers = try await client.workers()
                if let w = workers.first(where: { $0.model == model }) {
                    print("           worker state \(w.state ?? "?"), load_ms \(w.load_ms.map { String(format: "%.0f", $0) } ?? "-"), pid \(w.pid.map(String.init) ?? "-")")
                }
                if done.status != "succeeded" { return 1 }
            } catch {
                print("\(action.rawValue): FAIL (\(error))"); return 1
            }
        }
        return 0
    }

    @MainActor
    private static func selfTest(_ config: AppConfig, loaded: AppConfig.LoadResult) async -> Int32 {
        var ok = true
        print("GraniteBar self-test")
        switch loaded {
        case .loaded(let c): print("config   : \(c.source ?? "?")")
        case .missing(_, let p): print("config   : defaults (no file at \(p))")
        case .invalid(_, let p, let r): print("config   : defaults (\(p): \(r))")
        }
        print("url      : \(config.url)")
        let (client, note) = client(config)
        print("keys     : \(note)")

        do { print(String(format: "health   : ok (%.0f ms)", try await client.health())) } catch {
            ok = false; print("health   : FAIL (\(error))")
        }

        do {
            let r = try await client.adminModels()
            let workers = r.workers ?? []
            print("models   : \(r.models?.count ?? 0) in registry, \(workers.count) local worker(s), \(r.peers?.count ?? 0) peer(s)")
            for w in workers {
                var parts = [w.state ?? "unknown", w.device ?? "-", w.backend ?? "-",
                             "\(w.active_requests ?? 0)/\(w.max_concurrency ?? 0) requests"]
                if let f = r.models?.first(where: { $0.id == w.model })?.family { parts.insert(f, at: 0) }
                if let h = w.hold_reason { parts.append("held: \(h)") }
                if let e = w.last_error { parts.append("last_error \(e.summary)") }
                print("  - \(w.model): " + parts.joined(separator: ", "))
            }
        } catch {
            ok = false; print("models   : FAIL (\(error))")
        }

        do {
            let hosts = try await client.hosts()
            print("hosts    : \(hosts.count)")
            for h in hosts {
                let reach = (h.reachable ?? false) ? "reachable" : "unreachable"
                let rtt = h.round_trip_ms.map { Format.ms($0) } ?? (h.reachable == true ? "local" : (h.status.map { "HTTP \($0)" } ?? "-"))
                print("  - \(h.host): \(reach), \(rtt), \(h.modelCount.map { plural($0, "model") } ?? "? models")")
            }
        } catch {
            ok = false; print("hosts    : FAIL (\(error))")
        }

        do {
            let hw = try await client.hardware()
            let used = (hw.ram?.total_bytes ?? 0) - (hw.ram?.available_bytes ?? 0)
            print("hardware : \(hw.cpu_brand ?? "?"), \(hw.logical_cpus ?? 0) logical CPUs, memory \(Format.bytes(used)) of \(Format.bytes(hw.ram?.total_bytes)) used, swap \(Format.bytes(hw.ram?.swap_used_bytes)) of \(Format.bytes(hw.ram?.swap_total_bytes))")
            print("           stall \(hw.memory_stall_percent.map { String(format: "%.0f%%", $0) } ?? "-"), load \(hw.load_average.map { String(format: "%.2f %.2f %.2f", $0.one ?? 0, $0.five ?? 0, $0.fifteen ?? 0) } ?? "-"), reserve \(Format.bytes(hw.admission?.ram_reserve_bytes))")
            for a in hw.accelerators ?? [] {
                print("  - accelerator \(a.name ?? "?") (\(a.vendor ?? "?"), \(a.memory_kind ?? "?") memory\(a.cores.map { ", \($0) cores" } ?? ""))")
            }
        } catch {
            ok = false; print("hardware : FAIL (\(error))")
        }

        do {
            let m = try await client.metrics()
            print("metrics  : uptime \(Format.duration(m.uptime_seconds)), \(m.operations?.count ?? 0) operation(s) recorded")
        } catch {
            ok = false; print("metrics  : FAIL (\(error))")
        }

        do {
            let models = try await client.publicModels()
            let servable = models.filter(\.servable)
            let chat = servable.first(where: \.canChat)?.id
            print("v1/models: \(models.count) listed, \(servable.count) servable here (\(servable.map(\.id).joined(separator: ", ")))")
            print("snippet  : model \(chat ?? "granite-4.2-8b")\(chat == nil ? " (fallback: no servable chat model)" : "")")
        } catch {
            ok = false; print("v1/models: FAIL (\(error))")
        }

        if let path = config.profile {
            switch EngineProfile.read(path) {
            case .success(let p):
                print("profile  : \(path): listen \(p.listen ?? "-"), ram_reserve_bytes \(p.ram_reserve_bytes.map(String.init) ?? "-"), \(p.deployments?.count ?? 0) deployment(s), \(p.peers?.count ?? 0) peer(s)")
                for d in p.deployments ?? [] { print("  - \(d.model): keep_warm \(d.keep_warm ?? false)\(d.hold_reason.map { ", hold \($0)" } ?? "")") }
            case .failure(let e): print("profile  : \(e)")
            }
        } else {
            print("profile  : not configured")
        }
        print("logs     : \(config.logs ?? "not configured")")
        let lc = Launchctl(label: config.label, plistPath: config.plist, dryRun: true)
        print("login    : RunAtLoad \(lc.runAtLoad().map(String.init) ?? "unknown (no plist at \(config.plist))")")
        print("launchctl plan (not executed):")
        for (action, cmd) in lc.plan { print("  \(action.rawValue): \(cmd)") }
        if let g = try? Granitectl(config: config) {
            print("granitectl plan (not executed):")
            print("  scan     : \(g.scan().commandLine)")
            if let add = try? g.nodeAdd(destination: "user@host", name: nil, jump: nil) { print("  node add : \(add.commandLine)") }
            print("  uninstall: \(g.uninstall().commandLine)")
        } else {
            print("granitectl: not configured")
        }
        print("result   : \(ok ? "PASS" : "FAIL")")
        return ok ? 0 : 1
    }
}
