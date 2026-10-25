import Foundation

/// Start / Stop / Restart the engine LaunchAgent in the user's GUI domain, and its RunAtLoad key.
/// Commands are argv arrays (no shell). With `dryRun` nothing is executed or written; the
/// outcome carries the command that would have run.
struct Launchctl: Sendable {
    let label: String
    let plistPath: String
    var dryRun = false
    var uid: uid_t = getuid()

    static let tool = "/bin/launchctl"

    enum Action: String, CaseIterable, Sendable { case start, stop, restart }

    var domain: String { "gui/\(uid)" }
    var target: String { "\(domain)/\(label)" }

    func arguments(for action: Action) -> [String] {
        switch action {
        case .start: return ["bootstrap", domain, plistPath]
        case .stop: return ["bootout", target]
        case .restart: return ["kickstart", "-k", target]
        }
    }

    func commandLine(_ args: [String]) -> String { ([Self.tool] + args).map(shellQuote).joined(separator: " ") }

    var plan: [(Action, String)] {
        Action.allCases.map { ($0, commandLine(arguments(for: $0))) }
    }

    struct Outcome: Sendable {
        var status: Int32
        var output: String
        var dryRun = false
    }

    func run(_ action: Action) async -> Outcome {
        let outcome = await exec(arguments(for: action))
        // bootstrap of a loaded agent fails with EIO (5): the engine is already running.
        if action == .start, outcome.status == 5 { return Outcome(status: 0, output: "already running") }
        // Stop boots the agent out, so kickstart then finds no service (113); load it instead.
        if action == .restart, outcome.status == 113 { return await exec(arguments(for: .start)) }
        return outcome
    }

    private func exec(_ args: [String]) async -> Outcome {
        if dryRun { return Outcome(status: 0, output: "dry run: " + commandLine(args), dryRun: true) }
        let r = await ProcessCapture.run(executable: Self.tool, arguments: args)
        return Outcome(status: r.status, output: (r.stdout + r.stderr).trimmingCharacters(in: .whitespacesAndNewlines))
    }

    // MARK: Start at login (RunAtLoad)

    /// The plist's RunAtLoad value, or nil when the plist is missing or unreadable.
    func runAtLoad() -> Bool? {
        guard let dict = readPlist()?.dict else { return nil }
        return dict["RunAtLoad"] as? Bool ?? false
    }

    private func readPlist() -> (dict: [String: Any], format: PropertyListSerialization.PropertyListFormat)? {
        guard let data = FileManager.default.contents(atPath: plistPath) else { return nil }
        var format = PropertyListSerialization.PropertyListFormat.xml
        guard let dict = try? PropertyListSerialization.propertyList(from: data, options: [], format: &format) as? [String: Any]
        else { return nil }
        return (dict, format)
    }

    /// Writes RunAtLoad into the plist (same format and keys; the XML is rewritten in canonical form).
    func writeRunAtLoad(_ on: Bool) throws {
        guard let (original, format) = readPlist() else { throw LaunchAgentError.unreadable(plistPath) }
        var dict = original
        dict["RunAtLoad"] = on
        let data = try PropertyListSerialization.data(fromPropertyList: dict, format: format, options: 0)
        try data.write(to: URL(fileURLWithPath: plistPath), options: .atomic)
    }

    /// Turning it on writes RunAtLoad=true and bootstraps the agent so launchd knows it now;
    /// turning it off writes RunAtLoad=false and boots it out (this stops a running engine).
    func setStartAtLogin(_ on: Bool) async -> Outcome {
        let args = on ? arguments(for: .start) : arguments(for: .stop)
        if dryRun {
            return Outcome(status: 0, output: "dry run: set RunAtLoad=\(on) in \(plistPath); then " + commandLine(args), dryRun: true)
        }
        do { try writeRunAtLoad(on) } catch {
            return Outcome(status: -1, output: String(describing: error))
        }
        var outcome = await exec(args)
        // bootstrap of an already-loaded agent fails with "service already loaded" (EIO = 5);
        // the key is written, which is what matters for the next login.
        if on, outcome.status == 5 { outcome = Outcome(status: 0, output: "already loaded; RunAtLoad written") }
        return outcome
    }
}

enum LaunchAgentError: Error, CustomStringConvertible {
    case unreadable(String)
    var description: String {
        switch self { case .unreadable(let p): return "cannot read LaunchAgent plist \(p)" }
    }
}

func shellQuote(_ s: String) -> String {
    let safe = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_./:=@,+%")
    if !s.isEmpty, s.unicodeScalars.allSatisfy({ safe.contains($0) }) { return s }
    return "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
}
