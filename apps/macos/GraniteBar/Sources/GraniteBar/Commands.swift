import Foundation

/// A process to run: argv plus working directory. Never goes through a shell.
struct CommandSpec: Sendable, Equatable {
    var executable: String
    var arguments: [String]
    var cwd: String?

    var argv: [String] { [executable] + arguments }
    var commandLine: String { argv.map(shellQuote).joined(separator: " ") }

    /// Finder-launched apps get a minimal PATH; granitectl needs ssh, uv, git and Homebrew tools.
    static var environment: [String: String] {
        var env = ProcessInfo.processInfo.environment
        let extra = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        let path = (env["PATH"] ?? "").split(separator: ":").map(String.init)
        env["PATH"] = (path + extra.filter { !path.contains($0) }).joined(separator: ":")
        env["PYTHONUNBUFFERED"] = "1"
        return env
    }
}

/// Runs a process to completion and captures its output (used for launchctl and `scan --json`).
enum ProcessCapture {
    struct Result: Sendable {
        var status: Int32
        var stdout: String
        var stderr: String
    }

    static func run(executable: String, arguments: [String], cwd: String? = nil) async -> Result {
        await withCheckedContinuation { cont in
            DispatchQueue.global(qos: .userInitiated).async {
                let p = Process()
                p.executableURL = URL(fileURLWithPath: executable)
                p.arguments = arguments
                if let cwd { p.currentDirectoryURL = URL(fileURLWithPath: cwd) }
                p.environment = CommandSpec.environment
                let out = Pipe(), err = Pipe()
                p.standardOutput = out
                p.standardError = err
                p.standardInput = FileHandle.nullDevice
                do {
                    try p.run()
                } catch {
                    cont.resume(returning: Result(status: -1, stdout: "", stderr: "could not run \(executable)"))
                    return
                }
                // Drain stderr concurrently so a chatty process cannot fill the pipe and stall.
                let errData = LockedData()
                let group = DispatchGroup()
                group.enter()
                DispatchQueue.global().async {
                    errData.set(err.fileHandleForReading.readDataToEndOfFile())
                    group.leave()
                }
                let outData = out.fileHandleForReading.readDataToEndOfFile()
                group.wait()
                p.waitUntilExit()
                cont.resume(returning: Result(status: p.terminationStatus,
                                              stdout: String(decoding: outData, as: UTF8.self),
                                              stderr: String(decoding: errData.get(), as: UTF8.self)))
            }
        }
    }

    static func run(_ spec: CommandSpec) async -> Result {
        await run(executable: spec.executable, arguments: spec.arguments, cwd: spec.cwd)
    }
}

private final class LockedData: @unchecked Sendable {
    private var data = Data()
    private let lock = NSLock()
    func set(_ d: Data) { lock.lock(); data = d; lock.unlock() }
    func get() -> Data { lock.lock(); defer { lock.unlock() }; return data }
}

/// Runs a long command (node add, node remove, uninstall) and streams stdout and stderr into
/// `log` as they arrive. `cancel()` sends SIGTERM, then SIGKILL after 3 s.
@MainActor
final class StreamingCommand: ObservableObject, Identifiable {
    let id = UUID()
    let title: String
    let spec: CommandSpec
    let dryRun: Bool

    @Published private(set) var log = ""
    @Published private(set) var running = false
    @Published private(set) var exitStatus: Int32?
    @Published private(set) var cancelled = false

    private var process: Process?

    init(title: String, spec: CommandSpec, dryRun: Bool) {
        self.title = title
        self.spec = spec
        self.dryRun = dryRun
    }

    var finished: Bool { exitStatus != nil }

    func start() {
        guard !running, exitStatus == nil else { return }
        append("$ \(spec.commandLine)\n")
        if dryRun {
            append("dry run: not executed. argv:\n")
            for (i, a) in spec.argv.enumerated() { append("  [\(i)] \(a)\n") }
            if let cwd = spec.cwd { append("  cwd: \(cwd)\n") }
            exitStatus = 0
            return
        }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: spec.executable)
        p.arguments = spec.arguments
        if let cwd = spec.cwd { p.currentDirectoryURL = URL(fileURLWithPath: cwd) }
        p.environment = CommandSpec.environment
        p.standardInput = FileHandle.nullDevice
        let out = Pipe(), err = Pipe()
        p.standardOutput = out
        p.standardError = err
        for pipe in [out, err] {
            pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
                let data = handle.availableData
                guard !data.isEmpty else { handle.readabilityHandler = nil; return }
                let text = String(decoding: data, as: UTF8.self)
                Task { @MainActor in self?.append(text) }
            }
        }
        p.terminationHandler = { [weak self] proc in
            let status = proc.terminationStatus
            Task { @MainActor in
                // Let the readability handlers flush their last chunk first.
                try? await Task.sleep(for: .milliseconds(150))
                self?.finish(status)
            }
        }
        do {
            try p.run()
            process = p
            running = true
        } catch {
            append("could not start \(spec.executable)\n")
            exitStatus = -1
        }
    }

    func cancel() {
        guard running, let p = process else { return }
        cancelled = true
        append("\nCancelling…\n")
        p.terminate()
        let pid = p.processIdentifier
        Task {
            try? await Task.sleep(for: .seconds(3))
            if self.running { kill(pid, SIGKILL) }
        }
    }

    private func append(_ s: String) { log += s }

    private func finish(_ status: Int32) {
        running = false
        exitStatus = status
        process = nil
        append(cancelled ? "\nCancelled (exit \(status)).\n" : "\nExited with status \(status).\n")
    }
}

/// argv builders for `scripts/granitectl.py`. Arguments that reach argparse or ssh are checked so
/// a value can never be parsed as an option.
struct Granitectl: Sendable {
    let python: String
    let script: String
    let root: String?

    enum BuildError: Error, CustomStringConvertible {
        case notConfigured
        case invalid(String)
        var description: String {
            switch self {
            case .notConfigured: return "granitectl is not configured (set `granitectl` or `root` in app.json)"
            case .invalid(let why): return why
            }
        }
    }

    init(config: AppConfig) throws {
        guard let script = config.granitectl else { throw BuildError.notConfigured }
        self.python = config.python ?? "/usr/bin/python3"
        self.script = script
        self.root = config.root ?? ((script as NSString).deletingLastPathComponent as NSString).deletingLastPathComponent
    }

    private func spec(_ args: [String]) -> CommandSpec {
        CommandSpec(executable: python, arguments: [script] + args, cwd: root)
    }

    private static func check(_ value: String, _ what: String) throws -> String {
        let v = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !v.isEmpty else { throw BuildError.invalid("\(what) is empty") }
        guard !v.hasPrefix("-") else { throw BuildError.invalid("\(what) must not start with '-'") }
        guard !v.contains(where: { $0.isWhitespace || $0.isNewline }) else { throw BuildError.invalid("\(what) must not contain spaces") }
        return v
    }

    func scan() -> CommandSpec { spec(["scan", "--json"]) }

    func nodeAdd(destination: String, name: String?, jump: String?) throws -> CommandSpec {
        var args = ["node", "add", try Self.check(destination, "SSH destination"), "--yes"]
        if let name, !name.trimmingCharacters(in: .whitespaces).isEmpty { args += ["--name", try Self.check(name, "Name")] }
        if let jump, !jump.trimmingCharacters(in: .whitespaces).isEmpty { args += ["--jump", try Self.check(jump, "Jump host")] }
        return spec(args)
    }

    func nodeRemove(name: String) throws -> CommandSpec {
        spec(["node", "remove", try Self.check(name, "Node name")])
    }

    func uninstall() -> CommandSpec { spec(["uninstall"]) }
}

/// `granitectl scan --json` → `{inventory, plan}`.
struct ScanResult: Decodable, Sendable, Equatable {
    struct Plan: Decodable, Sendable, Equatable {
        struct Llama: Decodable, Sendable, Equatable {
            var device: String?
            var pool: String?
            var asset: String?
            var pool_bytes: Int64?
        }
        var host: String?
        var arch: String?
        var reserve_gib: Double?
        var llama: Llama?
        var languages: [String]?
        var native: [String]?
        var native_lock: String?
        var notes: [String]?
        var skipped: [String]?
        var existing_granited: Bool?
    }
    struct Inventory: Decodable, Sendable, Equatable {
        var hostname: String?
        var distro: String?
        var cpu: String?
        var machine: String?
    }
    var inventory: Inventory?
    var plan: Plan
}
