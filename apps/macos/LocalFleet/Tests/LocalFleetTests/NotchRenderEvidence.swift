import AppKit
import Foundation
import Testing
@testable import LocalFleet

/// Renders the notch for fixture states through the real derivation and views. Runs only when
/// NOTCH_RENDER_DIR is set; every file is named fixture-* because the data is synthetic.
@Suite @MainActor struct NotchRenderEvidence {
    nonisolated static let dir = ProcessInfo.processInfo.environment["NOTCH_RENDER_DIR"]

    private let cal = Calendar.current
    private let collapsed = CGSize(width: 270, height: 28)
    private let expanded = CGSize(width: 650, height: 380)

    private func hosts(_ json: String) -> NotchHostsReply {
        try! JSONDecoder().decode(NotchHostsReply.self, from: Data(json.utf8))
    }

    /// A ledger that observed `perPoll` output tokens every 5 s for `polls` polls ending at `now`.
    private func ledger(host: String, polls: Int, perPoll: (Int) -> Int64, now: Date) -> UsageLedger {
        var l = UsageLedger()
        var out: Int64 = 1_000_000
        for i in 0...polls {
            if i > 0 { out += perPoll(i) }
            l.observe([(host, .init(since: 1, output: out, prompt: out / 3, cached: out / 5))],
                      at: now - Double(polls - i) * 5, calendar: cal)
        }
        return l
    }

    private let fleet = #"""
    {"hosts":[
     {"host":"mac","reachable":true,"usage":{"since_unix_ms":1,"usage_reported":true,"models":{}},
      "hardware":{"ram":{"total_bytes":51539607552,"available_bytes":41045835776},
       "accelerators":[{"vendor":"apple","name":"Apple M5 Pro","memory_kind":"unified","power":{"watts":null,"scope":"Apple SoC","reason":"powermetrics needs root"}}],
       "managed_workers":[{"model":"granite-4.2-3b","state":"ready","active_requests":1,"occupied_slots":1,"max_concurrency":2}]}},
     {"host":"cuda","reachable":true,"usage":{"since_unix_ms":7,"usage_reported":true,"models":{}},
      "hardware":{"ram":{"total_bytes":130395140096,"available_bytes":66883022848},
       "accelerators":[{"vendor":"nvidia","name":"NVIDIA GeForce RTX 4060 Ti","uuid":"GPU-1","memory_kind":"dedicated","total_memory_bytes":17175674880,"available_memory_bytes":10411880704,"power":{"watts":84.0,"limit_watts":165,"scope":"GPU board, all workloads","source":"nvidia-smi power.draw"}},{"class":"/sys/class/drm","node":"card1","vendor_pci_id":"0x10de","device_id":"0x2805"}],
       "managed_workers":[{"model":"granite-4.2-3b","state":"ready","active_requests":3,"occupied_slots":2,"max_concurrency":2}]}},
     {"host":"hx470","reachable":true,
      "hardware":{"accelerators":[{"class":"/sys/class/drm","node":"card1","vendor_pci_id":"0x1002","device_id":"0x150e","power":{"watts":26.1,"scope":"AMDGPU SoC sensor, not wall power","source":"hwmon power1_average"}}],
       "managed_workers":[]}}
    ]}
    """#

    private func save(_ image: NSImage?, _ name: String) throws {
        guard let dir = Self.dir else { return }
        let img = try #require(image)
        let rep = try #require(img.tiffRepresentation.flatMap(NSBitmapImageRep.init(data:)))
        let png = try #require(rep.representation(using: .png, properties: [:]))
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("fixture-\(name).png"))
    }

    private func render(_ s: NotchSnapshot, _ fresh: Freshness, _ name: String, history: [TimePoint] = []) throws {
        try save(NotchController.renderPreview(snapshot: s, freshness: fresh, activeHistory: history,
                                               expanded: false, tab: .today, size: collapsed), "\(name)-collapsed")
        for tab in NotchTab.allCases {
            try save(NotchController.renderPreview(snapshot: s, freshness: fresh, activeHistory: history,
                                                   expanded: true, tab: tab, size: expanded), "\(name)-\(tab.rawValue)")
        }
    }

    @Test(.enabled(if: dir != nil)) func measuredFleet() throws {
        let now = Date()
        // 3 h of 5 s polls; output varies so the chart and the pace are not flat.
        let l = ledger(host: "cuda", polls: 2_160, perPoll: { i in Int64(500 + 400 * sin(Double(i) / 9)) + (i % 7 == 0 ? 900 : 0) }, now: now)
        var e = EnergyLedger()
        for i in 0...60 { e.observe(sensor: "cuda/GPU-1", watts: 80 + Double(i % 5), at: now - Double(60 - i) * 5, calendar: cal) }
        let s = NotchSnapshot.derive(hosts: hosts(fleet), metrics: nil, usage: l, energy: e, at: now, calendar: cal)
        let history = (0..<30).map { TimePoint(at: now - Double(29 - $0) * 2, value: Double([3, 4, 4, 2, 4][$0 % 5])) }
        try render(s, .live, "measured-fleet", history: history)
    }

    @Test(.enabled(if: dir != nil)) func pendingEnginesWithOMLXLeading() throws {
        let now = Date()
        let pending = hosts(#"{"hosts":[{"host":"mac","reachable":true,"hardware":{"ram":{"total_bytes":51539607552,"available_bytes":40045835776},"managed_workers":[]}},{"host":"cuda","reachable":true,"hardware":{"managed_workers":[{"model":"granite-4.2-3b","state":"ready","active_requests":0,"occupied_slots":0,"max_concurrency":2}]}}]}"#)
        let om = ledger(host: "omlx", polls: 40, perPoll: { i in Int64(150 + 60 * cos(Double(i) / 3)) }, now: now)
        let status = OMLXStatus(uptime_seconds: 7_200, active_requests: 1, waiting_requests: 1,
                                total_prompt_tokens: 90_000, total_completion_tokens: 40_000, total_cached_tokens: 30_000,
                                model_memory_used: 9_000_000_000, model_memory_max: 32_000_000_000)
        let activity = OMLXActivity(active_models: .init(models: [
            .init(id: "Qwen3-8B-4bit", is_loading: false, active_requests: 1, waiting_requests: 1,
                  prefilling: [.init(processed: 1_200, total: 4_000, eta: 3)],
                  generating: [.init(generated_tokens: 180, tokens_per_second: 41)])],
            memory_pressure: .init(pressure_level: "ok")))
        let s = NotchSnapshot.derive(hosts: pending, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                     omlx: status, omlxActivity: activity, omlxLedger: om, at: now, calendar: cal)
        try render(s, .live, "pending-engines-omlx-leads")
    }

    @Test(.enabled(if: dir != nil)) func pendingEnginesNoOMLX() throws {
        let pending = hosts(#"{"hosts":[{"host":"mac","reachable":true,"hardware":{"managed_workers":[]}}]}"#)
        let s = NotchSnapshot.derive(hosts: pending, metrics: nil, usage: UsageLedger(), energy: EnergyLedger(),
                                     at: Date(), calendar: cal)
        try render(s, .live, "pending-engines")
    }

    @Test(.enabled(if: dir != nil)) func staleTelemetry() throws {
        let now = Date()
        let l = ledger(host: "cuda", polls: 30, perPoll: { _ in 600 }, now: now - 120)
        let s = NotchSnapshot.derive(hosts: hosts(fleet), metrics: nil, usage: l, energy: EnergyLedger(), at: now, calendar: cal)
        try render(s, .stale(since: now - 120), "stale")
    }
}
