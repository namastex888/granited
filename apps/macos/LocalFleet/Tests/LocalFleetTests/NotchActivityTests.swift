import AppKit
import Foundation
import Testing
@testable import LocalFleet

private func calendar(_ tz: String) -> Calendar {
    var c = Calendar(identifier: .gregorian)
    c.timeZone = TimeZone(identifier: tz)!
    return c
}

private func noon(_ c: Calendar, _ y: Int, _ m: Int, _ d: Int) -> Date {
    c.date(from: DateComponents(year: y, month: m, day: d, hour: 12))!
}

/// One succeeded chat job in every 15-minute UTC bucket of [from, to).
private func everyBucket(_ from: Int64, _ to: Int64) -> [ActivityReply.Bucket] {
    stride(from: from, to: to, by: Int(LocalDay.bucketMs)).map {
        .init(start_unix_ms: $0, model: "granite-4.2-3b", capability: "chat", succeeded: 1,
              units: ["output_tokens": 10, "prompt_tokens": 20])
    }
}

private func decode(_ json: String) throws -> ActivityReply {
    try JSONDecoder().decode(ActivityReply.self, from: Data(json.utf8))
}

@Suite struct LocalDayTests {
    /// A local day is an exact union of 15-minute UTC buckets: midnight is a bucket boundary and the
    /// derivation counts exactly the buckets inside the day, none from either neighbour.
    @Test(arguments: [
        ("Asia/Kolkata", 2026, 10, 26, 96),       // UTC+5:30
        ("Asia/Kathmandu", 2026, 10, 26, 96),     // UTC+5:45
        ("America/St_Johns", 2026, 1, 15, 96),    // UTC−3:30
        ("Europe/Berlin", 2026, 3, 29, 92),       // DST starts: 23 h
        ("Europe/Berlin", 2026, 10, 25, 100),     // DST ends: 25 h
    ])
    func localDayIsAnExactUnionOfBuckets(tz: String, y: Int, m: Int, d: Int, buckets: Int) {
        let c = calendar(tz)
        let day = LocalDay.whole(containing: noon(c, y, m, d), calendar: c)
        #expect(day.isBucketAligned)
        #expect(day.bucketsInDay == buckets)
        #expect(day.bucketStarts.count == buckets)
        let eightHours: Int64 = 8 * 3_600_000
        let reply = ActivityReply(hosts: [.init(host: "mac",
                                                coverage: [.init(start_unix_ms: day.sinceMs - eightHours, end_unix_ms: day.untilMs + eightHours)],
                                                buckets: everyBucket(day.sinceMs - eightHours, day.untilMs + eightHours))])
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        #expect(s.jobsToday == .value(Int64(buckets)))
        #expect(s.series.count == buckets)
        #expect(s.series.allSatisfy { $0.jobs == 1 })
        #expect(!s.partial)
    }

    @Test func theQueryAsksFromLocalMidnightToNow() {
        let c = calendar("Asia/Kathmandu")
        let now = noon(c, 2026, 10, 26)
        let day = LocalDay(now: now, calendar: c)
        #expect(day.since == c.startOfDay(for: now))
        #expect(day.query == "/internal/activity?since_unix_ms=\(day.sinceMs)&until_unix_ms=\(LocalDay.ms(now))")
        // Midnight to noon: 48 buckets, the last one still filling.
        #expect(day.bucketStarts.count == 48)
    }
}

@Suite @MainActor struct ActivitySummaryTests {
    private let c = calendar("Europe/Lisbon")
    private var day: LocalDay { LocalDay(now: noon(c, 2026, 10, 26), calendar: c) }

    @Test func anOlderPeerIsPartialCoverageNeverZeroJobs() {
        let day = self.day
        let reply = ActivityReply(hosts: [
            .init(host: "mac", coverage: [.init(start_unix_ms: day.sinceMs, end_unix_ms: day.untilMs)],
                  buckets: [.init(start_unix_ms: day.sinceMs, capability: "chat", succeeded: 4)]),
            .init(host: "hx470", activity_error: "404 from peer"),
        ])
        let s = ActivitySummary.derive(reply, day: day, installed: nil, knownHosts: ["mac", "hx470", "ryzen"])
        #expect(s.jobsToday == .value(4))
        #expect(s.partial)
        #expect(s.coverage.first { $0.host == "hx470" }?.notReporting == ActivitySummary.olderEngine)
        #expect(s.coverage.first { $0.host == "ryzen" }?.notReporting == "not in the activity report")
        #expect(DiagnosticsPresentation.coverageLine(s.coverage[1]) == "hx470 not reporting jobs (older engine)")
        #expect(TodayPresentation(s, calendar: c).footer?.hasPrefix("Partial · hx470 not reporting jobs") == true)

        // Only older engines: no job count at all, never a clean 0.
        let older = ActivitySummary.derive(ActivityReply(hosts: [.init(host: "hx470", activity_error: "unsupported")]),
                                           day: day, installed: nil)
        #expect(older.jobsToday.value == nil)
        #expect(older.partial)
        #expect(TodayPresentation(older, calendar: c).headline == "—")
    }

    @Test func coverageReportsTheStartAndTheGapsToday() {
        let day = self.day
        let hour: Int64 = 3_600_000
        let reply = ActivityReply(hosts: [.init(host: "mac", coverage: [
            .init(start_unix_ms: day.sinceMs - 5 * hour, end_unix_ms: day.sinceMs + 2 * hour),   // from yesterday
            .init(start_unix_ms: day.sinceMs + 3 * hour, end_unix_ms: day.sinceMs + 4 * hour),
            .init(start_unix_ms: day.sinceMs + 4 * hour, end_unix_ms: day.untilMs),              // touching: no gap
        ])])
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        #expect(s.coverage.first?.since == day.since)
        #expect(s.coverage.first?.gaps == 1)
        #expect(!s.partial)
        #expect(TodayPresentation(s, calendar: c).footer == "Covered since \(NotchFormat.time(day.since)) on 1 node · 1 gap")
        // Buckets in the hour without coverage are unknown, not zero.
        #expect(s.series.filter { $0.jobs == nil }.count == 4)
        #expect(s.series.filter { $0.jobs == 0 }.count == s.series.count - 4)
    }

    @Test func unavailableUnitsAreShownAsUnavailableNotZero() {
        let day = self.day
        let reply = ActivityReply(hosts: [.init(host: "mac", buckets: [
            .init(start_unix_ms: day.sinceMs, capability: "transcription", succeeded: 3, units_unavailable: 3),
            .init(start_unix_ms: day.sinceMs, capability: "chat", succeeded: 2, units: ["output_tokens": 400]),
            .init(start_unix_ms: day.sinceMs, capability: "embeddings", succeeded: 5,
                  units: ["vectors": 30, "prompt_tokens": 900], units_unavailable: 2),
        ])])
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        let today = TodayPresentation(s, calendar: c)
        #expect(today.breakdown.first { $0.id == "transcription" }?.detail == "audio length unavailable")
        #expect(today.breakdown.first { $0.id == "embeddings" }?.detail == "30 vectors · partial")
        let chat = try! #require(s.rows.first { $0.id == "chat" })
        #expect(chat.unit("output_tokens")?.value == 400)
        #expect(chat.unit("prompt_tokens")?.value == nil)
        var snap = NotchSnapshot()
        snap.activity = s
        #expect(DiagnosticsPresentation(snap).chatTokens == ["400 output tokens", "prompt tokens unavailable"])
    }

    @Test func reasoningIsShownOnlyWhenReportedAndNeverAddedToOutput() {
        let day = self.day
        func chat(_ units: [String: Double]) -> DiagnosticsPresentation {
            var snap = NotchSnapshot()
            snap.activity = ActivitySummary.derive(ActivityReply(hosts: [.init(host: "mac", buckets: [
                .init(start_unix_ms: day.sinceMs, capability: "chat", succeeded: 2, units: units),
            ])]), day: day, installed: nil)
            return snap.activity.rows.first { $0.id == "chat" }.map { _ in DiagnosticsPresentation(snap) }!
        }
        let with = chat(["output_tokens": 5_000, "prompt_tokens": 9_000, "reasoning_tokens": 1_200])
        #expect(with.chatTokens == ["5k output tokens", "of which reasoning 1.2k", "9k prompt tokens"])
        let without = chat(["output_tokens": 5_000, "prompt_tokens": 9_000])
        #expect(without.chatTokens == ["5k output tokens", "9k prompt tokens"])
        #expect(!without.chatTokens.contains { $0.contains("reasoning") })
    }

    @Test func failedAndCancelledAreSeparateFromJobs() {
        let day = self.day
        let reply = ActivityReply(hosts: [.init(host: "mac", buckets: [
            .init(start_unix_ms: day.sinceMs, capability: "chat", succeeded: 3, failed: 2, cancelled: 1),
            .init(start_unix_ms: day.sinceMs, capability: "guardian", failed: 1),
        ])])
        var snap = NotchSnapshot()
        snap.activity = ActivitySummary.derive(reply, day: day, installed: nil)
        #expect(snap.activity.jobsToday == .value(3))
        let today = TodayPresentation(snap.activity, calendar: c)
        #expect(today.headline == "3")
        #expect(today.breakdown.map(\.id) == ["chat"])
        #expect(today.breakdown.first?.value == "3 jobs")
        #expect(today.all.first { $0.id == "guardian" }?.value == "no completed jobs")
        #expect(DiagnosticsPresentation(snap).failures == ["Chat: 2 failed, 1 cancelled", "Guardian: 1 failed"])
    }

    @Test func usedCapabilitiesComeFirstThenIdleThenNotInstalled() {
        let day = self.day
        let reply = ActivityReply(hosts: [.init(host: "mac", buckets: [
            .init(start_unix_ms: day.sinceMs, capability: "chat", succeeded: 10),
            .init(start_unix_ms: day.sinceMs, capability: "embeddings", succeeded: 30),
            .init(start_unix_ms: day.sinceMs, capability: "guardian", failed: 1),
        ])])
        let s = ActivitySummary.derive(reply, day: day, installed: ["chat", "embeddings", "forecast", "transcription"])
        #expect(s.usedRows.map(\.id) == ["embeddings", "chat", "guardian"])
        #expect(s.otherRows.prefix(2).map(\.id) == ["forecast", "transcription"])
        #expect(s.otherRows.dropFirst(2).allSatisfy { $0.installed == false && $0.idleStatus == "not installed" })
    }

    @Test func allThirteenFamiliesAreListed() {
        let ids = ["chat", "adapters", "embeddings", "retrieval", "forecast", "anomalies", "rerank", "documents",
                   "geospatial", "guardian", "vision", "transcription", "speech_synthesis"]
        #expect(ActivityCatalog.families.count == 13)
        #expect(Set(ActivityCatalog.families.map(\.id)) == Set(ids))
        let empty = ActivitySummary.derive(ActivityReply(hosts: [.init(host: "mac")]), day: day, installed: [])
        let all = TodayPresentation(empty, calendar: c).all
        #expect(Set(all.map(\.id)) == Set(ids))
        #expect(all.allSatisfy { $0.value == "not installed" })
        #expect(all.first { $0.id == "adapters" }?.title == "Decisions")
        #expect(ActivityCatalog.unitLabel("questions_answered") == "decisions made")
        // Before any reply the list is still complete.
        #expect(Set(TodayPresentation(ActivitySummary(), calendar: c).all.map(\.id)) == Set(ids))
    }

    @Test func installedComesOnlyFromReportedDeployments() {
        let registry = ["granite-4.2-3b": ["chat"], "granite-embedding-97m-multilingual-r2": ["embeddings", "retrieval"]]
        // The registry alone installs nothing.
        #expect(ActivityCatalog.installed(deployed: [String](), registry: registry) == [])
        #expect(ActivityCatalog.installed(deployed: ["granite-embedding-97m-multilingual-r2"], registry: registry) == ["embeddings", "retrieval"])
        // Reported models that can't be mapped yet: unknown, not "not installed".
        #expect(ActivityCatalog.installed(deployed: ["granite-4.2-3b"], registry: [:]) == nil)
        let s = ActivitySummary.derive(ActivityReply(hosts: [.init(host: "mac")]), day: day,
                                       installed: ActivityCatalog.installed(deployed: [String](), registry: registry))
        #expect(s.rows.first { $0.id == "chat" }?.idleStatus == "not installed")
    }

    @Test func todayHasOneHeadlineFigureAndNoEstimate() {
        let day = self.day
        var snap = NotchSnapshot()
        snap.activity = ActivitySummary.derive(ActivityReply(hosts: [.init(host: "mac",
            coverage: [.init(start_unix_ms: day.sinceMs, end_unix_ms: day.untilMs)],
            buckets: [.init(start_unix_ms: day.sinceMs, capability: "chat", succeeded: 7, units: ["output_tokens": 900, "prompt_tokens": 100]),
                      .init(start_unix_ms: day.sinceMs, capability: "embeddings", succeeded: 3, units: ["vectors": 3, "prompt_tokens": 40])])]),
            day: day, installed: nil)
        snap.estimate = .value(TokenRange(low: 1_200_000, high: 1_500_000))
        snap.sources = [ProductionSource(id: "engine", name: NotchSnapshot.engineSourceName, scope: "", rate: .value(12),
                                         history: [], today: .value(900), prompt: .value(100), cached: .value(10), coverageStart: nil,
                                         gaps: 0, estimate: .value(TokenRange(low: 1_200_000, high: 1_500_000)), note: nil)]
        let texts = TodayPresentation(snap.activity, calendar: c).primaryTexts
        #expect(texts.filter { $0.contains("completed job") } == ["10 completed jobs"])
        #expect(!texts.contains { $0.localizedCaseInsensitiveContains("midnight") || $0.contains("est.") })
        #expect(!texts.contains { $0.contains("tok/s") || $0.contains("active") })
    }

    @Test func omlxStaysReachableInDiagnosticsWhileTheFleetReportsJobs() {
        let now = Date()
        var ledger = UsageLedger()
        for i in 0...20 {
            ledger.observe([("omlx", .init(since: 1, output: 1_000 + Int64(i) * 300, prompt: 500 + Int64(i) * 100, cached: Int64(i) * 40))],
                           at: now - Double(20 - i) * 2, calendar: c)
        }
        let status = OMLXStatus(uptime_seconds: 3_600, active_requests: 1, waiting_requests: 0,
                                total_prompt_tokens: 2_500, total_completion_tokens: 7_000, total_cached_tokens: 800,
                                model_memory_used: 9_000_000_000, model_memory_max: 32_000_000_000)
        let activity = OMLXActivity(active_models: .init(models: [
            .init(id: "Qwen3-8B-4bit", is_loading: false, active_requests: 1, waiting_requests: 0,
                  prefilling: [.init(processed: 1_200, total: 4_000, eta: 3)], generating: [])],
            memory_pressure: .init(pressure_level: "ok")))
        let hosts = try! JSONDecoder().decode(NotchHostsReply.self, from: Data(#"{"hosts":[{"host":"mac","reachable":true,"hardware":{"managed_workers":[]}}]}"#.utf8))
        var snap = NotchSnapshot.derive(hosts: hosts, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                        omlx: status, omlxActivity: activity, omlxLedger: ledger, at: now, calendar: c)
        let day = LocalDay(now: now, calendar: c)
        snap.activity = ActivitySummary.derive(ActivityReply(hosts: [.init(host: "mac",
            buckets: [.init(start_unix_ms: day.sinceMs, capability: "chat", succeeded: 12, units: ["output_tokens": 4_000, "prompt_tokens": 900])])]),
            day: day, installed: nil)
        let d = DiagnosticsPresentation(snap)
        #expect(d.sources.contains { $0.id == "omlx" })
        #expect(d.omlxModels.contains { $0.model == "Qwen3-8B-4bit" })
        #expect(d.omlxPressure == "ok")
        // Sources are listed separately, never summed into the jobs.
        #expect(snap.activity.jobsToday == .value(12))
    }
}

/// Lossy decoding is never silent: anything that can't be read marks the host and the day
/// partial or unavailable, and never reads as a clean 0.
@Suite @MainActor struct LossyActivityTests {
    private let c = calendar("Europe/Lisbon")
    private var day: LocalDay { LocalDay(now: noon(c, 2026, 10, 26), calendar: c) }

    private func host(_ buckets: String, name: String = "mac") -> String {
        let s = day.sinceMs
        return #"{"host":"\#(name)","coverage":[{"start_unix_ms":\#(s),"end_unix_ms":\#(s + 3_600_000)}],"buckets":[\#(buckets)]}"#
    }
    private func bucket(_ fields: String) -> String { #"{"start_unix_ms":\#(day.sinceMs),"model":"m",\#(fields)}"# }
    private let counters = #""failed":0,"cancelled":0,"units_unavailable":0"#

    @Test func aMalformedHostEntryMarksTheDayPartial() throws {
        let good = host(bucket(#""capability":"chat","succeeded":2,\#(counters)"#))
        let reply = try decode(#"{"hosts":[\#(good),{"nohost":1}]}"#)
        #expect(reply.droppedHosts == 1)
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        #expect(s.jobsToday == .value(2))
        #expect(s.partial)
        #expect(s.partialReasons.contains("a node's activity record couldn't be read"))
    }

    @Test func aMalformedBucketMarksTheHostIncomplete() throws {
        let buckets = [bucket(#""capability":"chat","succeeded":2,\#(counters)"#), bucket(#""succeeded":5,\#(counters)"#)]
        let reply = try decode(#"{"hosts":[\#(host(buckets.joined(separator: ",")))]}"#)
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        #expect(s.jobsToday == .value(2))
        #expect(s.coverage.first?.incomplete == ActivitySummary.unreadableRecords)
        #expect(s.partial)
    }

    @Test(arguments: [#""capability":"chat",\#("\"failed\":0,\"cancelled\":0,\"units_unavailable\":0")"#,       // succeeded missing
                      #""capability":"chat","succeeded":"many","failed":0,"cancelled":0,"units_unavailable":0"#,
                      #""capability":"chat","succeeded":-3,"failed":0,"cancelled":0,"units_unavailable":0"#])
    func aMissingOrInvalidCounterIsUnavailableNotZero(fields: String) throws {
        let reply = try decode(#"{"hosts":[\#(host(bucket(fields)))]}"#)
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        // The host's only bucket couldn't be read: "not readable", never 0 jobs.
        #expect(s.coverage.first?.notReporting == ActivitySummary.notReadable)
        #expect(s.jobsToday.value == nil)
        #expect(s.partial)
        #expect(TodayPresentation(s, calendar: c).headline == "—")
    }

    @Test func aNonNumericUnitIsUnavailable() throws {
        let fields = #""capability":"chat","succeeded":2,\#(counters),"units":{"output_tokens":"lots","prompt_tokens":40}"#
        let reply = try decode(#"{"hosts":[\#(host(bucket(fields)))]}"#)
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        let chat = try #require(s.rows.first { $0.id == "chat" })
        #expect(chat.unit("output_tokens") == UnitValue(name: "output_tokens", value: nil))
        #expect(chat.unit("prompt_tokens")?.value == 40)
        #expect(TodayPresentation(s, calendar: c).breakdown.first?.detail == "output tokens unavailable")
        #expect(s.partial)
    }


    @Test func malformedCoverageIsVisibleAndMissingUnitsAreUnavailable() throws {
        let b = bucket(#""capability":"embeddings","succeeded":2,\#(counters)"#)
        let reply = try decode(#"{"hosts":[{"host":"mac","coverage":"bad","buckets":[\#(b)]}]}"#)
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        #expect(s.jobsToday == .value(2))
        #expect(s.partial)
        #expect(s.rows.first { $0.id == "embeddings" }?.unit("vectors")?.value == nil)
    }

    @Test func oversizedCountsDoNotCrashAndArePartial() {
        let reply = ActivityReply(hosts: [.init(host: "mac", buckets: [
            .init(start_unix_ms: day.sinceMs, capability: "guardian", succeeded: Int64.max),
            .init(start_unix_ms: day.sinceMs, capability: "guardian", succeeded: 1),
        ])])
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        #expect(s.partial)
        #expect(s.jobsToday == .value(Int64.max))
    }

    @Test func anUnknownCapabilityIsCountedAndFlagged() throws {
        let fields = #""capability":"telepathy","succeeded":4,\#(counters)"#
        let reply = try decode(#"{"hosts":[\#(host(bucket(fields)))]}"#)
        let s = ActivitySummary.derive(reply, day: day, installed: nil)
        #expect(s.jobsToday == .value(4))
        #expect(s.rows.contains { $0.id == "telepathy" && $0.succeeded == 4 })
        #expect(s.partialReasons.contains("capability not recognised by this app: telepathy"))
        #expect(TodayPresentation(s, calendar: c).all.count == 14)
    }
}

@Suite struct LowRateFormatTests {
    @Test func lowPositiveRatesKeepDecimalsAndNeverReadZero() {
        let one = { (v: Double) in v.formatted(.number.precision(.fractionLength(1))) }
        let two = { (v: Double) in v.formatted(.number.precision(.fractionLength(2))) }
        #expect(NotchFormat.rate(148.4) == (148.0).formatted(.number.precision(.fractionLength(0))))
        #expect(NotchFormat.rate(10) == "10")
        #expect(NotchFormat.rate(9.94) == one(9.9))
        #expect(NotchFormat.rate(1.0) == one(1.0))
        #expect(NotchFormat.rate(0.456) == two(0.46))
        #expect(NotchFormat.rate(0.004) == "<0.01")
        #expect(NotchFormat.rate(0) == "0")
    }

    @Test func unitsReadAsWorkDone() {
        #expect(NotchFormat.unit("output_tokens", 1_234_567) == "1.2M output tokens")
        #expect(NotchFormat.unit("audio_seconds", 725) == "12 min 5 s of audio")
        #expect(NotchFormat.unit("audio_seconds", 4.25) == String(format: "%.1f s", 4.25) + " of audio")
        #expect(NotchFormat.unit("questions_answered", 96) == "96 decisions made")
    }
}

// MARK: - Collector

/// Answers by path prefix, so the activity query string (which carries the time) still matches.
private final class PrefixFetcher: NotchFetching, @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [String: [Result<String, ClientError>]] = [:]

    func set(_ prefix: String, _ r: Result<String, ClientError>...) { lock.withLock { replies[prefix] = r } }

    func get<T: Decodable & Sendable>(_ base: String, _ path: String, key: String?, timeout: TimeInterval,
                                      as type: T.Type) async throws -> T {
        let next: Result<String, ClientError>? = lock.withLock {
            guard let prefix = replies.keys.first(where: { path.hasPrefix($0) }), var list = replies[prefix],
                  !list.isEmpty else { return nil }
            let r = list.count > 1 ? list.removeFirst() : list[0]
            replies[prefix] = list
            return r
        }
        switch next {
        case .success(let json): return try JSONDecoder().decode(T.self, from: Data(json.utf8))
        case .failure(let e): throw e
        case nil: throw URLError(.cannotConnectToHost)
        }
    }
}

@MainActor
private func waitFor(_ seconds: Double = 2, _ check: () -> Bool) async -> Bool {
    let deadline = Date() + seconds
    while Date() < deadline {
        if check() { return true }
        try? await Task.sleep(for: .milliseconds(20))
    }
    return check()
}

@Suite @MainActor struct ActivityCollectorTests {
    private let engine = EngineAccess(running: true, label: "Running", baseURL: "http://192.0.2.10:8310", adminKey: "k",
                                      modelCapabilities: ["granite-4.2-3b": ["chat"],
                                                          "granite-embedding-97m-multilingual-r2": ["embeddings", "retrieval"]])

    private func reply(jobs: Int64) -> String {
        let midnight = LocalDay(now: Date(), calendar: .autoupdatingCurrent).sinceMs
        return #"{"bucket_seconds":900,"hosts":[{"host":"mac","coverage":[{"start_unix_ms":\#(midnight),"end_unix_ms":\#(midnight + 60_000)}],"buckets":[{"start_unix_ms":\#(midnight),"model":"granite-4.2-3b","capability":"chat","succeeded":\#(jobs),"failed":0,"cancelled":0,"units_unavailable":0,"units":{"output_tokens":100,"prompt_tokens":50}}]}]}"#
    }

    private func telemetry(_ f: PrefixFetcher) -> NotchTelemetry {
        let t = NotchTelemetry(fetcher: f, defaults: UserDefaults(suiteName: UUID().uuidString)!, omlxEndpoint: { nil })
        t.metricsInterval = .seconds(3_600); t.hostsInterval = .seconds(3_600); t.activityInterval = .seconds(3_600)
        return t
    }

    @Test func jobsArriveAndARiseLightsTheActivityDot() async {
        let f = PrefixFetcher()
        f.set("/internal/activity", .success(reply(jobs: 3)), .success(reply(jobs: 5)))
        let t = telemetry(f)
        let engine = self.engine
        t.start { engine }
        #expect(await waitFor { t.snapshot.activity.jobsToday == .value(3) })
        #expect(!t.snapshot.activity.recentJob)
        await t.pollActivity()
        t.stop()
        #expect(t.snapshot.activity.jobsToday == .value(5))
        #expect(t.snapshot.activity.recentJob)
        #expect(t.activityFreshness == .live)
    }

    @Test func anEngineWithoutTheEndpointIsUpdatePending() async {
        let f = PrefixFetcher()
        f.set("/internal/activity", .failure(.http(404, nil)))
        let t = telemetry(f)
        let engine = self.engine
        t.start { engine }
        #expect(await waitFor { t.snapshot.activity.jobsToday == .unavailable(ActivitySummary.pending) })
        t.stop()
    }

    @Test func installedFollowsTheDeploymentsNodesReport() async {
        let f = PrefixFetcher()
        f.set("/internal/activity", .success(reply(jobs: 0)))
        f.set("/internal/hosts", .success(#"{"hosts":[{"host":"mac","reachable":true,"hardware":{"managed_workers":[{"model":"granite-embedding-97m-multilingual-r2","state":"ready"}]}}]}"#))
        let t = telemetry(f)
        let engine = self.engine
        t.start { engine }
        #expect(await waitFor { t.snapshot.activity.rows.first { $0.id == "embeddings" }?.installed == true })
        t.stop()
        let rows = t.snapshot.activity.rows
        #expect(rows.first { $0.id == "retrieval" }?.idleStatus == "no activity today")
        // In the registry but not deployed on any node: not installed.
        #expect(rows.first { $0.id == "chat" }?.idleStatus == "not installed")
    }
}

// MARK: - Rendering

/// Offscreen renders of the new trigger and Today from fixture activity. Always rendered; written as
/// fixture-activity-*.png when NOTCH_RENDER_DIR is set (the data is synthetic).
@Suite @MainActor struct ActivityRenderTests {
    private func fixture(now: Date, calendar c: Calendar) -> NotchSnapshot {
        let day = LocalDay(now: now, calendar: c)
        let hour: Int64 = 3_600_000
        var buckets: [ActivityReply.Bucket] = []
        for (i, start) in day.bucketStarts.enumerated() where i % 3 != 1 {
            buckets.append(.init(start_unix_ms: start, model: "granite-4.2-3b", capability: "chat", succeeded: Int64(2 + i % 5),
                                 units: ["output_tokens": Double(900 * (2 + i % 5)), "prompt_tokens": Double(2_400 * (2 + i % 5)),
                                         "reasoning_tokens": Double(200 * (2 + i % 5))]))
            if i % 4 == 0 {
                buckets.append(.init(start_unix_ms: start, model: "granite-embedding-97m-multilingual-r2", capability: "embeddings",
                                     succeeded: 6, units: ["vectors": 48, "prompt_tokens": 3_100]))
            }
        }
        buckets.append(.init(start_unix_ms: day.sinceMs + 2 * hour, model: "granite-speech-5.0-470m-turboctc",
                             capability: "transcription", succeeded: 4, failed: 1, units: ["audio_seconds": 312]))
        buckets.append(.init(start_unix_ms: day.sinceMs + 3 * hour, model: "granite-switch-4.1-8b",
                             capability: "adapters", succeeded: 9, cancelled: 1, units: ["questions_answered": 27, "input_tokens": 4_000]))
        let reply = ActivityReply(hosts: [
            .init(host: "mac", coverage: [.init(start_unix_ms: day.sinceMs, end_unix_ms: day.untilMs)], buckets: buckets),
            .init(host: "cuda", coverage: [.init(start_unix_ms: day.sinceMs + hour, end_unix_ms: day.sinceMs + 4 * hour),
                                            .init(start_unix_ms: day.sinceMs + 5 * hour, end_unix_ms: day.untilMs)]),
            .init(host: "hx470", activity_error: "older engine"),
        ])
        var s = NotchSnapshot()
        s.activity = ActivitySummary.derive(reply, day: day, installed: ["chat", "embeddings", "retrieval", "transcription", "adapters"])
        s.activity.recentJob = true
        s.activeRequests = .value(2)
        s.sources = [ProductionSource(id: "engine", name: NotchSnapshot.engineSourceName, scope: "Requests run by LocalFleet engines",
                                      rate: .value(0.42), history: [], today: .value(175_000), prompt: .value(466_000),
                                      cached: .value(120_000), coverageStart: nil, gaps: 0, estimate: .collecting("Collecting history"), note: nil),
                     ProductionSource(id: "omlx", name: NotchSnapshot.omlxSourceName, scope: "Every client of the oMLX server on this Mac",
                                      rate: .value(38), history: [], today: .value(40_000), prompt: .value(90_000),
                                      cached: .value(30_000), coverageStart: nil, gaps: 0, estimate: .collecting("Collecting history"), note: nil)]
        s.models = [ModelLine(source: NotchSnapshot.omlxSourceName, host: "this Mac", model: "Qwen3-8B-4bit", state: "ready",
                              active: 1, detail: "prefill 1.2k / 4.0k · ETA 3 s")]
        s.omlxPressure = "ok"
        return s
    }

    private func save(_ image: NSImage, _ name: String) throws {
        guard let dir = NotchRenderEvidence.dir else { return }
        let rep = try #require(image.tiffRepresentation.flatMap(NSBitmapImageRep.init(data:)))
        let png = try #require(rep.representation(using: .png, properties: [:]))
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("fixture-activity-\(name).png"))
    }

    @Test func triggerAndTodayRenderFromActivity() throws {
        let c = Calendar.current
        let now = c.date(bySettingHour: 18, minute: 7, second: 0, of: Date())!
        let s = fixture(now: now, calendar: c)
        #expect(s.activity.jobsToday.value.map { $0 > 0 } == true)
        let display = NotchDisplay(snapshot: s, freshness: .live, activeHistory: [])
        #expect(display.accessibilitySummary.contains("completed jobs today"))
        #expect(display.accessibilitySummary.contains("partial coverage"))

        let trigger = try #require(NotchController.renderPreview(snapshot: s, freshness: .live, activeHistory: [],
                                                                 expanded: false, tab: .today,
                                                                 size: CGSize(width: 270, height: 28)))
        #expect(trigger.size == CGSize(width: 270, height: 28))
        try save(trigger, "trigger")

        let today = try #require(NotchController.renderPreview(snapshot: s, freshness: .live, activeHistory: [],
                                                               expanded: true, tab: .today,
                                                               size: CGSize(width: 650, height: 380)))
        // Both disclosures are open offscreen (all 13 families and diagnostics), so the body grows.
        #expect(today.size.width == 650)
        #expect(today.size.height > 380)
        try save(today, "today")

        let primary = try #require(NotchController.renderPreview(snapshot: s, freshness: .live, activeHistory: [],
                                                                 expanded: true, tab: .today,
                                                                 size: CGSize(width: 650, height: 380), expandDetails: false))
        try save(primary, "today-primary")

        let pending = try #require(NotchController.renderPreview(snapshot: NotchSnapshot(), freshness: .live, activeHistory: [],
                                                                 expanded: false, tab: .today,
                                                                 size: CGSize(width: 270, height: 28),
                                                                 activityFreshness: .offline("Stopped")))
        try save(pending, "trigger-offline")
    }
}
