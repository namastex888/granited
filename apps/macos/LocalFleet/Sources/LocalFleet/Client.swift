import Foundation

// MARK: - Response types
// Every field is optional so new or missing server fields never break decoding. Shapes follow
// src/api.rs and src/lifecycle.rs (`worker_status`, `hardware`, `Operation`).

/// A JSON value that may arrive as a string or a number (e.g. `cores: "20"`).
struct FlexString: Decodable, Sendable, Hashable, CustomStringConvertible {
    var description: String
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if let s = try? c.decode(String.self) { description = s }
        else if let i = try? c.decode(Int64.self) { description = String(i) }
        else if let d = try? c.decode(Double.self) { description = String(d) }
        else if let b = try? c.decode(Bool.self) { description = String(b) }
        else { description = "" }
    }
}

/// One entry of `workers` from GET /admin/models and `managed_workers` in a hardware report.
struct Worker: Decodable, Sendable, Identifiable, Hashable {
    var model: String
    var state: String?
    var pid: Int?
    var backend: String?
    var device: String?
    var revision: String?
    var active_requests: Int?
    var max_concurrency: Int?
    var external: Bool?
    var load_ms: Double?
    var unload_ms: Double?
    var idle_seconds: Double?
    var reserved_peak_bytes: Int64?
    var hold_reason: String?
    var last_error: ServerError?

    var id: String { model }
    var isHeld: Bool { !(hold_reason ?? "").isEmpty }
}

struct ServerError: Decodable, Sendable, Hashable {
    var code: String?
    var message: String?

    private enum CodingKeys: String, CodingKey { case code, message, error }

    init(code: String?, message: String?) { self.code = code; self.message = message }

    init(from decoder: Decoder) throws {
        // May be an object ({code,message} or {error:{code,message}}) or a plain string.
        if let single = try? decoder.singleValueContainer(), let text = try? single.decode(String.self) {
            code = nil; message = text; return
        }
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let nested = try? c.decode(ServerError.self, forKey: .error) {
            code = nested.code; message = nested.message; return
        }
        code = try? c.decode(String.self, forKey: .code)
        message = try? c.decode(String.self, forKey: .message)
    }

    var summary: String {
        switch (code, message) {
        case let (c?, m?): return "\(c): \(m)"
        case let (c?, nil): return c
        case let (nil, m?): return m
        default: return "error"
        }
    }
}

/// One entry of `models` (the registry) from GET /admin/models.
struct RegistryModel: Decodable, Sendable, Identifiable, Hashable {
    var id: String
    var family: String?
    var capabilities: [String]?
    var hf_repo: String?
}

struct Peer: Decodable, Sendable, Identifiable, Hashable {
    var id: String
    var url: String?
    var models: [String]?
}

/// GET /admin/models.
struct AdminModels: Decodable, Sendable {
    var models: [RegistryModel]?
    var workers: [Worker]?
    var peers: [Peer]?
}

/// GET /internal/hardware (also nested in each /internal/hosts entry).
struct Hardware: Decodable, Sendable, Hashable {
    struct RAM: Decodable, Sendable, Hashable {
        var total_bytes: Int64?
        var available_bytes: Int64?
        var swap_total_bytes: Int64?
        var swap_used_bytes: Int64?
    }
    struct LoadAverage: Decodable, Sendable, Hashable {
        var one: Double?
        var five: Double?
        var fifteen: Double?
    }
    struct Accelerator: Decodable, Sendable, Hashable {
        var vendor: String?
        var name: String?
        var cores: FlexString?
        var memory_kind: String?
        var total_memory_bytes: Int64?
        var available_memory_bytes: Int64?
        var driver: FlexString?
        var note: String?
    }
    struct Admission: Decodable, Sendable, Hashable {
        var ram_reserve_bytes: Int64?
        var accelerator_reserve_bytes: Int64?
    }

    var host_id: String?
    var hostname: String?
    var cpu_brand: String?
    var logical_cpus: Int?
    var architecture: String?
    var kernel: String?
    var ram: RAM?
    var memory_stall_percent: Double?
    var load_average: LoadAverage?
    var accelerators: [Accelerator]?
    var managed_workers: [Worker]?
    var admission: Admission?
    var issues: [FlexString]?
}

/// One entry of `hosts` from GET /internal/hosts. The first is the local host and carries no
/// `round_trip_ms`.
struct HostStatus: Decodable, Sendable, Identifiable, Hashable {
    var host: String
    var reachable: Bool?
    var round_trip_ms: Double?
    var status: Int?
    var error: String?
    var hardware: Hardware?

    var id: String { host }
    var modelCount: Int? { hardware?.managed_workers?.count }
}

/// An `Operation` from POST /admin/models/{id}/{action} and GET /admin/operations/{id}.
struct Operation: Decodable, Sendable, Hashable, Identifiable {
    var id: String
    var model: String?
    var action: String?
    /// `running`, `succeeded` or `failed` (src/lifecycle.rs).
    var status: String?
    var elapsed_ms: Double?
    var error: ServerError?

    var isRunning: Bool { status == "running" }
    var summary: String {
        var s = "\(action ?? "operation") \(model ?? "") \(status ?? "?")"
        if let ms = elapsed_ms { s += String(format: " in %.0f ms", ms) }
        if let e = error { s += " (\(e.summary))" }
        return s
    }
}

/// GET /internal/metrics.
struct Metrics: Decodable, Sendable {
    var uptime_seconds: Double?
    var workers: [Worker]?
    var operations: [Operation]?
}

/// One entry of `data` from GET /v1/models.
struct PublicModel: Decodable, Sendable, Identifiable, Hashable {
    var id: String
    var family: String?
    var capabilities: [String]?
    var deployment_configured: Bool?

    var servable: Bool { deployment_configured == true }
    var canChat: Bool { (capabilities ?? []).contains("chat") }
}

// MARK: - Errors

enum ClientError: Error, CustomStringConvertible, Sendable {
    case badURL
    case noAdminKey
    case unauthorized
    case http(Int, ServerError?)
    case transport(String)
    case decode(String)
    case timedOut(String)

    var description: String {
        switch self {
        case .badURL: return "invalid service URL"
        case .noAdminKey: return "no admin key (configure `keys` in app.json)"
        case .unauthorized: return "unauthorized (key rejected)"
        case .http(let code, let e): return "HTTP \(code)" + (e.map { " \($0.summary)" } ?? "")
        case .transport(let m): return m
        case .decode(let m): return "unexpected response: \(m)"
        case .timedOut(let m): return m
        }
    }

    /// True when nothing is listening (the engine is stopped rather than broken).
    var isConnectionRefused: Bool {
        if case .transport(let m) = self { return m == "connection refused" }
        return false
    }
}

// MARK: - Client

/// Thin HTTP client for a granited router. Holds the keys in memory only; they are sent as a
/// bearer header and never logged or included in errors.
struct GraniteClient: Sendable {
    let baseURL: String
    let adminKey: String?
    var apiKey: String? = nil
    var timeout: TimeInterval = 3

    private func session(timeout: TimeInterval) -> URLSession {
        let cfg = URLSessionConfiguration.ephemeral
        cfg.timeoutIntervalForRequest = timeout
        cfg.timeoutIntervalForResource = timeout
        cfg.requestCachePolicy = .reloadIgnoringLocalCacheData
        return URLSession(configuration: cfg)
    }

    func url(_ path: String) throws -> URL {
        guard let u = URL(string: trimSlash(baseURL) + path), u.scheme != nil, u.host != nil else {
            throw ClientError.badURL
        }
        return u
    }

    /// GET /healthz (public). Returns round-trip milliseconds on `{"status":"ok"}`.
    func health() async throws -> Double {
        let started = Date()
        let data = try await send(path: "/healthz", method: "GET", auth: .none)
        struct H: Decodable { var status: String? }
        let h = try decode(H.self, data)
        guard h.status == "ok" else { throw ClientError.decode("status=\(h.status ?? "nil")") }
        return Date().timeIntervalSince(started) * 1000
    }

    /// GET /admin/models: registry, local workers and configured peers.
    func adminModels() async throws -> AdminModels {
        try decode(AdminModels.self, try await send(path: "/admin/models", method: "GET", auth: .admin))
    }

    /// GET /admin/models → `workers` only.
    func workers() async throws -> [Worker] { try await adminModels().workers ?? [] }

    /// GET /internal/hosts. The router probes every peer (5 s timeout each, concurrently), so
    /// this call gets a longer client timeout and is never on the 5 s health cadence.
    func hosts() async throws -> [HostStatus] {
        struct R: Decodable { var hosts: [HostStatus]? }
        return try decode(R.self, try await send(path: "/internal/hosts", method: "GET", auth: .admin, timeout: 8)).hosts ?? []
    }

    /// GET /internal/hardware.
    func hardware() async throws -> Hardware {
        try decode(Hardware.self, try await send(path: "/internal/hardware", method: "GET", auth: .admin))
    }

    /// GET /internal/metrics.
    func metrics() async throws -> Metrics {
        try decode(Metrics.self, try await send(path: "/internal/metrics", method: "GET", auth: .admin))
    }

    /// GET /v1/models. The router accepts the admin key on /v1 too; the API key is the fallback.
    func publicModels() async throws -> [PublicModel] {
        struct R: Decodable { var data: [PublicModel]? }
        return try decode(R.self, try await send(path: "/v1/models", method: "GET", auth: .any)).data ?? []
    }

    enum ModelAction: String, Sendable { case load, unload }

    /// POST /admin/models/{id}/{load|unload}. Returns the accepted operation (202).
    func modelAction(_ action: ModelAction, model: String) async throws -> Operation {
        let data = try await send(path: "/admin/models/\(try segment(model))/\(action.rawValue)", method: "POST", auth: .admin, timeout: 15)
        return try decode(Operation.self, data)
    }

    /// GET /admin/operations/{id}.
    func operation(_ id: String) async throws -> Operation {
        try decode(Operation.self, try await send(path: "/admin/operations/\(try segment(id))", method: "GET", auth: .admin))
    }

    /// Polls an operation until its status is no longer `running`.
    func waitForOperation(_ id: String, every interval: Duration = .milliseconds(500),
                          limit: Duration = .seconds(180)) async throws -> Operation {
        let deadline = ContinuousClock.now + limit
        while true {
            let op = try await operation(id)
            if !op.isRunning { return op }
            if ContinuousClock.now >= deadline { throw ClientError.timedOut("operation \(id) still running") }
            try await Task.sleep(for: interval)
        }
    }

    private func segment(_ s: String) throws -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/")
        guard let seg = s.addingPercentEncoding(withAllowedCharacters: allowed) else { throw ClientError.badURL }
        return seg
    }

    private enum Auth { case none, admin, any }

    private func send(path: String, method: String, auth: Auth, timeout: TimeInterval? = nil) async throws -> Data {
        var req = URLRequest(url: try url(path))
        req.httpMethod = method
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        switch auth {
        case .none: break
        case .admin:
            guard let adminKey, !adminKey.isEmpty else { throw ClientError.noAdminKey }
            req.setValue("Bearer \(adminKey)", forHTTPHeaderField: "Authorization")
        case .any:
            guard let key = [adminKey, apiKey].compactMap({ $0 }).first(where: { !$0.isEmpty }) else {
                throw ClientError.noAdminKey
            }
            req.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
        }
        let data: Data
        let response: URLResponse
        let s = session(timeout: timeout ?? self.timeout)
        defer { s.finishTasksAndInvalidate() }
        do {
            (data, response) = try await s.data(for: req)
        } catch let e as URLError {
            throw ClientError.transport(Self.describe(e))
        } catch {
            throw ClientError.transport("request failed")
        }
        guard let http = response as? HTTPURLResponse else { throw ClientError.transport("no HTTP response") }
        switch http.statusCode {
        case 200..<300: return data
        case 401, 403: throw ClientError.unauthorized
        default: throw ClientError.http(http.statusCode, try? JSONDecoder().decode(ServerError.self, from: data))
        }
    }

    private func decode<T: Decodable>(_ type: T.Type, _ data: Data) throws -> T {
        do { return try JSONDecoder().decode(type, from: data) } catch {
            throw ClientError.decode(String(describing: type))
        }
    }

    private static func describe(_ e: URLError) -> String {
        switch e.code {
        case .cannotConnectToHost: return "connection refused"
        case .timedOut: return "timed out"
        case .cannotFindHost: return "host not found"
        case .notConnectedToInternet, .networkConnectionLost: return "network unavailable"
        case .appTransportSecurityRequiresSecureConnection: return "blocked by App Transport Security"
        default: return "network error \(e.code.rawValue)"
        }
    }
}
