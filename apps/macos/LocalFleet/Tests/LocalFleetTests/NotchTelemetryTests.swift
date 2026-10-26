import Foundation
import Testing
@testable import LocalFleet

private func calendar(_ tz: String = "America/Sao_Paulo") -> Calendar {
    var c = Calendar(identifier: .gregorian)
    c.timeZone = TimeZone(identifier: tz)!
    return c
}

private func at(_ s: String, _ tz: String = "America/Sao_Paulo") -> Date {
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.timeZone = TimeZone(identifier: tz)
    f.dateFormat = "yyyy-MM-dd HH:mm:ss"
    return f.date(from: s)!
}

private func counters(_ output: Int64, since: Double = 1, prompt: Int64 = 0, cached: Int64 = 0) -> UsageLedger.Counters {
    .init(since: since, output: output, prompt: prompt, cached: cached)
}

@Suite struct UsageLedgerTests {
    let cal = calendar()

    @Test func firstObservationIsABaselineNotToday() {
        var l = UsageLedger()
        l.observe([("ryzen", counters(2_321_540))], at: at("2026-10-26 10:00:00"), calendar: cal)
        #expect(l.output == 0)
        #expect(l.coverageStart == at("2026-10-26 10:00:00"))
    }

    @Test func continuousDeltasAccumulatePerHost() {
        var l = UsageLedger()
        let t = at("2026-10-26 10:00:00")
        l.observe([("mac", counters(100, prompt: 10)), ("ryzen", counters(5_000))], at: t, calendar: cal)
        l.observe([("mac", counters(160, prompt: 30)), ("ryzen", counters(5_040))], at: t + 5, calendar: cal)
        #expect(l.output == 100)
        #expect(l.prompt == 20)
        #expect(l.outputByHost == ["mac": 60, "ryzen": 40])
    }

    @Test func engineRestartStartsANewBaselineWithoutSpike() {
        var l = UsageLedger()
        let t = at("2026-10-26 10:00:00")
        l.observe([("mac", counters(1_000, since: 1))], at: t, calendar: cal)
        l.observe([("mac", counters(1_050, since: 1))], at: t + 5, calendar: cal)
        l.observe([("mac", counters(900_000, since: 2))], at: t + 10, calendar: cal)
        l.observe([("mac", counters(900_020, since: 2))], at: t + 15, calendar: cal)
        #expect(l.output == 70)
        #expect(l.gaps == 1)
    }

    @Test func negativeDeltaRebaselines() {
        var l = UsageLedger()
        let t = at("2026-10-26 10:00:00")
        l.observe([("mac", counters(500))], at: t, calendar: cal)
        l.observe([("mac", counters(20))], at: t + 5, calendar: cal)
        l.observe([("mac", counters(30))], at: t + 10, calendar: cal)
        #expect(l.output == 10)
        #expect(l.gaps == 1)
    }

    @Test func longGapIsNotBridged() {
        var l = UsageLedger()
        let t = at("2026-10-26 10:00:00")
        l.observe([("mac", counters(0))], at: t, calendar: cal)
        l.observe([("mac", counters(10_000))], at: t + 600, calendar: cal)
        l.observe([("mac", counters(10_005))], at: t + 605, calendar: cal)
        #expect(l.output == 5)
        #expect(l.gaps == 1)
    }

    @Test func midnightResetsTotalsAndDropsTheStraddlingInterval() {
        var l = UsageLedger()
        l.observe([("mac", counters(0))], at: at("2026-10-26 23:59:50"), calendar: cal)
        l.observe([("mac", counters(40))], at: at("2026-10-26 23:59:55"), calendar: cal)
        #expect(l.output == 40)
        l.observe([("mac", counters(90))], at: at("2026-10-27 00:00:02"), calendar: cal)
        #expect(l.day == "2026-10-27")
        #expect(l.output == 0)
        #expect(l.gaps == 0)
        l.observe([("mac", counters(95))], at: at("2026-10-27 00:00:07"), calendar: cal)
        #expect(l.output == 5)
    }

    @Test func timeZoneChangeStartsTheNewLocalDay() {
        var l = UsageLedger()
        let t = at("2026-10-26 22:00:00", "America/Sao_Paulo")  // 01:00 next day in Lisbon
        l.observe([("mac", counters(0))], at: t, calendar: calendar("America/Sao_Paulo"))
        l.observe([("mac", counters(10))], at: t + 5, calendar: calendar("America/Sao_Paulo"))
        #expect(l.output == 10)
        l.observe([("mac", counters(20))], at: t + 10, calendar: calendar("Europe/Lisbon"))
        #expect(l.day == "2026-10-27")
        #expect(l.output == 0)
    }

    @Test func repeatedHostInOnePollCountsOnce() {
        var l = UsageLedger()
        let t = at("2026-10-26 10:00:00")
        l.observe([("mac", counters(0))], at: t, calendar: cal)
        l.observe([("mac", counters(30)), ("mac", counters(30))], at: t + 5, calendar: cal)
        #expect(l.output == 30)
    }

    @Test func rollingRateNeedsTheFullWindow() {
        var l = UsageLedger()
        let t = at("2026-10-26 10:00:00")
        for i in 0...6 { l.observe([("mac", counters(Int64(i) * 50))], at: t + Double(i) * 5, calendar: cal) }
        #expect(l.rollingRate(window: 60, at: t + 30) == nil)
        for i in 7...12 { l.observe([("mac", counters(Int64(i) * 50))], at: t + Double(i) * 5, calendar: cal) }
        #expect(l.rollingRate(window: 60, at: t + 60) == 10)
    }

    @Test func estimateWaitsForAnHourThenBracketsThePace() {
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        l.observe([("mac", counters(0))], at: t, calendar: cal)
        #expect(l.estimate(at: t, calendar: cal) == nil)
        var out: Int64 = 0
        for i in 1...720 {  // one hour of 5 s polls at 2 tok/s
            out += 10
            l.observe([("mac", counters(out))], at: t + Double(i) * 5, calendar: cal)
        }
        let now = t + 3_600
        let e = try! #require(l.estimate(at: now, calendar: cal))
        // 7_200 so far plus 11 h at 2 tok/s on both paces.
        #expect(e.low == 7_200 + 79_200)
        #expect(e.high == e.low)
    }
}

@Suite struct CoverageTests {
    let cal = calendar()

    private func feed(_ l: inout UsageLedger, from t: Date, polls: ClosedRange<Int>, hosts: [String], start: Int64 = 0) {
        for i in polls {
            l.observe(hosts.map { ($0, counters(start + Int64(i) * 10)) }, at: t + Double(i) * 5, calendar: cal)
        }
    }

    @Test func estimateNeedsAnUnbrokenHourAfterAGap() {
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        feed(&l, from: t, polls: 0...720, hosts: ["mac"])
        #expect(l.estimate(at: t + 3_600, calendar: cal) != nil)
        // Engine restart at +3605: the stretch restarts, so no estimate for the next hour.
        l.observe([("mac", counters(5, since: 2))], at: t + 3_605, calendar: cal)
        #expect(l.estimate(at: t + 3_605, calendar: cal) == nil)
        for i in 1...719 { l.observe([("mac", counters(5 + Int64(i) * 10, since: 2))], at: t + 3_605 + Double(i) * 5, calendar: cal) }
        #expect(l.estimate(at: t + 3_605 + 3_595, calendar: cal) == nil)
        l.observe([("mac", counters(5 + 7_200, since: 2))], at: t + 3_605 + 3_600, calendar: cal)
        #expect(l.estimate(at: t + 3_605 + 3_600, calendar: cal) != nil)
        #expect(l.gaps == 1)
    }

    @Test func aNodeJoiningOrLeavingRestartsTheRateWindow() {
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        feed(&l, from: t, polls: 0...12, hosts: ["mac"])
        #expect(l.rollingRate(window: 60, at: t + 60) == 2)
        l.observe([("mac", counters(130)), ("ryzen", counters(9_000_000))], at: t + 65, calendar: cal)
        #expect(l.rollingRate(window: 60, at: t + 65) == nil)
        #expect(l.output == 130)
        #expect(l.gaps == 1)
    }

    @Test func aPollWithNoReportingNodeMeasuresNothing() {
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        feed(&l, from: t, polls: 0...12, hosts: ["mac"])
        l.observe([], at: t + 65, calendar: cal)
        #expect(l.rollingRate(window: 60, at: t + 65) == nil)
        #expect(l.estimate(at: t + 65, calendar: cal) == nil)
    }

    @Test func aStaleSeriesShowsNoRate() {
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        feed(&l, from: t, polls: 0...12, hosts: ["mac"])
        #expect(l.rollingRate(window: 60, at: t + 60) == 2)
        #expect(l.rollingRate(window: 60, at: t + 120) == nil)
    }

    @Test func aClockThatMovesBackIsNotCounted() {
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        l.observe([("mac", counters(0))], at: t, calendar: cal)
        l.observe([("mac", counters(50))], at: t - 20, calendar: cal)
        #expect(l.output == 0)
        #expect(l.gaps == 1)
        var e = EnergyLedger()
        e.observe(sensor: "s", watts: 100, at: t, calendar: cal)
        e.observe(sensor: "s", watts: 100, at: t - 10, calendar: cal)
        #expect(e.wattHours["s"] == nil)
    }
}

@Suite struct EnergyLedgerTests {
    let cal = calendar()

    @Test func integratesOnlyValidIntervals() {
        var e = EnergyLedger()
        let t = at("2026-10-26 10:00:00")
        e.observe(sensor: "cuda/gpu", watts: 80, at: t, calendar: cal)
        e.observe(sensor: "cuda/gpu", watts: 100, at: t + 18, calendar: cal)  // 90 W × 18 s = 0.45 Wh
        e.observe(sensor: "cuda/gpu", watts: 100, at: t + 300, calendar: cal)  // gap: not bridged
        e.observe(sensor: "cuda/gpu", watts: nil, at: t + 305, calendar: cal)  // missing: ends interval
        e.observe(sensor: "cuda/gpu", watts: 100, at: t + 310, calendar: cal)
        #expect(abs((e.wattHours["cuda/gpu"] ?? 0) - 0.45) < 1e-9)
    }

    @Test func midnightResetsAndDoesNotStraddle() {
        var e = EnergyLedger()
        e.observe(sensor: "s", watts: 50, at: at("2026-10-26 23:59:55"), calendar: cal)
        e.observe(sensor: "s", watts: 50, at: at("2026-10-27 00:00:05"), calendar: cal)
        #expect(e.wattHours["s"] == nil)
        #expect(e.day == "2026-10-27")
    }
}

@Suite struct NotchSnapshotTests {
    let cal = calendar()

    private func reply(_ json: String) -> NotchHostsReply {
        try! JSONDecoder().decode(NotchHostsReply.self, from: Data(json.utf8))
    }

    @Test func enginesWithoutUsageShowUpdatePending() {
        let r = reply(#"{"hosts":[{"host":"mac","reachable":true,"hardware":{"managed_workers":[{"model":"m","state":"ready","active_requests":1,"occupied_slots":1,"max_concurrency":2}]}}]}"#)
        let s = NotchSnapshot.derive(hosts: r, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                     at: at("2026-10-26 10:00:00"), calendar: cal)
        #expect(s.outputRate == .unavailable("Engine update pending: no token usage reported yet"))
        #expect(s.activeRequests == .value(1))
        #expect(s.slots == .value(SlotUsage(occupied: 1, capacity: 2)))
        #expect(s.powerCoverage == "Engine update pending: no power sensors reported yet")
        #expect(s.nodes.first?.usageReported == nil)
    }

    @Test func coordinatorAndPeerAreEachCountedOnceFromTheirOwnBlocks() {
        // The coordinator forwarded the request; only the peer that ran it reports the tokens.
        let before = reply(#"{"hosts":[{"host":"mac","usage":{"since_unix_ms":1,"usage_reported":true,"models":{}}},{"host":"ryzen","usage":{"since_unix_ms":7,"usage_reported":true,"models":{"g":{"output_tokens":1000,"prompt_tokens":10}}}},{"host":"ryzen","usage":{"since_unix_ms":7,"models":{"g":{"output_tokens":1000}}}}]}"#)
        let after = reply(#"{"hosts":[{"host":"mac","usage":{"since_unix_ms":1,"usage_reported":true,"models":{}}},{"host":"ryzen","usage":{"since_unix_ms":7,"usage_reported":true,"models":{"g":{"output_tokens":1240,"prompt_tokens":40}}}},{"host":"ryzen","usage":{"since_unix_ms":7,"models":{"g":{"output_tokens":1240}}}}]}"#)
        var l = UsageLedger()
        let t = at("2026-10-26 10:00:00")
        for (r, dt) in [(before, 0.0), (after, 5.0)] {
            let hosts = NotchSnapshot.uniqueHosts(r)
            l.observe(hosts.compactMap { h in h.usage?.counters.map { (h.host, $0) } }, at: t + dt, calendar: cal)
        }
        #expect(l.output == 240)
        #expect(l.outputByHost == ["mac": 0, "ryzen": 240])
        let s = NotchSnapshot.derive(hosts: after, metrics: nil, usage: l, energy: EnergyLedger(), at: t + 5, calendar: cal)
        #expect(s.outputToday == .value(240))
        #expect(s.nodes.map(\.host) == ["mac", "ryzen"])
    }

    @Test func partialHostsAreNamedAndStaleRepliesShowNoValues() {
        let r = reply(#"{"hosts":[{"host":"mac","usage":{"since_unix_ms":1,"models":{}}},{"host":"hx470","reachable":true}]}"#)
        let s = NotchSnapshot.derive(hosts: r, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                     at: at("2026-10-26 10:00:00"), calendar: cal)
        #expect(s.usageNote == "Not reporting yet: hx470")
        #expect(s.outputRate == .collecting("Measuring the first 60 s"))
        let stale = NotchSnapshot.derive(hosts: nil, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                         at: at("2026-10-26 10:00:00"), calendar: cal)
        #expect(stale.outputRate == .unavailable("Engine not reachable"))
        #expect(stale.activeRequests == .unavailable("Engine not reachable"))
    }

    @Test func duplicateGpuDiscoveryIsOneDeviceAndScopesAreNotSummed() {
        let r = reply(#"{"hosts":[{"host":"cuda","reachable":true,"hardware":{"accelerators":[{"vendor":"nvidia","name":"NVIDIA GeForce RTX 4060 Ti","uuid":"GPU-1","memory_kind":"dedicated","total_memory_bytes":17175674880,"available_memory_bytes":9991880704,"power":{"watts":83.99,"scope":"GPU board, all workloads"}},{"class":"/sys/class/drm","node":"card1","vendor_pci_id":"0x10de","device_id":"0x2805"}]}},{"host":"mac","reachable":true,"hardware":{"accelerators":[{"vendor":"apple","name":"Apple M5 Pro","memory_kind":"unified","power":{"watts":null,"scope":"Apple SoC","reason":"powermetrics needs root"}}]}}]}"#)
        let s = NotchSnapshot.derive(hosts: r, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                     at: at("2026-10-26 10:00:00"), calendar: cal)
        #expect(s.power.count == 2)
        #expect(s.power.first?.watts == 83.99)
        #expect(s.power.last?.reason == "powermetrics needs root")
        #expect(s.memory.filter { $0.label.hasPrefix("NVIDIA") }.count == 1)
        #expect(s.powerCoverage.hasPrefix("Power measured on 1 of 2 nodes"))
    }
}

@Suite struct SourceTests {
    let cal = calendar()

    private func ledger(_ outputs: [Int64], host: String) -> UsageLedger {
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        for (i, o) in outputs.enumerated() { l.observe([(host, counters(o))], at: t + Double(i) * 5, calendar: cal) }
        return l
    }

    private func hosts(_ json: String) -> NotchHostsReply {
        try! JSONDecoder().decode(NotchHostsReply.self, from: Data(json.utf8))
    }

    @Test func omlxLeadsOnlyWhileTheFleetReportsNoUsageAndIsNeverAdded() {
        let fleetPending = hosts(#"{"hosts":[{"host":"mac","reachable":true,"hardware":{"managed_workers":[]}}]}"#)
        let o = OMLXStatus(uptime_seconds: 100, active_requests: 1, waiting_requests: 0,
                           total_prompt_tokens: 0, total_completion_tokens: 0, total_cached_tokens: 0)
        let om = ledger([0, 400], host: "omlx")
        let s = NotchSnapshot.derive(hosts: fleetPending, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                     omlx: o, omlxLedger: om, at: at("2026-10-26 12:00:05"), calendar: cal)
        #expect(s.productionSource == NotchSnapshot.omlxSourceName)
        #expect(s.outputToday == .value(400))
        #expect(s.sources.map(\.id) == ["engine", "omlx"])
        #expect(s.sources[0].today == .unavailable(NotchSnapshot.pendingUsage))

        let fleetReporting = hosts(#"{"hosts":[{"host":"mac","reachable":true,"usage":{"since_unix_ms":1,"models":{}}}]}"#)
        let fl = ledger([0, 70], host: "mac")
        let s2 = NotchSnapshot.derive(hosts: fleetReporting, metrics: nil, usage: fl, energy: EnergyLedger(),
                                      omlx: o, omlxLedger: om, at: at("2026-10-26 12:00:05"), calendar: cal)
        #expect(s2.productionSource == NotchSnapshot.engineSourceName)
        #expect(s2.outputToday == .value(70))  // not 470: sources are never summed
        #expect(s2.sources[1].today == .value(400))
    }

    @Test func omlxBehindTheEngineIsFlaggedAndNeverLeads() {
        let r = hosts(#"{"hosts":[{"host":"mac","reachable":true,"hardware":{"managed_workers":[{"model":"g","state":"ready","backend":"omlx","active_requests":1}]}}]}"#)
        let o = OMLXStatus(uptime_seconds: 100, total_completion_tokens: 10)
        let s = NotchSnapshot.derive(hosts: r, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                     omlx: o, omlxLedger: ledger([0, 10], host: "omlx"), at: at("2026-10-26 12:00:05"), calendar: cal)
        #expect(s.productionSource == "")
        #expect(s.outputRate == .unavailable(NotchSnapshot.pendingUsage))
        #expect(s.sources.last?.note?.contains("overlap") == true)
    }

    @Test func phaseDetailUsesOnlyMeasuredPhases() {
        let m = OMLXActivity.Model(id: "q", is_loading: false, active_requests: 2, waiting_requests: 1,
                                   prefilling: [.init(processed: 1_200, total: 4_000, eta: 3)],
                                   generating: [.init(generated_tokens: 50, tokens_per_second: 20.6), .init(generated_tokens: 9, tokens_per_second: 20)])
        #expect(NotchSnapshot.phaseDetail(m) == "prefill 1.2k / 4.0k · ETA 3 s · decoding, avg 41 tok/s since start across 2")
        let idle = OMLXActivity.Model(id: "q", is_loading: false, active_requests: 0, waiting_requests: 0, prefilling: [], generating: [])
        #expect(NotchSnapshot.phaseDetail(idle) == nil)
    }

    @Test func omlxEndpointUsesLoopbackOnly() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let local = dir.appendingPathComponent("a.json"), remote = dir.appendingPathComponent("b.json")
        try Data(#"{"server":{"host":"127.0.0.1","port":8888},"auth":{"api_key":"k","skip_api_key_verification":false}}"#.utf8).write(to: local)
        try Data(#"{"server":{"host":"203.0.113.20","port":8888},"auth":{"api_key":"k"}}"#.utf8).write(to: remote)
        #expect(NotchTelemetry.OMLXEndpoint.load(local.path) == .init(baseURL: "http://127.0.0.1:8888", key: "k"))
        #expect(NotchTelemetry.OMLXEndpoint.load(remote.path) == nil)
        let any = dir.appendingPathComponent("c.json")
        try Data(#"{"server":{"host":"0.0.0.0","port":8888},"auth":{"api_key":"k"}}"#.utf8).write(to: any)
        #expect(NotchTelemetry.OMLXEndpoint.load(any.path) == .init(baseURL: "http://127.0.0.1:8888", key: "k"))
    }
}

// MARK: - Collector

/// Serves canned JSON per path; a path can hang (until cancelled) or fail.
private final class FakeFetcher: NotchFetching, @unchecked Sendable {
    enum Reply { case json(String), hang, fail, delayed(TimeInterval, String) }
    private let lock = NSLock()
    private var replies: [String: [Reply]] = [:]

    func set(_ path: String, _ r: Reply...) { lock.withLock { replies[path] = r } }

    private func next(_ path: String) -> Reply {
        lock.withLock {
            guard var list = replies[path], !list.isEmpty else { return .fail }
            let r = list.count > 1 ? list.removeFirst() : list[0]
            replies[path] = list
            return r
        }
    }

    func get<T: Decodable & Sendable>(_ base: String, _ path: String, key: String?, timeout: TimeInterval,
                                      as type: T.Type) async throws -> T {
        switch next(path) {
        case .json(let s): return try JSONDecoder().decode(T.self, from: Data(s.utf8))
        case .delayed(let d, let s):
            try await Task.sleep(for: .seconds(d))
            return try JSONDecoder().decode(T.self, from: Data(s.utf8))
        case .hang:
            try await Task.sleep(for: .seconds(600))
            throw CancellationError()
        case .fail: throw URLError(.cannotConnectToHost)
        }
    }
}

private let metrics2 = #"{"workers":[{"model":"g","state":"ready","active_requests":2,"occupied_slots":2,"max_concurrency":2}],"operations":[],"rejections":{}}"#
private let hostsMac = #"{"hosts":[{"host":"mac","reachable":true,"hardware":{"managed_workers":[{"model":"g","state":"ready","active_requests":2,"occupied_slots":2,"max_concurrency":2}]}}]}"#
private let running = EngineAccess(running: true, label: "Running", baseURL: "http://127.0.0.1:1", adminKey: "k")

@MainActor
private func telemetry(_ f: FakeFetcher, omlx: Bool = true) -> NotchTelemetry {
    let t = NotchTelemetry(fetcher: f, defaults: UserDefaults(suiteName: UUID().uuidString)!,
                           omlxEndpoint: { omlx ? .init(baseURL: "http://127.0.0.1:2", key: nil) : nil })
    t.metricsInterval = .milliseconds(50); t.hostsInterval = .milliseconds(50); t.omlxInterval = .milliseconds(50)
    return t
}

@MainActor
private func eventually(_ seconds: Double = 2, _ check: () -> Bool) async -> Bool {
    let deadline = Date() + seconds
    while Date() < deadline {
        if check() { return true }
        try? await Task.sleep(for: .milliseconds(20))
    }
    return check()
}

@Suite @MainActor struct CollectorTests {
    @Test func aHangingOMLXServerDoesNotDelayTheFleet() async {
        let f = FakeFetcher()
        f.set("/internal/metrics", .json(metrics2)); f.set("/internal/hosts", .json(hostsMac))
        f.set("/api/status", .hang); f.set("/admin/api/activity", .hang)
        let t = telemetry(f)
        t.start { running }
        #expect(await eventually { t.snapshot.activeRequests == .value(2) })
        #expect(t.engineFreshness == .live)
        t.stop()
    }

    @Test func aFailingGatewayKeepsCachedRepliesOnlyUntilStale() async {
        let f = FakeFetcher()
        f.set("/internal/metrics", .json(metrics2), .fail); f.set("/internal/hosts", .json(hostsMac), .fail)
        let t = telemetry(f, omlx: false)
        t.staleAfter = 0.2
        t.start { running }
        t.stop()
        await t.pollEngine()
        #expect(t.engineFreshness == .live)
        try? await Task.sleep(for: .milliseconds(300))
        await t.pollEngine()
        guard case .stale = t.engineFreshness else { Issue.record("expected stale, got \(t.engineFreshness)"); return }
        #expect(t.freshness == t.engineFreshness)
    }

    @Test func aReplyFromBeforeStopIsDropped() async {
        let f = FakeFetcher()
        let nine = #"{"workers":[{"model":"g","state":"ready","active_requests":9}],"operations":[],"rejections":{}}"#
        let one = #"{"workers":[{"model":"g","state":"ready","active_requests":1}],"operations":[],"rejections":{}}"#
        f.set("/internal/metrics", .delayed(0.3, nine), .json(one)); f.set("/internal/hosts", .fail)
        let t = telemetry(f, omlx: false)
        t.start { running }
        t.stop()
        t.start { running }
        #expect(await eventually { t.snapshot.activeRequests == .value(1) })
        try? await Task.sleep(for: .milliseconds(450))
        #expect(t.snapshot.activeRequests == .value(1))
        t.stop()
    }

    @Test func freshOMLXIsNotDimmedByAnOfflineGateway() async {
        let f = FakeFetcher()
        f.set("/api/status", .json(#"{"uptime_seconds":100,"active_requests":0,"total_completion_tokens":50,"total_prompt_tokens":5,"total_cached_tokens":0}"#))
        f.set("/admin/api/activity", .json(#"{"active_models":{"models":[]}}"#))
        let t = telemetry(f)
        t.start { EngineAccess(running: false, label: "Stopped", baseURL: "http://127.0.0.1:1", adminKey: "k") }
        #expect(await eventually { t.snapshot.productionSource == NotchSnapshot.omlxSourceName })
        #expect(t.freshness == .live)
        #expect(t.engineFreshness == .offline("Stopped"))
        t.stop()
    }
}

@Suite struct AppsTests {
    @Test func appsCountOnlyLabelledCallersInTheWindow() {
        let now = Date(timeIntervalSince1970: 2_000_000)
        let ms = now.timeIntervalSince1970 * 1_000
        let json = """
        {"hosts":[{"host":"mac","reachable":true,"clients":{"window_seconds":300,"active":{"editor":1},"unidentified_active":2,
          "last_seen_unix_ms":{"editor":\(ms - 1_000),"notes":\(ms - 200_000),"old":\(ms - 900_000)}}},
         {"host":"cuda","reachable":true,"clients":{"window_seconds":300,"active":{},"unidentified_active":1,"last_seen_unix_ms":{"notes":\(ms - 5_000)}}}]}
        """
        let r = try! JSONDecoder().decode(NotchHostsReply.self, from: Data(json.utf8))
        let s = NotchSnapshot.derive(hosts: r, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(), at: now, calendar: .current)
        #expect(s.apps == .value(2))
        #expect(s.appLabels == ["editor", "notes"])
        #expect(s.labelledActive == 1)
        #expect(s.unidentifiedActive == 3)
    }

    @Test func enginesWithoutClientBlocksAreUpdatePending() {
        let r = try! JSONDecoder().decode(NotchHostsReply.self, from: Data(#"{"hosts":[{"host":"mac","reachable":true}]}"#.utf8))
        let s = NotchSnapshot.derive(hosts: r, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(), at: Date(), calendar: .current)
        #expect(s.apps == .unavailable("Engine update pending: no client labels reported yet"))
    }
}


@Suite struct ReviewFixTests {
    let cal = calendar()

    @Test func yesterdaysTotalsAreNotShownAfterMidnightWithoutANewPoll() {
        var l = UsageLedger()
        l.observe([("mac", counters(0))], at: at("2026-10-26 23:59:50"), calendar: cal)
        l.observe([("mac", counters(40_000))], at: at("2026-10-26 23:59:55"), calendar: cal)
        var e = EnergyLedger()
        e.observe(sensor: "s", watts: 100, at: at("2026-10-26 23:59:50"), calendar: cal)
        e.observe(sensor: "s", watts: 100, at: at("2026-10-26 23:59:55"), calendar: cal)
        let r = try! JSONDecoder().decode(NotchHostsReply.self, from: Data(#"{"hosts":[{"host":"mac","reachable":true,"usage":{"since_unix_ms":1,"models":{}},"hardware":{"accelerators":[{"name":"g","uuid":"s","power":{"watts":100,"scope":"x"}}]}}]}"#.utf8))
        let s = NotchSnapshot.derive(hosts: r, metrics: nil, usage: l, energy: e, at: at("2026-10-27 00:00:03"), calendar: cal)
        #expect(s.outputToday == .value(0))
        #expect(s.coverageStart == nil)
        #expect(s.power.first?.wattHoursToday == nil)
        // The first poll of the new day after a roll counts nothing from yesterday's baseline.
        l.roll(to: at("2026-10-27 00:00:03"), calendar: cal)
        l.observe([("mac", counters(40_100))], at: at("2026-10-27 00:00:05"), calendar: cal)
        #expect(l.output == 0)
    }

    @Test func noRateIsShownForAWindowAfterAGap() {
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        for i in 0...12 { l.observe([("mac", counters(Int64(i) * 10))], at: t + Double(i) * 5, calendar: cal) }
        #expect(l.rollingRate(window: 60, at: t + 60) == 2)
        l.observe([("mac", counters(9_000))], at: t + 660, calendar: cal)
        #expect(l.rollingRate(window: 60, at: t + 660) == nil)
        l.observe([("mac", counters(9_010))], at: t + 665, calendar: cal)
        #expect(l.rollingRate(window: 60, at: t + 665) == nil)
    }

    @Test func aStalledOMLXRequestIsNotShownAsDecoding() {
        let m = OMLXActivity.Model(id: "q", is_loading: false, active_requests: 1, waiting_requests: 0, prefilling: [],
                                   generating: [.init(generated_tokens: 500, tokens_per_second: 30, last_activity_age_seconds: 40)])
        #expect(NotchSnapshot.phaseDetail(m) == nil)
    }
}

@Suite @MainActor struct ReviewCollectorTests {
    @Test func aStaleHostsReplyIsDropped() async {
        let f = FakeFetcher()
        f.set("/internal/metrics", .json(metrics2), .fail); f.set("/internal/hosts", .json(hostsMac), .fail)
        let t = telemetry(f, omlx: false)
        t.staleAfter = 0.2
        t.start { running }
        t.stop()
        await t.pollEngine()
        #expect(t.snapshot.activeRequests == .value(2))
        try? await Task.sleep(for: .milliseconds(300))
        await t.pollEngine()
        #expect(t.snapshot.activeRequests == .unavailable("Engine not reachable"))
        #expect(t.snapshot.slots == .unavailable("Engine not reachable"))
    }

    @Test func aSlowHostsReplyDoesNotStallMetrics() async {
        let f = FakeFetcher()
        f.set("/internal/metrics", .json(metrics2)); f.set("/internal/hosts", .hang)
        let t = telemetry(f, omlx: false)
        t.start { running }
        #expect(await eventually(1) { t.activeHistory.count >= 5 })
        t.stop()
    }
}


@Suite struct HysteresisTests {
    @Test func aReturningHostDoesNotSpikeTheSparkline() {
        let cal = calendar()
        var l = UsageLedger()
        let t = at("2026-10-26 12:00:00")
        var mac: Int64 = 0, cuda: Int64 = 0
        for i in 0...14 {
            mac += 10; cuda += 10
            let hosts: [(host: String, counters: UsageLedger.Counters)] = (i == 10 || i == 11)
                ? [("mac", counters(mac))] : [("mac", counters(mac)), ("cuda", counters(cuda, since: 2))]
            l.observe(hosts, at: t + Double(i) * 5, calendar: cal)
        }
        let history = NotchSnapshot.rateHistory(l, at: t + 70).map(\.value)
        #expect(history.allSatisfy { $0 <= 4 })
        #expect(l.gaps == 0)
        #expect(l.rollingRate(window: 60, at: t + 70) != nil)
    }
}
