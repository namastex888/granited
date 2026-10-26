import Foundation

// MARK: - Engine reply (GET /internal/activity)

/// Decodes an array element by element. An element that fails to decode is skipped and counted
/// in `dropped`, so one malformed entry never blanks the reply and is never silently lost.
struct LossyArray<Element: Decodable & Sendable>: Decodable, Sendable {
    var items: [Element]
    var dropped = 0

    init(from decoder: Decoder) throws {
        var c = try decoder.unkeyedContainer()
        var out: [Element] = []
        var dropped = 0
        while !c.isAtEnd {
            if let e = try? c.decode(Element.self) {
                out.append(e)
            } else {
                _ = try? c.decode(Skip.self)
                dropped += 1
            }
        }
        items = out
        self.dropped = dropped
    }

    private struct Skip: Decodable { init(from decoder: Decoder) throws {} }
}

private struct AnyKey: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }
    init(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}

/// Per-node job counts in 15-minute UTC buckets; a host from an older engine carries
/// `activity_error` instead. Decoding is lenient but never silent: whatever can't be read is
/// counted, so the summary can mark that host and the day as partial.
struct ActivityReply: Decodable, Sendable, Equatable {
    struct Interval: Decodable, Sendable, Equatable {
        var start_unix_ms: Int64
        var end_unix_ms: Int64

        init(start_unix_ms: Int64, end_unix_ms: Int64) {
            self.start_unix_ms = start_unix_ms
            self.end_unix_ms = end_unix_ms
        }

        enum CodingKeys: String, CodingKey { case start_unix_ms, end_unix_ms }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            start_unix_ms = try c.decode(Int64.self, forKey: .start_unix_ms)
            end_unix_ms = try c.decode(Int64.self, forKey: .end_unix_ms)
            guard start_unix_ms >= 0, end_unix_ms >= start_unix_ms else {
                throw DecodingError.dataCorruptedError(forKey: .end_unix_ms, in: c, debugDescription: "invalid coverage interval")
            }
        }
    }

    struct Bucket: Decodable, Sendable, Equatable {
        var start_unix_ms: Int64
        var model: String?
        var capability: String
        var succeeded: Int64
        /// Server-side failures only (5xx, 408, 429, broken stream, backend timeout or unreachable).
        var failed: Int64
        /// The worker refused the request's input (a 4xx); engines before rejected was counted send none.
        var rejected: Int64
        var cancelled: Int64
        /// Unit sums from parsed success responses.
        var units: [String: Double]
        /// Succeeded jobs whose response could not be measured (no units counted for them).
        var units_unavailable: Int64
        /// Unit names whose value could not be read; those units are unavailable, never 0.
        var unreadableUnits: [String] = []

        enum CodingKeys: String, CodingKey {
            case start_unix_ms, model, capability, succeeded, failed, rejected, cancelled, units, units_unavailable
        }

        init(start_unix_ms: Int64, model: String? = nil, capability: String, succeeded: Int64 = 0, failed: Int64 = 0,
             rejected: Int64 = 0, cancelled: Int64 = 0, units: [String: Double] = [:], units_unavailable: Int64 = 0) {
            self.start_unix_ms = start_unix_ms; self.model = model; self.capability = capability
            self.succeeded = succeeded; self.failed = failed; self.rejected = rejected; self.cancelled = cancelled
            self.units = units; self.units_unavailable = units_unavailable
        }

        /// A missing or invalid counter makes the bucket unreadable (counted as dropped), so it
        /// can never read as 0 jobs.
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            start_unix_ms = try c.decode(Int64.self, forKey: .start_unix_ms)
            capability = try c.decode(String.self, forKey: .capability)
            model = try? c.decode(String.self, forKey: .model)
            func counter(_ key: CodingKeys) throws -> Int64 {
                let v = try c.decode(Int64.self, forKey: key)
                guard v >= 0 else {
                    throw DecodingError.dataCorruptedError(forKey: key, in: c, debugDescription: "negative counter")
                }
                return v
            }
            succeeded = try counter(.succeeded)
            failed = try counter(.failed)
            // Absent from older engines (their 4xx replies were counted as failed); present but invalid is unreadable.
            rejected = c.contains(.rejected) ? try counter(.rejected) : 0
            cancelled = try counter(.cancelled)
            units_unavailable = try counter(.units_unavailable)
            var units: [String: Double] = [:]
            var unreadable: [String] = []
            if c.contains(.units) {
                let u = try c.nestedContainer(keyedBy: AnyKey.self, forKey: .units)
                for key in u.allKeys {
                    if let v = try? u.decode(Double.self, forKey: key), v.isFinite, v >= 0, v < Double(Int64.max) {
                        units[key.stringValue] = v
                    } else {
                        unreadable.append(key.stringValue)
                    }
                }
            }
            self.units = units
            unreadableUnits = unreadable.sorted()
        }
    }

    struct Host: Decodable, Sendable, Equatable {
        var host: String
        var coverage: [Interval]
        var buckets: [Bucket]
        var truncated: Bool
        var activity_error: String?
        /// Coverage intervals and buckets that could not be read.
        var droppedRecords = 0
        /// The `buckets` field itself was missing or not a list.
        var bucketsUnreadable = false
        var metadataUnreadable = false

        enum CodingKeys: String, CodingKey { case host, coverage, buckets, truncated, activity_error }

        init(host: String, coverage: [Interval] = [], buckets: [Bucket] = [], truncated: Bool = false,
             activity_error: String? = nil) {
            self.host = host; self.coverage = coverage; self.buckets = buckets
            self.truncated = truncated; self.activity_error = activity_error
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            host = try c.decode(String.self, forKey: .host)
            activity_error = try? c.decode(String.self, forKey: .activity_error)
            truncated = (try? c.decode(Bool.self, forKey: .truncated)) ?? false
            metadataUnreadable = c.contains(.truncated) && (try? c.decode(Bool.self, forKey: .truncated)) == nil
            let coverage = try? c.decode(LossyArray<Interval>.self, forKey: .coverage)
            let buckets = try? c.decode(LossyArray<Bucket>.self, forKey: .buckets)
            self.coverage = coverage?.items ?? []
            self.buckets = buckets?.items ?? []
            droppedRecords = (coverage?.dropped ?? 0) + (buckets?.dropped ?? 0)
            metadataUnreadable = metadataUnreadable || (activity_error == nil && coverage == nil)
            bucketsUnreadable = activity_error == nil && buckets == nil
        }
    }

    var bucket_seconds: Int64?
    var since_unix_ms: Int64?
    var until_unix_ms: Int64?
    var hosts: [Host]
    /// Host entries that could not be read (no usable host name).
    var droppedHosts = 0

    enum CodingKeys: String, CodingKey { case bucket_seconds, since_unix_ms, until_unix_ms, hosts }

    init(since_unix_ms: Int64? = nil, until_unix_ms: Int64? = nil, hosts: [Host]) {
        bucket_seconds = 900; self.since_unix_ms = since_unix_ms; self.until_unix_ms = until_unix_ms; self.hosts = hosts
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        bucket_seconds = try? c.decode(Int64.self, forKey: .bucket_seconds)
        since_unix_ms = try? c.decode(Int64.self, forKey: .since_unix_ms)
        until_unix_ms = try? c.decode(Int64.self, forKey: .until_unix_ms)
        let hosts = try c.decode(LossyArray<Host>.self, forKey: .hosts)
        self.hosts = hosts.items
        droppedHosts = hosts.dropped
    }
}

// MARK: - Capabilities

/// The 13 capability families of `contracts::capability` (src/contracts.rs), in that order, with
/// the unit names each family's success responses carry. Families without units count jobs only.
enum ActivityCatalog {
    struct Family: Sendable {
        let id: String
        let title: String
        /// The first unit is the family's key unit (the one Today shows).
        let units: [String]
        /// Units shown only when every measured job reported them (never as 0 when absent).
        var optional: [String] = []
    }

    static let families: [Family] = [
        Family(id: "chat", title: "Chat", units: ["output_tokens", "prompt_tokens"], optional: ["reasoning_tokens"]),
        Family(id: "adapters", title: "Decisions", units: ["questions_answered", "input_tokens"]),
        Family(id: "embeddings", title: "Embeddings", units: ["vectors", "prompt_tokens"]),
        Family(id: "forecast", title: "Forecast", units: ["forecast_steps"]),
        Family(id: "anomalies", title: "Anomalies", units: []),
        Family(id: "rerank", title: "Rerank", units: []),
        Family(id: "retrieval", title: "Retrieval", units: ["documents_ranked"]),
        Family(id: "documents", title: "Documents", units: []),
        Family(id: "geospatial", title: "Geospatial", units: []),
        Family(id: "guardian", title: "Guardian", units: []),
        Family(id: "vision", title: "Vision", units: ["output_tokens", "prompt_tokens"], optional: ["reasoning_tokens"]),
        Family(id: "transcription", title: "Transcription", units: ["audio_seconds"]),
        Family(id: "speech_synthesis", title: "Speech synthesis", units: ["audio_seconds"]),
    ]

    static func index(of id: String) -> Int { families.firstIndex { $0.id == id } ?? families.count }
    static func family(_ id: String) -> Family? { families.first { $0.id == id } }
    static func title(_ id: String) -> String {
        family(id)?.title ?? id.replacingOccurrences(of: "_", with: " ").capitalizedFirst
    }

    static func unitLabel(_ name: String) -> String {
        switch name {
        case "output_tokens": "output tokens"
        case "prompt_tokens": "prompt tokens"
        case "reasoning_tokens": "reasoning tokens"
        case "input_tokens": "input tokens"
        case "questions_answered": "decisions made"
        case "vectors": "vectors"
        case "documents_ranked": "documents ranked"
        case "forecast_steps": "forecast steps"
        case "audio_seconds": "of audio"
        default: name.replacingOccurrences(of: "_", with: " ")
        }
    }

    /// The unit as a noun, for "… unavailable".
    static func unitNoun(_ name: String) -> String { name == "audio_seconds" ? "audio length" : unitLabel(name) }

    /// Capabilities with a deployment that a node reports: this node's /internal/metrics workers and
    /// each reachable node's `managed_workers` from /internal/hosts. Configuration and the registry
    /// alone never make a family installed; the registry (GET /admin/models) only maps a reported
    /// model id to its capabilities. nil only when models are reported but the registry needed to
    /// map them is not loaded yet.
    static func installed(deployed: some Collection<String>, registry: [String: [String]]) -> Set<String>? {
        if deployed.isEmpty { return [] }
        guard !registry.isEmpty else { return nil }
        return Set(deployed.flatMap { registry[$0] ?? [] })
    }
}

// MARK: - Local day

/// The user's local day so far as a range of whole 15-minute UTC buckets. Every real UTC offset
/// is a multiple of 15 minutes, so local midnight is always a bucket boundary and the day is an
/// exact union of buckets (the last one, holding `until`, is still filling).
struct LocalDay: Sendable, Equatable {
    static let bucketMs: Int64 = 900_000

    var since: Date
    var until: Date
    /// Buckets in the whole local day: 96, or 92 / 100 on a DST change.
    var bucketsInDay: Int

    init(now: Date, calendar: Calendar) {
        since = calendar.startOfDay(for: now)
        until = now
        let next = calendar.date(byAdding: .day, value: 1, to: since) ?? since.addingTimeInterval(86_400)
        bucketsInDay = Int((Self.ms(next) - Self.ms(since)) / Self.bucketMs)
    }

    /// The whole local day that contains `date` (until the next local midnight).
    static func whole(containing date: Date, calendar: Calendar) -> LocalDay {
        var day = LocalDay(now: date, calendar: calendar)
        day.until = calendar.date(byAdding: .day, value: 1, to: day.since) ?? day.since.addingTimeInterval(86_400)
        return day
    }

    static func ms(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1_000).rounded()) }

    var sinceMs: Int64 { Self.ms(since) }
    var untilMs: Int64 { Self.ms(until) }
    var isBucketAligned: Bool { sinceMs % Self.bucketMs == 0 }
    /// Start of each bucket from local midnight up to the one holding `until`.
    var bucketStarts: [Int64] { Array(stride(from: sinceMs, to: untilMs, by: Int(Self.bucketMs))) }
    func contains(bucket start: Int64) -> Bool { start >= sinceMs && start < untilMs }

    var query: String { "/internal/activity?since_unix_ms=\(sinceMs)&until_unix_ms=\(untilMs)" }
}

// MARK: - Summary (pure derivation, testable)

struct UnitValue: Sendable, Equatable, Identifiable {
    var name: String
    /// nil: the jobs ran but their responses carried no measurable units.
    var value: Double?

    var id: String { name }
}

struct CapabilityRow: Sendable, Equatable, Identifiable {
    var id: String
    var title: String
    var succeeded: Int64 = 0
    var failed: Int64 = 0
    var rejected: Int64 = 0
    var cancelled: Int64 = 0
    var units: [UnitValue] = []
    var unitsUnavailable: Int64 = 0
    /// nil: unknown (reported models can't be mapped yet).
    var installed: Bool?

    var used: Bool { succeeded > 0 || failed > 0 || rejected > 0 || cancelled > 0 }
    var idleStatus: String { installed == false ? "not installed" : "no activity today" }
    func unit(_ name: String) -> UnitValue? { units.first { $0.name == name } }
    /// The family's key unit, when it has one and jobs succeeded.
    var keyUnit: UnitValue? { ActivityCatalog.family(id)?.units.first.flatMap(unit) }
}

struct HostCoverage: Sendable, Equatable, Identifiable {
    var host: String
    /// Earliest coverage today (clamped to local midnight).
    var since: Date?
    /// Breaks between coverage intervals today (the engine was down or restarted).
    var gaps = 0
    /// Set when the node's jobs are not in the totals (older engine, not in the report, not readable).
    var notReporting: String?
    /// Set when the node's jobs are in the totals but incomplete (records unreadable, reply truncated).
    var incomplete: String?

    var id: String { host }
    var reporting: Bool { notReporting == nil }
}

struct ActivityBucket: Sendable, Equatable {
    var start: Date
    /// nil: no node reported coverage for this bucket (unknown, not zero).
    var jobs: Int64?
}

/// Today's completed jobs across the fleet: counted once on the node that ran each job, summed
/// over nodes, never estimated. Failed, rejected and cancelled jobs are separate counts, never jobs.
struct ActivitySummary: Sendable, Equatable {
    static let pending = "Engine update pending: no job activity reported yet"
    static let olderEngine = "not reporting jobs (older engine)"
    static let notReadable = "not readable"
    static let unreadableRecords = "some activity records couldn't be read"

    var jobsToday: Reading<Int64> = .collecting("Waiting for the first activity report")
    var rows: [CapabilityRow] = ActivitySummary.ordered(ActivityCatalog.families.map {
        CapabilityRow(id: $0.id, title: $0.title)
    })
    var coverage: [HostCoverage] = []
    var series: [ActivityBucket] = []
    var dayStart: Date?
    var until: Date?
    var bucketsInDay = 96
    /// Why the totals may be incomplete; empty when every node's report was read in full.
    var partialReasons: [String] = []
    /// The fleet total rose since the previous poll, which was at most 60 s ago.
    var recentJob = false

    var partial: Bool { !partialReasons.isEmpty }
    var usedRows: [CapabilityRow] { rows.filter(\.used) }
    var otherRows: [CapabilityRow] { rows.filter { !$0.used } }

    /// Used families by jobs completed, then installed or unknown families, then families that are
    /// not installed; catalog order within each group.
    static func ordered(_ rows: [CapabilityRow]) -> [CapabilityRow] {
        func group(_ r: CapabilityRow) -> Int { r.used ? 0 : r.installed == false ? 2 : 1 }
        return rows.sorted { a, b in
            if group(a) != group(b) { return group(a) < group(b) }
            if group(a) == 0, a.succeeded != b.succeeded { return a.succeeded > b.succeeded }
            return ActivityCatalog.index(of: a.id) < ActivityCatalog.index(of: b.id)
        }
    }

    /// `knownHosts`: nodes from /internal/hosts; one missing from the reply is not reporting.
    /// `error`: why there is no reply (an engine without the endpoint).
    static func derive(_ reply: ActivityReply?, day: LocalDay, installed: Set<String>?,
                       knownHosts: [String] = [], error: String? = nil) -> ActivitySummary {
        var s = ActivitySummary()
        s.bucketsInDay = day.bucketsInDay
        s.dayStart = day.since
        s.until = day.until
        var rows: [String: CapabilityRow] = [:]
        for f in ActivityCatalog.families { rows[f.id] = CapabilityRow(id: f.id, title: f.title) }
        guard let reply else {
            s.jobsToday = error.map { .unavailable($0) } ?? .collecting("Waiting for the first activity report")
            s.rows = finish(rows, installed: installed, sums: [:], missing: [:])
            return s
        }

        var sums: [String: [String: Double]] = [:]
        // Per capability, unit names that some measured bucket did not report readably.
        var missing: [String: Set<String>] = [:]
        var perBucket: [Int64: Int64] = [:]
        var covered: [(Int64, Int64)] = []
        var seen = Set<String>()
        var unknown = Set<String>()
        var reporting = 0
        var total: Int64 = 0
        var overflowed = false
        func add(_ a: Int64, _ b: Int64) -> Int64 {
            let result = a.addingReportingOverflow(b)
            if result.overflow { overflowed = true; return Int64.max }
            return result.partialValue
        }

        for h in reply.hosts where seen.insert(h.host).inserted {
            if h.activity_error != nil {
                s.coverage.append(HostCoverage(host: h.host, notReporting: olderEngine))
                continue
            }
            // Nothing readable: "not readable", never "0 jobs".
            if h.bucketsUnreadable || (h.droppedRecords > 0 && h.buckets.isEmpty) {
                s.coverage.append(HostCoverage(host: h.host, notReporting: notReadable))
                continue
            }
            reporting += 1
            let intervals = merge(h.coverage.map { (max($0.start_unix_ms, day.sinceMs), min($0.end_unix_ms, day.untilMs)) })
            covered += intervals
            var incomplete: [String] = []
            if h.metadataUnreadable || h.droppedRecords > 0 || h.buckets.contains(where: { bucket in
                !bucket.unreadableUnits.isEmpty || (bucket.succeeded > bucket.units_unavailable &&
                    (ActivityCatalog.family(bucket.capability)?.units ?? []).contains(where: { name in bucket.units[name] == nil }))
            }) {
                incomplete.append(unreadableRecords)
            }
            if h.truncated { incomplete.append("reply truncated, counts incomplete") }
            s.coverage.append(HostCoverage(host: h.host, since: intervals.first.map { Date(timeIntervalSince1970: Double($0.0) / 1_000) },
                                           gaps: max(0, intervals.count - 1),
                                           incomplete: incomplete.isEmpty ? nil : incomplete.joined(separator: "; ")))
            for b in h.buckets where day.contains(bucket: b.start_unix_ms) {
                if ActivityCatalog.family(b.capability) == nil { unknown.insert(b.capability) }
                var row = rows[b.capability] ?? CapabilityRow(id: b.capability, title: ActivityCatalog.title(b.capability))
                row.succeeded = add(row.succeeded, b.succeeded)
                row.failed = add(row.failed, b.failed)
                row.rejected = add(row.rejected, b.rejected)
                row.cancelled = add(row.cancelled, b.cancelled)
                row.unitsUnavailable = add(row.unitsUnavailable, b.units_unavailable)
                rows[b.capability] = row
                total = add(total, b.succeeded)
                perBucket[b.start_unix_ms] = add(perBucket[b.start_unix_ms, default: 0], b.succeeded)
                for (name, v) in b.units {
                    let sum = sums[b.capability, default: [:]][name, default: 0] + v
                    if sum.isFinite && sum < Double(Int64.max) {
                        sums[b.capability, default: [:]][name] = sum
                    } else {
                        missing[b.capability, default: []].insert(name)
                        overflowed = true
                    }
                }
                missing[b.capability, default: []].formUnion(b.unreadableUnits)
                if b.succeeded > b.units_unavailable {
                    let f = ActivityCatalog.family(b.capability)
                    let expected = (f?.units ?? []) + (f?.optional ?? [])
                    missing[b.capability, default: []].formUnion(expected.filter { b.units[$0] == nil })
                }
            }
        }
        for host in knownHosts where !seen.contains(host) {
            s.coverage.append(HostCoverage(host: host, notReporting: "not in the activity report"))
        }

        let absent = s.coverage.filter { !$0.reporting }
        if !absent.isEmpty {
            s.partialReasons.append(absent.map { "\($0.host) \($0.notReporting ?? "")" }.joined(separator: ", "))
        }
        for c in s.coverage { if let note = c.incomplete { s.partialReasons.append("\(c.host): \(note)") } }
        if reply.droppedHosts > 0 {
            s.partialReasons.append(reply.droppedHosts == 1 ? "a node's activity record couldn't be read"
                                                            : "\(reply.droppedHosts) nodes' activity records couldn't be read")
        }
        if !unknown.isEmpty {
            s.partialReasons.append("capability not recognised by this app: " + unknown.sorted().joined(separator: ", "))
        }
        if overflowed { s.partialReasons.append("Activity exceeds the display range") }
        s.jobsToday = reporting > 0 ? .value(total) : .unavailable("No node's job activity could be read")
        s.rows = finish(rows, installed: installed, sums: sums, missing: missing)
        s.series = day.bucketStarts.map { start in
            let end = start + LocalDay.bucketMs
            let isCovered = covered.contains { $0.0 < end && $0.1 > start } || perBucket[start] != nil
            return ActivityBucket(start: Date(timeIntervalSince1970: Double(start) / 1_000),
                                  jobs: isCovered ? perBucket[start] ?? 0 : nil)
        }
        return s
    }

    /// Units per row: the family's own units first, then optional units every measured job
    /// reported, then any other names the engine sent. A unit some measured bucket did not report
    /// is unavailable, never zero or a partial sum (an optional unit is then left out); jobs
    /// counted in `units_unavailable` are excluded from the sums and named on the row.
    private static func finish(_ rows: [String: CapabilityRow], installed: Set<String>?,
                               sums: [String: [String: Double]], missing: [String: Set<String>]) -> [CapabilityRow] {
        ordered(rows.values.map { row in
            var r = row
            r.installed = r.used ? true : installed.map { $0.contains(r.id) }
            guard r.succeeded > 0 else { return r }
            let family = ActivityCatalog.family(r.id)
            let expected = family?.units ?? []
            let optional = family?.optional ?? []
            let lost = missing[r.id] ?? []
            let measured = r.succeeded > r.unitsUnavailable
            func value(_ name: String) -> Double? { measured && !lost.contains(name) ? sums[r.id]?[name] ?? 0 : nil }
            let shownOptional = optional.filter { measured && !lost.contains($0) && sums[r.id]?[$0] != nil }
            let extra = Set((sums[r.id] ?? [:]).keys).union(lost)
                .subtracting(expected).subtracting(optional).sorted()
            r.units = expected.map { UnitValue(name: $0, value: value($0)) }
                + shownOptional.map { UnitValue(name: $0, value: sums[r.id]?[$0]) }
                + extra.map { UnitValue(name: $0, value: value($0)) }
            return r
        })
    }

    /// Sorted, non-empty, overlapping or touching intervals merged.
    static func merge(_ intervals: [(Int64, Int64)]) -> [(Int64, Int64)] {
        var out: [(Int64, Int64)] = []
        for iv in intervals.filter({ $0.0 < $0.1 }).sorted(by: { $0.0 < $1.0 }) {
            if let last = out.last, iv.0 <= last.1 { out[out.count - 1].1 = max(last.1, iv.1) } else { out.append(iv) }
        }
        return out
    }
}
