import Foundation

/// The parts of the engine profile (the JSON passed to `granited serve`) the app reads.
struct EngineProfile: Decodable, Sendable, Equatable {
    struct Deployment: Decodable, Sendable, Equatable {
        var model: String
        var keep_warm: Bool?
        var hold_reason: String?
        var device: String?
        var backend: String?
    }
    struct PeerEntry: Decodable, Sendable, Equatable {
        var id: String
        var url: String?
        var models: [String]?
    }
    var listen: String?
    var host_id: String?
    var state_dir: String?
    var ram_reserve_bytes: Int64?
    var deployments: [Deployment]?
    var peers: [PeerEntry]?

    static func read(_ path: String) -> Result<EngineProfile, ProfileError> {
        guard let data = FileManager.default.contents(atPath: path) else { return .failure(.unreadable(path)) }
        do { return .success(try JSONDecoder().decode(EngineProfile.self, from: data)) } catch {
            return .failure(.invalid("not a granited profile"))
        }
    }

    func keepWarm(_ model: String) -> Bool {
        deployments?.first(where: { $0.model == model })?.keep_warm ?? false
    }
}

enum ProfileError: Error, CustomStringConvertible, Equatable {
    case unreadable(String)
    case invalid(String)
    case notFound(String)
    case writeFailed(String)

    var description: String {
        switch self {
        case .unreadable(let p): return "cannot read profile \(p)"
        case .invalid(let why): return "profile is not valid JSON (\(why))"
        case .notFound(let what): return "\(what) not found in profile"
        case .writeFailed(let why): return "could not write profile (\(why))"
        }
    }
}

/// Edits a JSON document in place by byte range, so everything not edited (key order, indent,
/// escapes, number spelling, trailing newline) stays byte-for-byte identical.
struct ProfileEditor {
    private(set) var bytes: [UInt8]

    init(data: Data) { bytes = [UInt8](data) }

    enum Outcome: Equatable { case changed, unchanged }

    /// Sets `deployments[i].keep_warm` for the deployment whose `model` matches.
    mutating func setKeepWarm(model: String, _ on: Bool) throws -> Outcome {
        let root = try parse()
        guard case .object(let top, _, _) = root,
              let deployments = top.first(where: { $0.key == "deployments" }),
              case .array(let items, _, _) = deployments.value
        else { throw ProfileError.notFound("deployments[]") }
        for item in items {
            guard case .object(let members, _, _) = item,
                  let m = members.first(where: { $0.key == "model" }),
                  string(m.value) == model
            else { continue }
            return try set(member: "keep_warm", raw: on ? "true" : "false", in: item, absentMeans: "false")
        }
        throw ProfileError.notFound("deployment for \(model)")
    }

    /// Sets the top-level `ram_reserve_bytes`.
    mutating func setRamReserve(bytes value: Int64) throws -> Outcome {
        try set(member: "ram_reserve_bytes", raw: String(value), in: try parse(), absentMeans: nil)
    }

    /// Replaces an existing member's value, or appends the member after the object's last one
    /// using the same separator and indentation as that sibling.
    private mutating func set(member key: String, raw: String, in object: Node, absentMeans: String?) throws -> Outcome {
        guard case .object(let members, let open, let close) = object else { throw ProfileError.invalid("expected object") }
        if let existing = members.first(where: { $0.key == key }) {
            let old = Array(bytes[existing.value.start..<existing.value.end])
            if old == Array(raw.utf8) { return .unchanged }
            bytes.replaceSubrange(existing.value.start..<existing.value.end, with: Array(raw.utf8))
            return .changed
        }
        if let absentMeans, raw == absentMeans { return .unchanged }
        let keyText = Array("\"\(key)\"".utf8)
        let insertion: [UInt8]
        let at: Int
        if let last = members.last {
            // Whitespace before the last key (newline + indent) and the "key": separator.
            let previous = members.count > 1 ? members[members.count - 2].value.end : open + 1
            var wsStart = previous
            while wsStart < last.keyStart, bytes[wsStart] == UInt8(ascii: ",") || bytes[wsStart] == UInt8(ascii: "{") { wsStart += 1 }
            let ws = Array(bytes[wsStart..<last.keyStart])
            let sep = Array(bytes[last.keyEnd..<last.value.start])
            insertion = [UInt8(ascii: ",")] + ws + keyText + sep + Array(raw.utf8)
            at = last.value.end
        } else {
            insertion = keyText + Array(": ".utf8) + Array(raw.utf8)
            at = close
        }
        bytes.insert(contentsOf: insertion, at: at)
        return .changed
    }

    /// Validates the result and writes it atomically, keeping the file's permissions.
    func write(to path: String) throws {
        let data = Data(bytes)
        guard (try? JSONSerialization.jsonObject(with: data)) != nil else {
            throw ProfileError.invalid("edit produced invalid JSON; nothing written")
        }
        let fm = FileManager.default
        let perms = (try? fm.attributesOfItem(atPath: path))?[.posixPermissions]
        do {
            try data.write(to: URL(fileURLWithPath: path), options: .atomic)
            if let perms { try? fm.setAttributes([.posixPermissions: perms], ofItemAtPath: path) }
        } catch {
            throw ProfileError.writeFailed(error.localizedDescription)
        }
    }

    /// Reads, edits and writes a profile in one step.
    static func edit(_ path: String, _ change: (inout ProfileEditor) throws -> Outcome) throws -> Outcome {
        guard let data = FileManager.default.contents(atPath: path) else { throw ProfileError.unreadable(path) }
        var editor = ProfileEditor(data: data)
        let outcome = try change(&editor)
        if outcome == .changed { try editor.write(to: path) }
        return outcome
    }

    // MARK: Minimal JSON parser that records byte ranges

    struct Member {
        var key: String
        var keyStart: Int
        var keyEnd: Int
        var value: Node
    }

    indirect enum Node {
        case object([Member], open: Int, close: Int)
        case array([Node], open: Int, close: Int)
        case scalar(start: Int, end: Int)

        var start: Int {
            switch self {
            case .object(_, let o, _), .array(_, let o, _): return o
            case .scalar(let s, _): return s
            }
        }
        var end: Int {
            switch self {
            case .object(_, _, let c), .array(_, _, let c): return c + 1
            case .scalar(_, let e): return e
            }
        }
    }

    private func string(_ node: Node) -> String? {
        guard case .scalar(let s, let e) = node, bytes[s] == UInt8(ascii: "\"") else { return nil }
        return try? JSONDecoder().decode(String.self, from: Data(bytes[s..<e]))
    }

    func parse() throws -> Node {
        var i = 0
        let node = try value(&i)
        skip(&i)
        guard i == bytes.count else { throw ProfileError.invalid("trailing content") }
        return node
    }

    private func skip(_ i: inout Int) {
        while i < bytes.count, [0x20, 0x0A, 0x0D, 0x09].contains(bytes[i]) { i += 1 }
    }

    private func value(_ i: inout Int) throws -> Node {
        skip(&i)
        guard i < bytes.count else { throw ProfileError.invalid("unexpected end") }
        switch bytes[i] {
        case UInt8(ascii: "{"):
            let open = i
            i += 1
            var members: [Member] = []
            skip(&i)
            if i < bytes.count, bytes[i] == UInt8(ascii: "}") { return .object([], open: open, close: i).advance(&i) }
            while true {
                skip(&i)
                guard i < bytes.count, bytes[i] == UInt8(ascii: "\"") else { throw ProfileError.invalid("expected key") }
                let ks = i
                try stringEnd(&i)
                let ke = i
                let key = (try? JSONDecoder().decode(String.self, from: Data(bytes[ks..<ke]))) ?? ""
                skip(&i)
                guard i < bytes.count, bytes[i] == UInt8(ascii: ":") else { throw ProfileError.invalid("expected :") }
                i += 1
                let v = try value(&i)
                members.append(Member(key: key, keyStart: ks, keyEnd: ke, value: v))
                skip(&i)
                guard i < bytes.count else { throw ProfileError.invalid("unterminated object") }
                if bytes[i] == UInt8(ascii: ",") { i += 1; continue }
                guard bytes[i] == UInt8(ascii: "}") else { throw ProfileError.invalid("expected , or }") }
                return .object(members, open: open, close: i).advance(&i)
            }
        case UInt8(ascii: "["):
            let open = i
            i += 1
            var items: [Node] = []
            skip(&i)
            if i < bytes.count, bytes[i] == UInt8(ascii: "]") { return .array([], open: open, close: i).advance(&i) }
            while true {
                items.append(try value(&i))
                skip(&i)
                guard i < bytes.count else { throw ProfileError.invalid("unterminated array") }
                if bytes[i] == UInt8(ascii: ",") { i += 1; continue }
                guard bytes[i] == UInt8(ascii: "]") else { throw ProfileError.invalid("expected , or ]") }
                return .array(items, open: open, close: i).advance(&i)
            }
        case UInt8(ascii: "\""):
            let s = i
            try stringEnd(&i)
            return .scalar(start: s, end: i)
        default:
            let s = i
            while i < bytes.count, !([0x20, 0x0A, 0x0D, 0x09] + Array(",}]".utf8)).contains(bytes[i]) { i += 1 }
            guard i > s else { throw ProfileError.invalid("expected value") }
            return .scalar(start: s, end: i)
        }
    }

    /// Advances past a string literal starting at the opening quote.
    private func stringEnd(_ i: inout Int) throws {
        i += 1
        while i < bytes.count {
            switch bytes[i] {
            case UInt8(ascii: "\\"): i += 2
            case UInt8(ascii: "\""): i += 1; return
            default: i += 1
            }
        }
        throw ProfileError.invalid("unterminated string")
    }
}

private extension ProfileEditor.Node {
    /// Returns self after moving the cursor past the closing bracket.
    func advance(_ i: inout Int) -> Self { i += 1; return self }
}
