import Foundation

// MARK: - Values the notch shows

/// A notch value: measured, waiting for enough real history, or not reported by the engine.
/// Nothing in the notch is simulated; a value the engine does not report says why.
enum Reading<Value: Sendable & Equatable>: Sendable, Equatable {
    case value(Value)
    case collecting(String)
    case unavailable(String)

    var value: Value? { if case .value(let v) = self { v } else { nil } }
}

struct TimePoint: Sendable, Equatable { var at: Date; var value: Double }
struct SlotUsage: Sendable, Equatable { var occupied: Int; var capacity: Int }
struct TokenRange: Sendable, Equatable { var low: Int64; var high: Int64 }

/// One power sensor, reported once per physical device, with the scope the sensor measures.
struct PowerReading: Sendable, Equatable, Identifiable {
    var id: String
    var host: String
    var device: String
    var watts: Double?
    var scope: String
    var reason: String?
    /// Integrated over today's valid sample intervals only (gaps are not bridged).
    var wattHoursToday: Double?
}

struct MemoryGauge: Sendable, Equatable, Identifiable {
    var id: String
    var host: String
    var label: String
    var usedBytes: Int64
    var totalBytes: Int64
}

struct NodeLine: Sendable, Equatable, Identifiable {
    var id: String { host }
    var host: String
    var reachable: Bool
    var activeRequests: Int
    /// nil: this node's engine does not report usage yet (update pending).
    var usageReported: Bool?
    var powerSensors: Int
}

/// One measured production source. Sources are shown side by side and never summed: a local
/// oMLX server can also be a backend of the fleet engine, so the same request may be in both.
struct ProductionSource: Sendable, Equatable, Identifiable {
    var id: String
    var name: String
    /// What the counters cover, e.g. "Requests run by LocalFleet engines".
    var scope: String
    var rate: Reading<Double>
    var history: [TimePoint]
    var today: Reading<Int64>
    var prompt: Reading<Int64>
    var cached: Reading<Int64>
    var coverageStart: Date?
    var gaps: Int
    var estimate: Reading<TokenRange>
    var note: String?
}

/// A loaded or loading model on one node, with its measured pressure.
struct ModelLine: Sendable, Equatable, Identifiable {
    var id: String { "\(source)/\(host)/\(model)" }
    var source: String
    var host: String
    var model: String
    var state: String
    var active: Int
    var waiting: Int?
    var occupied: Int?
    var capacity: Int?
    /// Measured phase detail, e.g. "prefill 1.2k / 4k · ETA 3 s" or "decoding 41 tok/s".
    var detail: String?
}

enum Freshness: Sendable, Equatable {
    case live
    case stale(since: Date)
    case offline(String)
}

// MARK: - Usage ledger

/// Turns per-host monotonic usage counters into output observed today. The first sample of a
/// host, an engine restart, a counter that went backwards, a sample gap longer than `maxGap`
/// and a midnight boundary all start a new baseline: nothing is counted for that interval, so
/// there are no spikes and backend lifetime totals are never counted as today's.
struct UsageLedger: Codable, Equatable, Sendable {
    struct Counters: Codable, Equatable, Sendable {
        var since: Double
        var output: Int64
        var prompt: Int64
        var cached: Int64
    }
    struct Baseline: Codable, Equatable, Sendable {
        var counters: Counters
        var at: Date
    }
    struct Point: Codable, Equatable, Sendable {
        var at: Date
        var output: Int64
    }

    static let maxGap: TimeInterval = 30
    static let seriesSpan: TimeInterval = 3_700

    /// Local calendar day of the totals, "yyyy-MM-dd".
    var day = ""
    var output: Int64 = 0
    var prompt: Int64 = 0
    var cached: Int64 = 0
    var outputByHost: [String: Int64] = [:]
    /// First observation today; totals cover [coverageStart, now] minus `gaps`.
    var coverageStart: Date?
    /// Times continuity broke today (restart, reset, long gap, clock change, app closed, a node
    /// starting or stopping to report).
    var gaps = 0
    /// Start of the current unbroken stretch, and `output` at that moment, for the pace.
    var continuousSince: Date?
    var outputAtContinuousStart: Int64 = 0
    /// Hosts that reported in the previous poll; a change restarts the rate window.
    var reportingHosts: [String] = []
    /// When the totals last rolled to a new local day; baselines taken before it never count.
    var rolledAt: Date?
    /// Consecutive polls each reporting host has been missing; a host leaves the coverage set only
    /// after `missedPollsAllowed` (one flaky reply doesn't restart the forecast's hour).
    var missedPolls: [String: Int] = [:]
    var baselines: [String: Baseline] = [:]
    /// Cumulative `output` after each poll of the current coverage set, for the rolling rate.
    /// Not persisted: after a relaunch the rate restarts anyway.
    var series: [Point] = []

    static let missedPollsAllowed = 2

    init() {}

    private enum CodingKeys: String, CodingKey {
        case day, output, prompt, cached, outputByHost, coverageStart, gaps, continuousSince
        case outputAtContinuousStart, reportingHosts, rolledAt, missedPolls, baselines
    }

    /// Tolerates missing keys, so adding a field never wipes today's totals.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        day = try c.decodeIfPresent(String.self, forKey: .day) ?? ""
        output = try c.decodeIfPresent(Int64.self, forKey: .output) ?? 0
        prompt = try c.decodeIfPresent(Int64.self, forKey: .prompt) ?? 0
        cached = try c.decodeIfPresent(Int64.self, forKey: .cached) ?? 0
        outputByHost = try c.decodeIfPresent([String: Int64].self, forKey: .outputByHost) ?? [:]
        coverageStart = try c.decodeIfPresent(Date.self, forKey: .coverageStart)
        gaps = try c.decodeIfPresent(Int.self, forKey: .gaps) ?? 0
        continuousSince = try c.decodeIfPresent(Date.self, forKey: .continuousSince)
        outputAtContinuousStart = try c.decodeIfPresent(Int64.self, forKey: .outputAtContinuousStart) ?? 0
        reportingHosts = try c.decodeIfPresent([String].self, forKey: .reportingHosts) ?? []
        rolledAt = try c.decodeIfPresent(Date.self, forKey: .rolledAt)
        missedPolls = try c.decodeIfPresent([String: Int].self, forKey: .missedPolls) ?? [:]
        baselines = try c.decodeIfPresent([String: Baseline].self, forKey: .baselines) ?? [:]
    }

    static func dayKey(_ date: Date, _ calendar: Calendar) -> String {
        let c = calendar.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0)
    }

    /// Starts a new local day (midnight or a time-zone change) when `now` is on another day than
    /// the totals. Returns true when it rolled. Safe to call on every publish.
    @discardableResult
    mutating func roll(to now: Date, calendar: Calendar) -> Bool {
        let today = Self.dayKey(now, calendar)
        guard today != day else { return false }
        day = today
        output = 0; prompt = 0; cached = 0; outputByHost = [:]
        coverageStart = nil; gaps = 0; series = []; continuousSince = nil
        rolledAt = now
        return true
    }

    /// Records one poll. `hosts` holds one entry per host that reported usage in this poll;
    /// a repeated host id is ignored.
    mutating func observe(_ hosts: [(host: String, counters: Counters)], at now: Date, calendar: Calendar) {
        // A new local day drops the interval that spans it.
        let rolled = roll(to: now, calendar: calendar)
        var seen = Set<String>()
        let unique = hosts.filter { seen.insert($0.host).inserted }
        // A host missing from this poll stays in the coverage set for a few polls. When it returns,
        // its absent interval is not counted (a new baseline for that host only, no fleet break).
        let returning = Set(seen.filter { (missedPolls[$0] ?? 0) > 0 })
        for h in reportingHosts where !seen.contains(h) { missedPolls[h, default: 0] += 1 }
        for h in seen { missedPolls[h] = nil }
        let tolerated = reportingHosts.filter { !seen.contains($0) && (missedPolls[$0] ?? 0) <= Self.missedPollsAllowed }
        for h in reportingHosts where !seen.contains(h) && !tolerated.contains(h) { missedPolls[h] = nil }
        let set = (unique.map(\.host) + tolerated).sorted()
        var broken = rolled || continuousSince == nil
        let setChanged = set != reportingHosts
        if setChanged {
            if !reportingHosts.isEmpty && !rolled { gaps += 1 }
            series = []
            broken = true
        }
        reportingHosts = set
        if let last = series.last, now <= last.at { series = []; broken = true }
        for (host, c) in unique {
            defer { baselines[host] = Baseline(counters: c, at: now) }
            if coverageStart == nil { coverageStart = now }
            guard let base = baselines[host], !rolled, rolledAt.map({ base.at >= $0 }) ?? true else {
                broken = true
                continue
            }
            if returning.contains(host) { continue }
            let b = base.counters
            let dt = now.timeIntervalSince(base.at)
            let continuous = b.since == c.since && dt > 0 && dt <= Self.maxGap
                && c.output >= b.output && c.prompt >= b.prompt && c.cached >= b.cached
            guard continuous else {
                if !setChanged { gaps += 1 }
                broken = true
                continue
            }
            output += c.output - b.output
            prompt += c.prompt - b.prompt
            cached += c.cached - b.cached
            outputByHost[host, default: 0] += c.output - b.output
        }
        guard !unique.isEmpty else { continuousSince = nil; return }
        if broken { continuousSince = now; outputAtContinuousStart = output }
        series.append(Point(at: now, output: output))
        series.removeAll { now.timeIntervalSince($0.at) > Self.seriesSpan }
    }

    /// Completed output per second over the last `window` seconds, once the series of the
    /// current unbroken stretch spans it and the newest point is fresh. nil (not zero) when
    /// nothing is being measured, and for a window after any gap, restart or relaunch.
    func rollingRate(window: TimeInterval, at now: Date) -> Double? {
        guard !reportingHosts.isEmpty, let since = continuousSince, let last = series.last,
              now.timeIntervalSince(last.at) <= Self.maxGap,
              let start = series.last(where: { now.timeIntervalSince($0.at) >= window }), start.at >= since else { return nil }
        let dt = last.at.timeIntervalSince(start.at)
        return dt > 0 ? Double(last.output - start.output) / dt : nil
    }

    /// At-this-pace output by midnight: today's total plus the remaining hours at the slower and
    /// the faster of the unbroken-stretch pace (idle time included) and the last hour's pace. nil
    /// until the current unbroken stretch, with the same nodes reporting, lasts `minimum` seconds.
    func estimate(at now: Date, calendar: Calendar, minimum: TimeInterval = 3_600) -> TokenRange? {
        guard let start = continuousSince, now.timeIntervalSince(start) >= minimum,
              let last = series.last,
              let hourAgo = series.last(where: { now.timeIntervalSince($0.at) >= 3_600 }),
              let midnight = calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: now)) else { return nil }
        let stretchPace = Double(output - outputAtContinuousStart) / now.timeIntervalSince(start)
        let hourPace = Double(last.output - hourAgo.output) / max(1, last.at.timeIntervalSince(hourAgo.at))
        let remaining = midnight.timeIntervalSince(now)
        let low = output + Int64((min(stretchPace, hourPace) * remaining).rounded())
        let high = output + Int64((max(stretchPace, hourPace) * remaining).rounded())
        return TokenRange(low: low, high: high)
    }
}

// MARK: - Energy ledger

/// Integrates power per sensor over today's valid intervals: both ends measured, no longer than
/// `maxGap` apart, on the same day. A missing reading, a longer gap or a clock that moved back
/// ends the interval.
struct EnergyLedger: Codable, Equatable, Sendable {
    struct Last: Codable, Equatable, Sendable { var watts: Double; var at: Date }

    static let maxGap: TimeInterval = 30

    var day = ""
    var wattHours: [String: Double] = [:]
    var last: [String: Last] = [:]
    /// First valid reading today, so "Wh today" can say what it covers.
    var coverageStart: Date?

    init() {}

    private enum CodingKeys: String, CodingKey { case day, wattHours, last, coverageStart }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        day = try c.decodeIfPresent(String.self, forKey: .day) ?? ""
        wattHours = try c.decodeIfPresent([String: Double].self, forKey: .wattHours) ?? [:]
        last = try c.decodeIfPresent([String: Last].self, forKey: .last) ?? [:]
        coverageStart = try c.decodeIfPresent(Date.self, forKey: .coverageStart)
    }

    mutating func roll(to now: Date, calendar: Calendar) {
        let today = UsageLedger.dayKey(now, calendar)
        if today != day { day = today; wattHours = [:]; last = [:]; coverageStart = nil }
    }

    mutating func observe(sensor: String, watts: Double?, at now: Date, calendar: Calendar) {
        roll(to: now, calendar: calendar)
        guard let watts, watts.isFinite, watts >= 0 else { last[sensor] = nil; return }
        if coverageStart == nil { coverageStart = now }
        if let l = last[sensor], case let dt = now.timeIntervalSince(l.at), dt > 0, dt <= Self.maxGap,
           l.at >= calendar.startOfDay(for: now) {
            wattHours[sensor, default: 0] += (l.watts + watts) / 2 * now.timeIntervalSince(l.at) / 3_600
        }
        last[sensor] = Last(watts: watts, at: now)
    }
}

// MARK: - Engine replies (only the fields the notch reads)

struct UsageBlock: Decodable, Sendable, Equatable {
    struct Model: Decodable, Sendable, Equatable {
        var requests_completed: Int64?
        var requests_without_usage: Int64?
        var prompt_tokens: Int64?
        var cached_prompt_tokens: Int64?
        var output_tokens: Int64?
    }
    var since_unix_ms: Double?
    var usage_reported: Bool?
    var models: [String: Model]?

    var counters: UsageLedger.Counters? {
        guard let since = since_unix_ms else { return nil }
        let m = (models ?? [:]).values
        return UsageLedger.Counters(since: since,
                                    output: m.reduce(0) { $0 + ($1.output_tokens ?? 0) },
                                    prompt: m.reduce(0) { $0 + ($1.prompt_tokens ?? 0) },
                                    cached: m.reduce(0) { $0 + ($1.cached_prompt_tokens ?? 0) })
    }
}

struct NotchWorker: Decodable, Sendable, Equatable {
    var model: String
    var state: String?
    var backend: String?
    var active_requests: Int?
    var occupied_slots: Int?
    var max_concurrency: Int?
}

struct NotchMetricsReply: Decodable, Sendable {
    var workers: [NotchWorker]?
    var operations: [Operation]?
    var rejections: [String: [String: Int64]]?
}

struct NotchHostsReply: Decodable, Sendable {
    struct Power: Decodable, Sendable, Equatable {
        var watts: Double?
        var limit_watts: Double?
        var scope: String?
        var source: String?
        var reason: String?
    }
    struct Accelerator: Decodable, Sendable, Equatable {
        var vendor: String?
        var name: String?
        var uuid: String?
        var device_id: String?
        var vendor_pci_id: String?
        var node: String?
        var memory_kind: String?
        var total_memory_bytes: Int64?
        var available_memory_bytes: Int64?
        var power: Power?
    }
    struct Hardware: Decodable, Sendable {
        var ram: Hardware_RAM?
        var accelerators: [Accelerator]?
        var managed_workers: [NotchWorker]?
    }
    struct Hardware_RAM: Decodable, Sendable {
        var total_bytes: Int64?
        var available_bytes: Int64?
    }
    /// Self-declared X-LocalFleet-Client labels counted on the entry gateway (counts only).
    struct Clients: Decodable, Sendable, Equatable {
        var window_seconds: Double?
        var active: [String: Int]?
        var unidentified_active: Int?
        var last_seen_unix_ms: [String: Double]?
    }
    struct Host: Decodable, Sendable {
        var host: String
        var reachable: Bool?
        var hardware: Hardware?
        var usage: UsageBlock?
        var usage_error: String?
        var clients: Clients?
    }
    var hosts: [Host]
}

/// GET /api/status of a local oMLX server: process-lifetime counters for all of its clients.
struct OMLXStatus: Decodable, Sendable, Equatable {
    var uptime_seconds: Double?
    var total_requests: Int64?
    var active_requests: Int?
    var waiting_requests: Int?
    var total_prompt_tokens: Int64?
    var total_completion_tokens: Int64?
    var total_cached_tokens: Int64?
    var model_memory_used: Int64?
    var model_memory_max: Int64?
}

/// GET /admin/api/activity of a local oMLX server: live per-model phases (counts and progress
/// only; request ids and prompt text are not read).
struct OMLXActivity: Decodable, Sendable, Equatable {
    struct Prefill: Decodable, Sendable, Equatable {
        var processed: Double?
        var total: Double?
        var eta: Double?
    }
    struct Generate: Decodable, Sendable, Equatable {
        var generated_tokens: Double?
        /// oMLX's generated_tokens / elapsed since generation started: an average, not live.
        var tokens_per_second: Double?
        var last_activity_age_seconds: Double?
    }
    struct Model: Decodable, Sendable, Equatable {
        var id: String
        var is_loading: Bool?
        var active_requests: Int?
        var waiting_requests: Int?
        var prefilling: [Prefill]?
        var generating: [Generate]?
    }
    struct Pressure: Decodable, Sendable, Equatable { var pressure_level: String? }
    struct Models: Decodable, Sendable, Equatable {
        var models: [Model]?
        var memory_pressure: Pressure?
    }
    var active_models: Models
}

// MARK: - Snapshot (pure derivation, testable)

/// Everything the notch shows, derived from one hosts reply, the latest local metrics and the
/// ledgers. Hosts are deduplicated by id; usage is taken from the hosts reply only, one entry per
/// host, so a coordinator's forward and the peer's execution are never both counted.
struct NotchSnapshot: Sendable, Equatable {
    static let rateWindow: TimeInterval = 60

    var outputRate: Reading<Double> = .unavailable("Engine update pending: no token usage reported yet")
    var rateHistory: [TimePoint] = []
    var outputToday: Reading<Int64> = .unavailable("Engine update pending: no token usage reported yet")
    var promptToday: Reading<Int64> = .unavailable("Not reported")
    var cachedToday: Reading<Int64> = .unavailable("Not reported")
    var coverageStart: Date?
    var coverageGaps = 0
    var usageNote: String?
    var estimate: Reading<TokenRange> = .collecting("Collecting history")
    var activeRequests: Reading<Int> = .unavailable("Engine not reachable")
    var slots: Reading<SlotUsage> = .unavailable("Engine not reachable")
    var loadingModels: [String] = []
    var rejections: Reading<Int64> = .unavailable("Engine not reachable")
    var apps: Reading<Int> = .unavailable("Apps share one API key, so the engine can't tell them apart")
    /// Labels seen in the window, in flight now with a label, and in flight without one.
    var appLabels: [String] = []
    var labelledActive: Int?
    var unidentifiedActive: Int?
    var appsNote: String?
    var power: [PowerReading] = []
    var powerCoverage = ""
    var memory: [MemoryGauge] = []
    var nodes: [NodeLine] = []
    /// Name of the source behind the headline rate ("" when none is measured).
    var productionSource = ""
    var sources: [ProductionSource] = []
    var models: [ModelLine] = []
    var omlxPressure: String?

    /// Unique hosts in reply order (first entry wins), so a repeated host is never counted twice.
    static func uniqueHosts(_ reply: NotchHostsReply) -> [NotchHostsReply.Host] {
        var seen = Set<String>()
        return reply.hosts.filter { seen.insert($0.host).inserted }
    }

    /// Accelerators reported once per physical device: a bare `/sys/class` node entry is dropped
    /// when a named entry from the same vendor exists on the host.
    static func physicalAccelerators(_ list: [NotchHostsReply.Accelerator]) -> [NotchHostsReply.Accelerator] {
        let vendorOf: (NotchHostsReply.Accelerator) -> String? = { a in
            switch a.vendor_pci_id?.lowercased() {
            case "0x10de": "nvidia"
            case "0x1002", "0x1022": "amd"
            default: a.vendor?.lowercased()
            }
        }
        let named = Set(list.filter { $0.name != nil }.compactMap(vendorOf))
        var seen = Set<String>()
        return list.filter { a in
            if a.name == nil, let v = vendorOf(a), named.contains(v), a.power == nil { return false }
            let key = a.uuid ?? a.name.map { "\($0)#\(a.device_id ?? "")" } ?? "\(a.node ?? "")#\(a.device_id ?? "")"
            return seen.insert(key).inserted
        }
    }
}

extension NotchSnapshot {
    static let engineSourceName = "LocalFleet fleet"
    static let omlxSourceName = "oMLX on this Mac"
    static let pendingUsage = "Engine update pending: no token usage reported yet"

    /// Builds the snapshot. `hosts` is nil before the first hosts reply or when it is stale;
    /// `metrics` is the latest local /internal/metrics (fresher for this node's requests,
    /// operations and rejections); `omlx` is a local oMLX server's status, when one is running.
    static func derive(hosts reply: NotchHostsReply?, metrics: NotchMetricsReply?, usage: UsageLedger,
                       energy: EnergyLedger, omlx: OMLXStatus? = nil, omlxActivity: OMLXActivity? = nil,
                       omlxLedger: UsageLedger = UsageLedger(), at now: Date, calendar: Calendar) -> NotchSnapshot {
        var s = NotchSnapshot()
        var usage = usage, energy = energy, omlxLedger = omlxLedger
        usage.roll(to: now, calendar: calendar)
        energy.roll(to: now, calendar: calendar)
        omlxLedger.roll(to: now, calendar: calendar)
        let hosts = reply.map(uniqueHosts) ?? []

        // Demand: this node from the 2 s metrics, peers from their last hardware report.
        var nodes: [NodeLine] = []
        var models: [ModelLine] = []
        var active = 0, occupied = 0, capacity = 0
        var loading: [String] = []
        var engineUsesOMLX = false
        for (i, h) in hosts.enumerated() {
            let workers = (i == 0 ? metrics?.workers : nil) ?? h.hardware?.managed_workers ?? []
            if i == 0 { engineUsesOMLX = workers.contains { $0.backend == "omlx" } }
            let hostActive = workers.reduce(0) { $0 + ($1.active_requests ?? 0) }
            if h.reachable != false {
                active += hostActive
                for w in workers where w.state == "ready" {
                    occupied += w.occupied_slots ?? w.active_requests ?? 0
                    capacity += w.max_concurrency ?? 0
                }
                loading += workers.filter { $0.state == "loading" }.map { "\($0.model) on \(h.host)" }
                for w in workers where (w.state ?? "unloaded") != "unloaded" {
                    models.append(ModelLine(source: engineSourceName, host: h.host, model: w.model, state: w.state ?? "unknown",
                                            active: w.active_requests ?? 0, waiting: nil,
                                            occupied: w.occupied_slots, capacity: w.max_concurrency, detail: nil))
                }
            }
            let sensors = physicalAccelerators(h.hardware?.accelerators ?? []).filter { $0.power?.watts != nil }.count
            nodes.append(NodeLine(host: h.host, reachable: h.reachable != false, activeRequests: hostActive,
                                  usageReported: h.usage == nil ? nil : (h.usage?.usage_reported ?? true),
                                  powerSensors: sensors))
        }
        if hosts.isEmpty, let w = metrics?.workers { engineUsesOMLX = w.contains { $0.backend == "omlx" } }
        if reply != nil || metrics != nil {
            if reply == nil, let w = metrics?.workers {
                active = w.reduce(0) { $0 + ($1.active_requests ?? 0) }
            }
            s.activeRequests = .value(active)
            s.slots = capacity > 0 ? .value(SlotUsage(occupied: occupied, capacity: capacity))
                : .unavailable("No model is loaded")
        }
        if let ops = metrics?.operations {
            loading += ops.filter { $0.isRunning && $0.action == "load" }.compactMap(\.model)
                .filter { m in !loading.contains { $0.hasPrefix(m + " ") } }
        }
        if let r = metrics?.rejections {
            s.rejections = .value(r.values.reduce(0) { $0 + $1.values.reduce(0, +) })
        }
        s.nodes = nodes

        // Apps: only callers that send an X-LocalFleet-Client label, counted on the entry gateway.
        let blocks = hosts.filter { $0.reachable != false }.compactMap(\.clients)
        if blocks.isEmpty {
            if reply != nil { s.apps = .unavailable("Engine update pending: no client labels reported yet") }
        } else {
            let nowMs = now.timeIntervalSince1970 * 1_000
            var labels = Set<String>()
            for b in blocks {
                let window = (b.window_seconds ?? 300) * 1_000
                for (label, seen) in b.last_seen_unix_ms ?? [:] where nowMs - seen <= window { labels.insert(label) }
                for (label, n) in b.active ?? [:] where n > 0 { labels.insert(label) }
            }
            let minutes = Int((blocks.first?.window_seconds ?? 300) / 60)
            s.appLabels = labels.sorted()
            s.apps = .value(labels.count)
            s.labelledActive = blocks.reduce(0) { $0 + ($1.active ?? [:]).values.reduce(0, +) }
            s.unidentifiedActive = blocks.reduce(0) { $0 + ($1.unidentified_active ?? 0) }
            s.appsNote = "Apps that label their requests (X-LocalFleet-Client) in the last \(minutes) min. "
                + "Labels are self-declared. Requests without a label, including those through LiteLLM, count as unidentified."
        }

        // Production: each source only from counters it reports; sources are never summed.
        var sources: [ProductionSource] = []
        let reporting = hosts.filter { $0.usage?.counters != nil }
        var engine = ProductionSource(id: "engine", name: engineSourceName, scope: "Requests run by LocalFleet engines, counted once on the node that ran them",
                                      rate: .unavailable(reply == nil ? "Engine not reachable" : pendingUsage), history: [],
                                      today: .unavailable(reply == nil ? "Engine not reachable" : pendingUsage),
                                      prompt: .unavailable("Not reported"), cached: .unavailable("Not reported"),
                                      coverageStart: nil, gaps: 0, estimate: .unavailable("No usage reported"), note: nil)
        if !reporting.isEmpty {
            var notes: [String] = []
            let pending = hosts.filter { $0.usage == nil }.map(\.host)
            if !pending.isEmpty { notes.append("Not reporting yet: " + pending.joined(separator: ", ")) }
            if reporting.contains(where: { $0.usage?.usage_reported == false }) {
                notes.append("Some backends returned no usage; their tokens are not counted")
            }
            fill(&engine, from: usage, at: now, calendar: calendar)
            engine.note = notes.isEmpty ? nil : notes.joined(separator: ". ")
        }
        sources.append(engine)

        if let o = omlx {
            var src = ProductionSource(id: "omlx", name: omlxSourceName, scope: "Every client of the oMLX server on this Mac, not only LocalFleet",
                                       rate: .collecting(""), history: [], today: .collecting(""),
                                       prompt: .collecting(""), cached: .collecting(""), coverageStart: nil, gaps: 0,
                                       estimate: .collecting(""), note: nil)
            fill(&src, from: omlxLedger, at: now, calendar: calendar)
            if engineUsesOMLX { src.note = "LocalFleet also uses this oMLX server, so these tokens overlap the fleet total" }
            sources.append(src)
            for m in omlxActivity?.active_models.models ?? [] {
                models.append(ModelLine(source: omlxSourceName, host: "this Mac", model: m.id,
                                        state: m.is_loading == true ? "loading" : "ready",
                                        active: m.active_requests ?? 0, waiting: m.waiting_requests,
                                        occupied: nil, capacity: nil, detail: phaseDetail(m)))
            }
            s.omlxPressure = omlxActivity?.active_models.memory_pressure?.pressure_level
            if omlxActivity == nil, (o.active_requests ?? 0) > 0 {
                models.append(ModelLine(source: omlxSourceName, host: "this Mac", model: "all models", state: "ready",
                                        active: o.active_requests ?? 0, waiting: o.waiting_requests,
                                        occupied: nil, capacity: nil, detail: nil))
            }
        }
        s.sources = sources
        s.models = models

        // Headline: the fleet when it reports usage; otherwise a local oMLX server that LocalFleet
        // does not route through (so nothing is counted twice); otherwise the fleet's reason.
        let primary = !reporting.isEmpty ? engine
            : (sources.first { $0.id == "omlx" && !engineUsesOMLX } ?? engine)
        s.productionSource = primary.rate.value != nil || primary.today.value != nil ? primary.name : ""
        s.outputRate = primary.rate
        s.rateHistory = primary.history
        s.outputToday = primary.today
        s.promptToday = primary.prompt
        s.cachedToday = primary.cached
        s.coverageStart = primary.coverageStart
        s.coverageGaps = primary.gaps
        s.estimate = primary.estimate
        s.usageNote = primary.id == "engine" ? engine.note
            : [primary.note, reply == nil ? nil : "LocalFleet engines: \(pendingUsage.lowercased())"].compactMap { $0 }.joined(separator: ". ")
        if s.usageNote?.isEmpty == true { s.usageNote = nil }

        // Energy: each physical sensor once, with its own scope; Wh only over valid intervals.
        var power: [PowerReading] = []
        var memory: [MemoryGauge] = []
        for h in hosts where h.reachable != false {
            for a in physicalAccelerators(h.hardware?.accelerators ?? []) {
                let device = a.name ?? a.node ?? a.device_id ?? "accelerator"
                let id = "\(h.host)/\(a.uuid ?? device)"
                if let p = a.power {
                    power.append(PowerReading(id: id, host: h.host, device: device, watts: p.watts,
                                              scope: p.scope ?? "unknown scope", reason: p.reason,
                                              wattHoursToday: energy.wattHours[id]))
                }
                if a.memory_kind == "dedicated", let total = a.total_memory_bytes, let avail = a.available_memory_bytes {
                    memory.append(MemoryGauge(id: id, host: h.host, label: device, usedBytes: total - avail, totalBytes: total))
                }
            }
            if let ram = h.hardware?.ram, let total = ram.total_bytes, let avail = ram.available_bytes {
                memory.append(MemoryGauge(id: "\(h.host)/ram", host: h.host, label: "Memory", usedBytes: total - avail, totalBytes: total))
            }
        }
        if let o = omlx, let used = o.model_memory_used, let max = o.model_memory_max, max > 0 {
            memory.append(MemoryGauge(id: "omlx/models", host: "this Mac", label: "oMLX model memory", usedBytes: used, totalBytes: max))
        }
        s.power = power
        let measured = Set(power.filter { $0.watts != nil }.map(\.host))
        let reachable = hosts.filter { $0.reachable != false }.count
        if reply == nil {
            s.powerCoverage = "Engine not reachable"
        } else if power.isEmpty {
            s.powerCoverage = "Engine update pending: no power sensors reported yet"
        } else {
            s.powerCoverage = "Power measured on \(measured.count) of \(reachable) nodes; sensor scopes differ, so readings are not summed"
            if let since = energy.coverageStart {
                s.powerCoverage += ". Wh counted since " + since.formatted(date: .omitted, time: .shortened)
                    + ", only over intervals of 30 s or less"
            }
        }
        s.memory = memory
        return s
    }

    /// Fills a source's production values from its ledger.
    static func fill(_ src: inout ProductionSource, from ledger: UsageLedger, at now: Date, calendar: Calendar) {
        src.rate = ledger.rollingRate(window: rateWindow, at: now).map { .value($0) }
            ?? .collecting("Measuring the first \(Int(rateWindow)) s")
        src.history = rateHistory(ledger, at: now)
        src.today = .value(ledger.output)
        src.prompt = .value(ledger.prompt)
        src.cached = .value(ledger.cached)
        src.coverageStart = ledger.coverageStart
        src.gaps = ledger.gaps
        src.estimate = ledger.estimate(at: now, calendar: calendar).map { .value($0) }
            ?? .collecting("Collecting history (needs an unbroken hour)")
    }

    /// "prefill 1.2k / 4k · ETA 3 s", "decoding, avg 41 tok/s since start" or nil, from measured
    /// phases only.
    static func phaseDetail(_ m: OMLXActivity.Model) -> String? {
        var parts: [String] = []
        for p in m.prefilling ?? [] {
            guard let done = p.processed, let total = p.total, total > 0 else { continue }
            var text = "prefill \(compact(done)) / \(compact(total))"
            if let eta = p.eta, eta.isFinite, eta >= 0 { text += String(format: " · ETA %.0f s", eta) }
            parts.append(text)
        }
        // Requests without a token for over 5 s are stalled; their average would read as activity.
        let decoding = (m.generating ?? []).filter { ($0.last_activity_age_seconds ?? 0) <= 5 }
            .compactMap(\.tokens_per_second).filter { $0.isFinite && $0 > 0 }
        if !decoding.isEmpty {
            parts.append(String(format: "decoding, avg %.0f tok/s since start", decoding.reduce(0, +))
                         + (decoding.count > 1 ? " across \(decoding.count)" : ""))
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    static func compact(_ v: Double) -> String {
        switch v {
        case 1_000_000...: String(format: "%.1fM", v / 1_000_000)
        case 1_000...: String(format: "%.1fk", v / 1_000)
        default: String(format: "%.0f", v)
        }
    }

    /// Completed-output rate at each poll in the last minute, from consecutive series points.
    static func rateHistory(_ usage: UsageLedger, at now: Date) -> [TimePoint] {
        let since = usage.continuousSince ?? .distantFuture
        let points = usage.series.filter { now.timeIntervalSince($0.at) <= rateWindow + 10 && $0.at >= since }
        return zip(points, points.dropFirst()).compactMap { a, b in
            let dt = b.at.timeIntervalSince(a.at)
            return dt > 0 ? TimePoint(at: b.at, value: Double(b.output - a.output) / dt) : nil
        }
    }
}

// MARK: - Collector

/// How the collector reaches a source; injectable so tests can simulate slow or failing sources.
protocol NotchFetching: Sendable {
    func get<T: Decodable & Sendable>(_ base: String, _ path: String, key: String?, timeout: TimeInterval,
                                      as type: T.Type) async throws -> T
}

struct URLSessionFetcher: NotchFetching {
    private static let session = URLSession(configuration: .ephemeral, delegate: NoRedirects(), delegateQueue: nil)

    /// Never follows a redirect, so a bearer key is only ever sent to the configured host.
    private final class NoRedirects: NSObject, URLSessionTaskDelegate, Sendable {
        func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                        newRequest request: URLRequest) async -> URLRequest? { nil }
    }

    func get<T: Decodable & Sendable>(_ base: String, _ path: String, key: String?, timeout: TimeInterval,
                                      as type: T.Type) async throws -> T {
        guard let url = URL(string: base.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + path) else {
            throw ClientError.badURL
        }
        var request = URLRequest(url: url, timeoutInterval: timeout)
        if let key { request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization") }
        let (data, response) = try await Self.session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw ClientError.http((response as? HTTPURLResponse)?.statusCode ?? 0, nil)
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
}

/// What the collector needs from the app's engine state, read at each poll.
struct EngineAccess: Sendable, Equatable {
    var running: Bool
    var label: String
    var baseURL: String
    var adminKey: String?
}

/// Polls while the notch is shown. Two independent loops, so a slow or missing source never
/// delays the other:
/// - the engine: /internal/metrics every 2 s (this node) and /internal/hosts every 5 s (one entry
///   per node, with usage and power);
/// - a local oMLX server, when one is configured on loopback: /api/status and
///   /admin/api/activity every 2 s, with a short timeout.
/// Each source keeps the time its last response arrived; counters are recorded at that time.
/// Keeps counts and timings only; the daily ledgers persist so a relaunch keeps today's totals
/// (the time it was closed shows up as a coverage gap).
@MainActor
final class NotchTelemetry: ObservableObject {
    static let shared = NotchTelemetry()

    @Published private(set) var snapshot = NotchSnapshot()
    /// Freshness of the source behind the headline (the fleet unless oMLX leads).
    @Published private(set) var freshness: Freshness = .offline("Not started")
    @Published private(set) var engineFreshness: Freshness = .offline("Not started")
    @Published private(set) var omlxFreshness: Freshness = .offline("No local oMLX server")
    /// Fleet active requests at each metrics poll in the last minute (actual counts).
    @Published private(set) var activeHistory: [TimePoint] = []

    var metricsInterval: Duration = .seconds(2)
    var hostsInterval: Duration = .seconds(5)
    var omlxInterval: Duration = .seconds(2)
    var engineTimeout: TimeInterval = 4
    var hostsTimeout: TimeInterval = 8
    var omlxTimeout: TimeInterval = 1.5
    var staleAfter: TimeInterval = 15

    private let fetcher: any NotchFetching
    private let defaults: UserDefaults
    private let omlxEndpoint: @Sendable () -> OMLXEndpoint?
    private let calendar: Calendar
    private var tasks: [Task<Void, Never>] = []
    private var generation = 0
    private var engine: (@MainActor () -> EngineAccess?)?
    private var usage: UsageLedger
    private var energy: EnergyLedger
    private var omlxLedger: UsageLedger
    private var hostsReply: NotchHostsReply?
    private var metricsReply: NotchMetricsReply?
    private var omlxStatus: OMLXStatus?
    private var omlxActivity: OMLXActivity?
    private var omlxStart: Double?
    private var hostsAt: Date?
    private var metricsAt: Date?
    private var omlxAt: Date?
    private var engineOffline: String? = "Not started"

    init(fetcher: any NotchFetching = URLSessionFetcher(), defaults: UserDefaults = .standard,
         calendar: Calendar = .autoupdatingCurrent,
         omlxEndpoint: @escaping @Sendable () -> OMLXEndpoint? = { OMLXEndpoint.load() }) {
        self.fetcher = fetcher
        self.defaults = defaults
        self.calendar = calendar
        self.omlxEndpoint = omlxEndpoint
        usage = Self.load(UsageLedger.self, "notch.usageLedger", defaults) ?? UsageLedger()
        energy = Self.load(EnergyLedger.self, "notch.energyLedger", defaults) ?? EnergyLedger()
        omlxLedger = Self.load(UsageLedger.self, "notch.omlxLedger", defaults) ?? UsageLedger()
    }

    var isRunning: Bool { !tasks.isEmpty }

    func start(state: AppState) {
        start { [weak state] in
            state.map { EngineAccess(running: $0.engine.isRunning, label: $0.engine.label,
                                     baseURL: $0.client.baseURL, adminKey: $0.client.adminKey) }
        }
    }

    func start(engine: @escaping @MainActor () -> EngineAccess?) {
        guard tasks.isEmpty else { return }
        generation += 1
        let gen = generation
        self.engine = engine
        tasks = [
            Task { [weak self] in await self?.metricsLoop(gen) },
            Task { [weak self] in await self?.hostsLoop(gen) },
            Task { [weak self] in await self?.omlxLoop(gen) },
        ]
    }

    func stop() {
        generation += 1
        tasks.forEach { $0.cancel() }
        tasks = []
    }

    /// True while `gen` is the running generation; a loop from before stop() must not write.
    private func current(_ gen: Int) -> Bool { gen == generation && !Task.isCancelled }

    // MARK: Engine

    private func metricsLoop(_ gen: Int) async {
        while current(gen) {
            await pollMetrics(gen)
            try? await Task.sleep(for: metricsInterval)
        }
    }

    private func hostsLoop(_ gen: Int) async {
        while current(gen) {
            await pollHosts(gen)
            try? await Task.sleep(for: hostsInterval)
        }
    }

    /// One metrics poll and one hosts poll (tests; the app runs them in separate loops).
    func pollEngine() async {
        await pollMetrics()
        await pollHosts()
    }

    /// The engine as configured now, or nil after marking it offline.
    private func runningEngine(_ gen: Int) -> EngineAccess? {
        guard let access = engine?(), access.running else {
            engineOffline = engine?()?.label ?? "Engine not configured"
            hostsReply = nil; metricsReply = nil; hostsAt = nil; metricsAt = nil
            if current(gen) { publish() }
            return nil
        }
        engineOffline = nil
        return access
    }

    func pollMetrics(_ gen: Int? = nil) async {
        let gen = gen ?? generation
        guard let access = runningEngine(gen) else { return }
        let metrics = try? await fetcher.get(access.baseURL, "/internal/metrics", key: access.adminKey,
                                             timeout: engineTimeout, as: NotchMetricsReply.self)
        guard current(gen) else { return }
        if let metrics {
            let now = Date()
            metricsReply = metrics
            metricsAt = now
            let peers = NotchSnapshot.uniqueHosts(freshHosts(at: now) ?? NotchHostsReply(hosts: [])).dropFirst()
                .filter { $0.reachable != false }
                .reduce(0) { $0 + ($1.hardware?.managed_workers ?? []).reduce(0) { $0 + ($1.active_requests ?? 0) } }
            let local = (metrics.workers ?? []).reduce(0) { $0 + ($1.active_requests ?? 0) }
            activeHistory.append(TimePoint(at: now, value: Double(local + peers)))
            activeHistory.removeAll { now.timeIntervalSince($0.at) > 60 }
        }
        publish()
    }

    func pollHosts(_ gen: Int? = nil) async {
        let gen = gen ?? generation
        guard let access = runningEngine(gen) else { return }
        let hosts = try? await fetcher.get(access.baseURL, "/internal/hosts", key: access.adminKey,
                                           timeout: hostsTimeout, as: NotchHostsReply.self)
        guard current(gen) else { return }
        if let hosts {
            let now = Date()
            hostsReply = hosts
            hostsAt = now
            record(hosts, at: now)
        }
        publish()
    }

    /// The last hosts reply while it is within `staleAfter`; a stale reply is not shown as data.
    private func freshHosts(at now: Date) -> NotchHostsReply? {
        hostsAt.map { now.timeIntervalSince($0) <= staleAfter } == true ? hostsReply : nil
    }

    private func record(_ reply: NotchHostsReply, at now: Date) {
        let hosts = NotchSnapshot.uniqueHosts(reply).filter { $0.reachable != false }
        usage.observe(hosts.compactMap { h in h.usage?.counters.map { (h.host, $0) } }, at: now, calendar: calendar)
        for h in hosts {
            for a in NotchSnapshot.physicalAccelerators(h.hardware?.accelerators ?? []) where a.power != nil {
                let device = a.name ?? a.node ?? a.device_id ?? "accelerator"
                energy.observe(sensor: "\(h.host)/\(a.uuid ?? device)", watts: a.power?.watts, at: now, calendar: calendar)
            }
        }
        Self.save(usage, "notch.usageLedger", defaults)
        Self.save(energy, "notch.energyLedger", defaults)
    }

    // MARK: oMLX

    /// A local oMLX server from its own settings file. Only a loopback host is used, and the key
    /// stays in memory for the request header.
    struct OMLXEndpoint: Sendable, Equatable {
        var baseURL: String
        var key: String?

        static func load(_ path: String = NSHomeDirectory() + "/.omlx/settings.json") -> OMLXEndpoint? {
            guard let data = FileManager.default.contents(atPath: path),
                  let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let server = json["server"] as? [String: Any] else { return nil }
            let host = (server["host"] as? String) ?? "127.0.0.1"
            // A server bound to every interface is still reached over loopback; other hosts are not used.
            let loopback = ["127.0.0.1": "127.0.0.1", "localhost": "127.0.0.1", "0.0.0.0": "127.0.0.1", "::1": "::1", "::": "::1"]
            guard let local = loopback[host], let port = server["port"] as? Int else { return nil }
            let auth = json["auth"] as? [String: Any]
            let skip = auth?["skip_api_key_verification"] as? Bool ?? false
            let key = skip ? nil : (auth?["api_key"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            let h = local == "::1" ? "[::1]" : local
            return OMLXEndpoint(baseURL: "http://\(h):\(port)", key: key)
        }
    }

    private func omlxLoop(_ gen: Int) async {
        while current(gen) {
            await pollOMLX(gen)
            try? await Task.sleep(for: omlxInterval)
        }
    }

    func pollOMLX(_ gen: Int? = nil) async {
        let gen = gen ?? generation
        let endpoint = omlxEndpoint()
        let status: OMLXStatus?
        let activity: OMLXActivity?
        if let endpoint {
            async let s = try? fetcher.get(endpoint.baseURL, "/api/status", key: endpoint.key, timeout: omlxTimeout, as: OMLXStatus.self)
            async let a = try? fetcher.get(endpoint.baseURL, "/admin/api/activity", key: endpoint.key, timeout: omlxTimeout, as: OMLXActivity.self)
            (status, activity) = await (s, a)
        } else {
            (status, activity) = (nil, nil)
        }
        guard current(gen) else { return }
        let now = Date()
        guard let status, let uptime = status.uptime_seconds else {
            if omlxStatus != nil { omlxLedger.observe([], at: now, calendar: calendar) }
            omlxStatus = nil; omlxActivity = nil; omlxAt = nil
            publish()
            return
        }
        omlxStatus = status
        omlxActivity = activity
        omlxAt = now
        // The process start, derived from uptime; jitter under 30 s is the same process.
        let start = now.timeIntervalSince1970 - uptime
        if omlxStart.map({ abs($0 - start) > 30 }) ?? true { omlxStart = start }
        let counters = UsageLedger.Counters(since: omlxStart ?? start, output: status.total_completion_tokens ?? 0,
                                            prompt: status.total_prompt_tokens ?? 0, cached: status.total_cached_tokens ?? 0)
        omlxLedger.observe([("omlx", counters)], at: now, calendar: calendar)
        Self.save(omlxLedger, "notch.omlxLedger", defaults)
        publish()
    }

    // MARK: Publish

    private func publish(at now: Date = Date()) {
        engineFreshness = Self.freshness(of: hostsAt ?? metricsAt, offline: engineOffline ?? "Waiting for the first report",
                                         isOffline: engineOffline != nil, staleAfter: staleAfter, at: now)
        omlxFreshness = Self.freshness(of: omlxAt, offline: "No local oMLX server", isOffline: omlxAt == nil,
                                       staleAfter: staleAfter, at: now)
        // A cached reply older than `staleAfter` is not passed on as current data.
        let metrics = metricsAt.map { now.timeIntervalSince($0) <= staleAfter } == true ? metricsReply : nil
        snapshot = NotchSnapshot.derive(hosts: freshHosts(at: now), metrics: metrics, usage: usage, energy: energy,
                                        omlx: omlxStatus, omlxActivity: omlxActivity, omlxLedger: omlxLedger,
                                        at: now, calendar: calendar)
        freshness = snapshot.productionSource == NotchSnapshot.omlxSourceName ? omlxFreshness : engineFreshness
    }

    static func freshness(of at: Date?, offline: String, isOffline: Bool, staleAfter: TimeInterval, at now: Date) -> Freshness {
        if isOffline { return .offline(offline) }
        guard let at else { return .offline(offline) }
        return now.timeIntervalSince(at) > staleAfter ? .stale(since: at) : .live
    }

    private static func load<T: Decodable>(_ type: T.Type, _ key: String, _ defaults: UserDefaults) -> T? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(T.self, from: $0) }
    }

    private static func save<T: Encodable>(_ value: T, _ key: String, _ defaults: UserDefaults) {
        if let data = try? JSONEncoder().encode(value) { defaults.set(data, forKey: key) }
    }
}
