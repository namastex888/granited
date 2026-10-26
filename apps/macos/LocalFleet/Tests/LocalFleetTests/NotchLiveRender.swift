import AppKit
import Foundation
import Testing
@testable import LocalFleet

/// Renders the notch from the live collector (the configured engine and a local oMLX server, GET
/// only) when NOTCH_LIVE_RENDER_DIR is set. Files are named live-*; nothing here is a fixture.
@Suite @MainActor struct NotchLiveRender {
    nonisolated static let dir = ProcessInfo.processInfo.environment["NOTCH_LIVE_RENDER_DIR"]

    @Test(.enabled(if: dir != nil), .timeLimit(.minutes(1))) func liveState() async throws {
        let dir = try #require(Self.dir)
        let state = AppState()
        await state.refresh()
        let t = NotchTelemetry(defaults: UserDefaults(suiteName: "notch-live-render")!)
        t.start(state: state)
        // Two hosts polls and several oMLX polls.
        try await Task.sleep(for: .seconds(12))
        t.stop()
        let report = """
        captured \(ISO8601DateFormatter().string(from: Date()))
        engine: \(state.engine.label) freshness: \(t.engineFreshness) omlx: \(t.omlxFreshness)
        headline source: \(t.snapshot.productionSource.isEmpty ? "none" : t.snapshot.productionSource) rate: \(t.snapshot.outputRate)
        sources: \(t.snapshot.sources.map { "\($0.name)=\($0.rate)" }.joined(separator: "; "))
        active: \(t.snapshot.activeRequests) slots: \(t.snapshot.slots) rejections: \(t.snapshot.rejections)
        nodes: \(t.snapshot.nodes.map { "\($0.host)(reachable \($0.reachable), usage \($0.usageReported.map(String.init) ?? "pending"))" }.joined(separator: ", "))
        models: \(t.snapshot.models.map { "\($0.model)@\($0.host) \($0.state)" }.joined(separator: ", "))
        power: \(t.snapshot.powerCoverage)
        memory: \(t.snapshot.memory.map { "\($0.label)@\($0.host)" }.joined(separator: ", "))
        """
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try report.write(toFile: dir + "/live-state.txt", atomically: true, encoding: .utf8)
        for (expanded, tab, size) in [(false, NotchTab.today, CGSize(width: 270, height: 28))]
            + NotchTab.allCases.map({ (true, $0, CGSize(width: 650, height: 380)) }) {
            let img = try #require(NotchController.renderPreview(snapshot: t.snapshot, freshness: t.freshness,
                                                                 activeHistory: t.activeHistory, expanded: expanded,
                                                                 tab: tab, size: size))
            let rep = try #require(img.tiffRepresentation.flatMap(NSBitmapImageRep.init(data:)))
            let png = try #require(rep.representation(using: .png, properties: [:]))
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("live-\(expanded ? tab.rawValue : "collapsed").png"))
        }
    }
}
