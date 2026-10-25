import Foundation

/// Contents of `~/Library/Application Support/granited/app.json` (written by `granitectl install`).
/// Every field is optional on disk; missing fields fall back to defaults or disable the feature
/// that needs them.
struct AppConfig: Sendable, Equatable {
    static let defaultURL = "http://127.0.0.1:8310"
    static let defaultLabel = "ai.granited.router"

    var url: String
    var keys: String?
    var label: String
    var plist: String
    var logs: String?
    /// The granited checkout (`granitectl` runs with this as its working directory).
    var root: String?
    /// The engine's JSON profile (`deployments[]`, `ram_reserve_bytes`, `listen`, `peers[]`).
    var profile: String?
    /// Interpreter used to run `granitectl`.
    var python: String?
    /// Path to `scripts/granitectl.py`.
    var granitectl: String?
    /// Where model weights live, when known.
    var models: String?
    /// When true, every process-spawning action (launchctl, granitectl, LaunchAgent edits)
    /// prints its argv instead of running it.
    var dryRun: Bool

    /// Where the config was read from, or nil when defaults are in use.
    var source: String?

    static var defaultPath: String {
        let base = FileManager.default.homeDirectoryForCurrentUser.path
        return base + "/Library/Application Support/granited/app.json"
    }

    static var defaults: AppConfig {
        AppConfig(
            url: defaultURL, keys: nil, label: defaultLabel,
            plist: expandTilde("~/Library/LaunchAgents/\(defaultLabel).plist"),
            logs: nil, root: nil, profile: nil, python: nil, granitectl: nil, models: nil,
            dryRun: false, source: nil)
    }

    private struct OnDisk: Decodable {
        var url: String?
        var keys: String?
        var label: String?
        var plist: String?
        var logs: String?
        var root: String?
        var profile: String?
        var python: String?
        var granitectl: String?
        var models: String?
        var dry_run: Bool?
    }

    enum LoadResult: Sendable {
        case loaded(AppConfig)
        case missing(AppConfig, path: String)
        case invalid(AppConfig, path: String, reason: String)

        var config: AppConfig {
            switch self {
            case .loaded(let c), .missing(let c, _), .invalid(let c, _, _): return c
            }
        }

        func with(_ config: AppConfig) -> LoadResult {
            switch self {
            case .loaded: return .loaded(config)
            case .missing(_, let p): return .missing(config, path: p)
            case .invalid(_, let p, let r): return .invalid(config, path: p, reason: r)
            }
        }
    }

    /// Resolution order: explicit path > `--config` > GRANITE_BAR_CONFIG > default path.
    static func load(path explicit: String? = nil) -> LoadResult {
        let path = expandTilde(
            explicit ?? LaunchOptions.current.configPath
                ?? ProcessInfo.processInfo.environment["GRANITE_BAR_CONFIG"] ?? defaultPath)
        var result: LoadResult
        if let data = FileManager.default.contents(atPath: path) {
            do {
                let disk = try JSONDecoder().decode(OnDisk.self, from: data)
                let label = nonEmpty(disk.label) ?? defaultLabel
                let root = nonEmpty(disk.root).map(expandTilde)
                var config = AppConfig(
                    url: trimSlash(nonEmpty(disk.url) ?? defaultURL),
                    keys: nonEmpty(disk.keys).map(expandTilde),
                    label: label,
                    plist: expandTilde(nonEmpty(disk.plist) ?? "~/Library/LaunchAgents/\(label).plist"),
                    logs: nonEmpty(disk.logs).map(expandTilde),
                    root: root,
                    profile: nonEmpty(disk.profile).map(expandTilde),
                    python: nonEmpty(disk.python).map(expandTilde),
                    granitectl: nonEmpty(disk.granitectl).map(expandTilde),
                    models: nonEmpty(disk.models).map(expandTilde),
                    dryRun: disk.dry_run ?? false,
                    source: path)
                // A checkout implies its standard layout when the explicit fields are absent.
                if let root {
                    let fm = FileManager.default
                    if config.granitectl == nil, fm.fileExists(atPath: root + "/scripts/granitectl.py") {
                        config.granitectl = root + "/scripts/granitectl.py"
                    }
                    if config.models == nil, fm.fileExists(atPath: root + "/models") {
                        config.models = root + "/models"
                    }
                }
                result = .loaded(config)
            } catch {
                result = .invalid(defaults, path: path, reason: "not valid JSON for app.json")
            }
        } else {
            result = .missing(defaults, path: path)
        }
        if LaunchOptions.current.dryRun {
            var c = result.config
            c.dryRun = true
            result = result.with(c)
        }
        return result
    }

    /// `<url>/v1`, the OpenAI-compatible base URL consumers use.
    var openAIBaseURL: String { trimSlash(url) + "/v1" }
}

/// Command-line options that also apply to the GUI. Parsed once in main.swift before anything
/// else runs and never mutated afterwards.
struct LaunchOptions: Sendable {
    var configPath: String?
    var openPreferences = false
    var pane: String?
    var dryRun = false

    nonisolated(unsafe) static var current = LaunchOptions()

    static func parse(_ args: [String]) -> LaunchOptions {
        var o = LaunchOptions()
        var i = 0
        while i < args.count {
            let next = i + 1 < args.count ? args[i + 1] : nil
            switch args[i] {
            case "--config": o.configPath = next; i += 1
            case "--open-preferences": o.openPreferences = true
            case "--pane": o.pane = next; o.openPreferences = true; i += 1
            case "--dry-run": o.dryRun = true
            default: break
            }
            i += 1
        }
        return o
    }
}

enum KeyError: Error, CustomStringConvertible {
    case noKeysPath
    case unreadable(String)
    case missingFields(String)

    var description: String {
        switch self {
        case .noKeysPath: return "no keys file configured"
        case .unreadable(let p): return "cannot read keys file \(p)"
        case .missingFields(let p): return "neither GRANITE_ADMIN_KEY nor GRANITE_API_KEY in \(p)"
        }
    }
}

/// The two bearer keys from the router's JSON secrets file. Values are never logged; error
/// messages carry only paths and field names.
struct EngineKeys: Sendable {
    /// `GRANITE_ADMIN_KEY`: authorizes /admin and /internal. Never displayed.
    let admin: String?
    /// `GRANITE_API_KEY`: what consumers send to /v1. Displayed only after Reveal.
    let api: String?
    /// Non-nil when the keys file is readable by group/other (expected 0600).
    let permissionWarning: String?

    static func load(from path: String?) -> Result<EngineKeys, KeyError> {
        guard let path, !path.isEmpty else { return .failure(.noKeysPath) }
        guard let data = FileManager.default.contents(atPath: path),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return .failure(.unreadable(path)) }
        let admin = (object["GRANITE_ADMIN_KEY"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        let api = (object["GRANITE_API_KEY"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        guard admin != nil || api != nil else { return .failure(.missingFields(path)) }
        var warning: String?
        if let attrs = try? FileManager.default.attributesOfItem(atPath: path),
           let mode = attrs[.posixPermissions] as? NSNumber,
           mode.intValue & 0o077 != 0 {
            warning = String(format: "keys file mode is %o; expected 600", mode.intValue)
        }
        return .success(EngineKeys(admin: admin, api: api, permissionWarning: warning))
    }
}

func expandTilde(_ path: String) -> String {
    (path as NSString).expandingTildeInPath
}

private func nonEmpty(_ s: String?) -> String? {
    guard let s, !s.trimmingCharacters(in: .whitespaces).isEmpty else { return nil }
    return s
}

func trimSlash(_ s: String) -> String {
    var s = s
    while s.hasSuffix("/") { s.removeLast() }
    return s
}
