import AppKit
import SwiftUI

// MARK: - Inputs

enum NotchTab: String, CaseIterable, Identifiable, Sendable {
    case production, demand, energy

    var id: String { rawValue }
    var title: String {
        switch self {
        case .production: "Production"
        case .demand: "Demand"
        case .energy: "Energy"
        }
    }
    var hint: String {
        switch self {
        case .production: "Shows output rate, today's output and the midnight estimate"
        case .demand: "Shows requests, slots, rejections and nodes"
        case .energy: "Shows power sensors and memory"
        }
    }
}

/// What the notch shows: one telemetry snapshot as published by NotchTelemetry (or a fixture in
/// renderPreview). Views read only this, so the same views render live and offscreen.
struct NotchDisplay {
    var snapshot: NotchSnapshot
    /// Freshness of the source behind the headline (oMLX or the fleet).
    var freshness: Freshness
    var activeHistory: [TimePoint]
    /// Fleet engine: Demand, Energy and the node list.
    var engineFreshness: Freshness
    /// Local oMLX server.
    var omlxFreshness: Freshness

    init(snapshot: NotchSnapshot, freshness: Freshness, activeHistory: [TimePoint],
         engineFreshness: Freshness? = nil, omlxFreshness: Freshness? = nil) {
        self.snapshot = snapshot
        self.freshness = freshness
        self.activeHistory = activeHistory
        self.engineFreshness = engineFreshness ?? freshness
        self.omlxFreshness = omlxFreshness ?? freshness
    }

    var isLive: Bool { freshness == .live }
    var engineLive: Bool { engineFreshness == .live }

    /// Freshness of one source, by ProductionSource id ("engine", "omlx") or source name.
    func freshness(ofSource source: String) -> Freshness {
        switch source {
        case "omlx", NotchSnapshot.omlxSourceName: omlxFreshness
        case "engine", NotchSnapshot.engineSourceName: engineFreshness
        default: freshness
        }
    }

    func freshness(of gauge: MemoryGauge) -> Freshness {
        gauge.id.hasPrefix("omlx/") ? omlxFreshness : engineFreshness
    }

    static func opacity(_ f: Freshness) -> Double { f == .live ? 1 : NotchStyle.dimmed }
}

/// Sizes of the two shapes. The trigger lives in the menu-bar strip: on a notched screen one left
/// wing plus the camera housing, ending at the housing's right edge so nothing covers the status
/// items right of the camera; elsewhere a small centred pill (`cameraWidth` 0). The details body
/// (`expandedSize`) sits entirely below the strip.
struct NotchLayout: Equatable {
    var cameraWidth: CGFloat
    var barHeight: CGFloat
    var wingWidth: CGFloat
    var expandedSize: CGSize
    /// Offscreen render: tabs draw every row at full height instead of inside a ScrollView,
    /// which ImageRenderer cannot draw. `expandedSize.height` is then a minimum.
    var isPreview = false

    static let bodyShoulder: CGFloat = 12
    static let bodyCorner: CGFloat = 22
    static let defaultWing: CGFloat = 104

    var hasNotch: Bool { cameraWidth > 0 }
    var collapsedShoulder: CGFloat { hasNotch ? 6 : 4 }
    var collapsedCorner: CGFloat { min(barHeight / 2, 10) }
    /// No right shoulder on a notched screen: the right edge meets the hardware housing.
    var rightShoulder: CGFloat { hasNotch ? 0 : collapsedShoulder }
    var collapsedSize: CGSize {
        CGSize(width: collapsedShoulder + wingWidth + cameraWidth + rightShoulder, height: barHeight)
    }

    /// Layout for an offscreen render of the given size (renderPreview).
    static func preview(size: CGSize, expanded: Bool) -> NotchLayout {
        if expanded {
            return NotchLayout(cameraWidth: size.width >= 420 ? 160 : 0, barHeight: 28,
                               wingWidth: defaultWing, expandedSize: size, isPreview: true)
        }
        let shoulder: CGFloat = 6
        let camera = max(0, size.width - defaultWing - shoulder)
        let wing = camera > 0 ? defaultWing : max(0, size.width - 8)
        return NotchLayout(cameraWidth: camera, barHeight: size.height, wingWidth: wing,
                           expandedSize: CGSize(width: 650, height: 380), isPreview: true)
    }
}

struct NotchActions {
    var toggle: @MainActor () -> Void = {}
    var details: @MainActor () -> Void = {}
}

// MARK: - Style and formatting

enum NotchStyle {
    static let mint = Color(red: 0x5B / 255, green: 0xE3 / 255, blue: 0xB0 / 255)
    static let primary = Color.white.opacity(0.92)
    static let secondary = Color.white.opacity(0.55)
    static let tertiary = Color.white.opacity(0.36)
    static let hairline = Color.white.opacity(0.10)
    static let dimmed = 0.5

    static func number(_ size: CGFloat, _ weight: Font.Weight = .semibold) -> Font {
        .system(size: size, weight: weight).monospacedDigit()
    }
}

@MainActor
enum NotchMark {
    /// The LocalFleet mark as a template (white in the notch); the `cpu` symbol for a bare binary.
    static let image: NSImage? = {
        guard let mark = Bundle.main.image(forResource: "MenuBarMark") else { return nil }
        mark.isTemplate = true
        return mark
    }()
}

enum NotchFormat {
    static func tokens(_ n: Int64) -> String {
        let v = Double(n)
        switch abs(v) {
        case ..<1_000: return "\(n)"
        case ..<9_950: return decimal(v / 1_000) + "k"
        case ..<999_500: return "\(Int((v / 1_000).rounded()))k"
        case ..<999_950_000: return decimal(v / 1_000_000) + "M"
        default: return decimal(v / 1_000_000_000) + "B"
        }
    }

    static func range(_ r: TokenRange) -> String {
        let low = tokens(r.low), high = tokens(r.high)
        return low == high ? low : "\(low)–\(high)"
    }

    static func rate(_ r: Double) -> String { r.formatted(.number.precision(.fractionLength(0))) }
    static func watts(_ w: Double) -> String { String(format: "%.0f W", w) }

    static func wattHours(_ wh: Double) -> String {
        if wh >= 1_000 { return String(format: "%.2f kWh", wh / 1_000) }
        return wh < 10 ? String(format: "%.1f Wh", wh) : String(format: "%.0f Wh", wh)
    }

    static func memory(_ used: Int64, _ total: Int64) -> String {
        let gib = 1_073_741_824.0
        let t = Double(total) / gib
        let totalText = abs(t - t.rounded()) < 0.05 ? String(format: "%.0f", t) : String(format: "%.1f", t)
        return String(format: "%.1f / ", Double(used) / gib) + totalText + " GiB"
    }

    static func time(_ date: Date) -> String { date.formatted(date: .omitted, time: .shortened) }

    private static func decimal(_ v: Double) -> String {
        let s = String(format: "%.1f", v)
        return s.hasSuffix(".0") ? String(s.dropLast(2)) : s
    }
}

extension String {
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

extension Reading {
    var reason: String? {
        switch self {
        case .value: nil
        case .collecting(let why), .unavailable(let why): why
        }
    }
}

extension NotchDisplay {
    /// Short state for the collapsed wing when there is no rate to show.
    var rateState: String {
        if case .offline = freshness { return "offline" }
        if case .stale = freshness { return "stale" }
        if case .collecting = snapshot.outputRate { return "measuring" }
        return "no data"
    }

    /// The series the small sparkline draws and what it means, or nil when neither has two points.
    var sparkline: (points: [TimePoint], label: String)? {
        if snapshot.rateHistory.count >= 2 { return (snapshot.rateHistory, "Completed output rate, last 60 s") }
        if activeHistory.count >= 2 { return (activeHistory, "Active requests, last 60 s") }
        return nil
    }

    /// Production reports the headline source; Demand and Energy report the fleet engine.
    func statusLine(for tab: NotchTab) -> String? {
        let (f, prefix) = tab == .production ? (freshness, "") : (engineFreshness, "Fleet ")
        switch f {
        case .live: return nil
        case .stale(let since): return "\(prefix)telemetry stale since \(NotchFormat.time(since))".capitalizedFirst
        case .offline(let why): return "\(prefix)offline · \(why)".capitalizedFirst
        }
    }

    var accessibilitySummary: String {
        let source = snapshot.productionSource.isEmpty ? "" : " from \(snapshot.productionSource)"
        let rate = snapshot.outputRate.value.map { "\(NotchFormat.rate($0)) tokens per second\(source)" }
            ?? "output rate \(snapshot.outputRate.reason ?? "unavailable")"
        let active = snapshot.activeRequests.value.map { plural($0, "active request") }
            ?? "active requests \(snapshot.activeRequests.reason ?? "unavailable")"
        let fresh = switch freshness {
        case .live: "telemetry live"
        case .stale(let since): "telemetry stale since \(NotchFormat.time(since))"
        case .offline(let why): "telemetry offline: \(why)"
        }
        return "LocalFleet: \(rate), \(active), \(fresh)"
    }
}

// MARK: - Shape

/// The notch silhouette: flush with the top edge, with concave shoulders that flare into the
/// screen edge and continuous rounded bottom corners.
struct NotchShape: Shape {
    var leftShoulder: CGFloat
    var rightShoulder: CGFloat
    var corner: CGFloat

    var animatableData: AnimatablePair<CGFloat, AnimatablePair<CGFloat, CGFloat>> {
        get { AnimatablePair(leftShoulder, AnimatablePair(rightShoulder, corner)) }
        set { leftShoulder = newValue.first; rightShoulder = newValue.second.first; corner = newValue.second.second }
    }

    func path(in rect: CGRect) -> Path {
        let l = min(leftShoulder, rect.width / 4)
        let r = min(rightShoulder, rect.width / 4)
        let c = min(corner, (rect.width - l - r) / 2, rect.height - max(l, r))
        var p = Path()
        p.move(to: CGPoint(x: rect.minX, y: rect.minY))
        p.addQuadCurve(to: CGPoint(x: rect.minX + l, y: rect.minY + l), control: CGPoint(x: rect.minX + l, y: rect.minY))
        p.addLine(to: CGPoint(x: rect.minX + l, y: rect.maxY - c))
        p.addQuadCurve(to: CGPoint(x: rect.minX + l + c, y: rect.maxY), control: CGPoint(x: rect.minX + l, y: rect.maxY))
        p.addLine(to: CGPoint(x: rect.maxX - r - c, y: rect.maxY))
        p.addQuadCurve(to: CGPoint(x: rect.maxX - r, y: rect.maxY - c), control: CGPoint(x: rect.maxX - r, y: rect.maxY))
        p.addLine(to: CGPoint(x: rect.maxX - r, y: rect.minY + r))
        p.addQuadCurve(to: CGPoint(x: rect.maxX, y: rect.minY), control: CGPoint(x: rect.maxX - r, y: rect.minY))
        p.closeSubpath()
        return p
    }
}

// MARK: - Surface

/// The trigger: the collapsed black shape in the menu-bar strip (left wing + camera housing on a
/// notched screen, a small pill elsewhere). `joined` squares its bottom corners while the body
/// hangs below it.
struct NotchTrigger: View {
    let display: NotchDisplay
    let layout: NotchLayout
    var joined = false
    var actions = NotchActions()

    var body: some View {
        let shape = NotchShape(leftShoulder: layout.collapsedShoulder, rightShoulder: layout.rightShoulder,
                               corner: joined ? 0 : layout.collapsedCorner)
        CollapsedNotch(display: display, layout: layout, actions: actions)
            .frame(width: layout.collapsedSize.width, height: layout.collapsedSize.height)
            .background(shape.fill(Color.black))
            .clipShape(shape)
            .environment(\.colorScheme, .dark)
    }
}

/// The details body: a black panel whose top edge meets the bottom of the menu-bar strip, with
/// small flared shoulders, below the trigger. Tabs scroll in the live panel; a preview draws every
/// row at full height.
struct NotchBody: View {
    let display: NotchDisplay
    let layout: NotchLayout
    @Binding var tab: NotchTab
    /// True once a click pinned the body open (it is key then): the tab selector takes focus.
    var pinned = false
    var actions = NotchActions()

    var body: some View {
        let shape = NotchShape(leftShoulder: NotchLayout.bodyShoulder, rightShoulder: NotchLayout.bodyShoulder,
                               corner: NotchLayout.bodyCorner)
        let inset = NotchLayout.bodyShoulder + 22
        VStack(spacing: 0) {
            ZStack {
                NotchTabBar(tab: $tab, wantsFocus: pinned)
                HStack {
                    Spacer(minLength: 0)
                    ActiveIndicator(display: display)
                        .opacity(display.isLive ? 1 : NotchStyle.dimmed)
                }
            }
            .padding(.top, 10)

            VStack(alignment: .leading, spacing: 8) {
                if let status = display.statusLine(for: tab) {
                    Text(status)
                        .font(.system(size: 11))
                        .foregroundStyle(NotchStyle.secondary)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .center)
                }
                if layout.isPreview {
                    tabContent.frame(maxHeight: .infinity, alignment: .top)
                } else {
                    ScrollView(.vertical) { tabContent.padding(.bottom, 4) }
                        .scrollIndicators(.automatic)
                        .frame(maxHeight: .infinity)
                }
            }
            .padding(.top, 10)
            .padding(.bottom, 8)

            Hairline()
            NotchFooter(display: display, actions: actions)
                .frame(height: 34)
        }
        .padding(.horizontal, inset)
        .padding(.bottom, 4)
        .frame(width: layout.expandedSize.width)
        .frame(minHeight: layout.expandedSize.height,
               maxHeight: layout.isPreview ? nil : layout.expandedSize.height, alignment: .top)
        .background(shape.fill(Color.black))
        .clipShape(shape)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .environment(\.colorScheme, .dark)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("LocalFleet monitor")
        .accessibilityAction(named: "Close") { actions.toggle() }
    }

    @ViewBuilder private var tabContent: some View {
        Group {
            switch tab {
            case .production: ProductionTab(display: display)
            case .demand: DemandTab(display: display)
            case .energy: EnergyTab(display: display)
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }
}

/// Offscreen render for renderPreview: the trigger when collapsed, the body when expanded.
struct NotchSurface: View {
    let display: NotchDisplay
    let layout: NotchLayout
    let expanded: Bool
    @Binding var tab: NotchTab

    var body: some View {
        if expanded {
            NotchBody(display: display, layout: layout, tab: $tab)
        } else {
            NotchTrigger(display: display, layout: layout)
        }
    }
}

/// Active requests and the 60 s sparkline (output rate when measured, else active requests).
struct ActiveIndicator: View {
    let display: NotchDisplay

    var body: some View {
        HStack(spacing: 6) {
            Group {
                if let n = display.snapshot.activeRequests.value {
                    HStack(alignment: .firstTextBaseline, spacing: 3) {
                        Text("\(n)").font(NotchStyle.number(13, .bold)).foregroundStyle(NotchStyle.primary)
                        Text("active").font(.system(size: 11, weight: .medium)).foregroundStyle(NotchStyle.secondary)
                    }
                } else {
                    Text("— active").font(.system(size: 11, weight: .medium)).foregroundStyle(NotchStyle.secondary)
                }
            }
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            if let line = display.sparkline {
                Sparkline(points: line.points)
                    .frame(width: 30, height: 12)
                    .help(line.label)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Active requests")
        .accessibilityValue(display.snapshot.activeRequests.value.map(String.init)
            ?? display.snapshot.activeRequests.reason ?? "unavailable")
    }
}

/// Mark + rate, then the camera housing, which stays empty at the right end of the shape.
struct NotchWings: View {
    let display: NotchDisplay
    let cameraWidth: CGFloat
    var wingWidth: CGFloat?

    var body: some View {
        HStack(spacing: 0) {
            left.frame(maxWidth: wingWidth ?? .infinity, alignment: .leading)
            Color.clear.frame(width: cameraWidth)
        }
        .opacity(display.isLive ? 1 : NotchStyle.dimmed)
    }

    private var left: some View {
        HStack(spacing: 6) {
            Group {
                if let mark = NotchMark.image {
                    Image(nsImage: mark).renderingMode(.template).resizable().interpolation(.high)
                } else {
                    Image(systemName: "cpu").resizable()
                }
            }
            .aspectRatio(contentMode: .fit)
            .frame(width: 14, height: 14)
            .foregroundStyle(NotchStyle.primary)
            .accessibilityHidden(true)

            if let rate = display.snapshot.outputRate.value {
                HStack(alignment: .firstTextBaseline, spacing: 3) {
                    Text(NotchFormat.rate(rate)).font(NotchStyle.number(13, .bold)).foregroundStyle(NotchStyle.primary)
                    Text("tok/s").font(.system(size: 11, weight: .medium)).foregroundStyle(NotchStyle.secondary)
                }
            } else {
                Text("— \(display.rateState)").font(.system(size: 11, weight: .medium)).foregroundStyle(NotchStyle.secondary)
            }
        }
        .lineLimit(1)
        .minimumScaleFactor(0.8)
    }
}

struct CollapsedNotch: View {
    let display: NotchDisplay
    let layout: NotchLayout
    let actions: NotchActions

    var body: some View {
        NotchWings(display: display, cameraWidth: layout.cameraWidth,
                   wingWidth: layout.hasNotch ? layout.wingWidth - 10 : nil)
            .padding(.leading, layout.collapsedShoulder + (layout.hasNotch ? 10 : 8))
            .padding(.trailing, layout.rightShoulder + (layout.hasNotch ? 0 : 8))
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .contentShape(Rectangle())
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(display.accessibilitySummary)
            .accessibilityHint("Opens or closes the LocalFleet monitor")
            .accessibilityAddTraits(.isButton)
            .accessibilityAction { actions.toggle() }
    }
}

struct NotchTabBar: View {
    @Binding var tab: NotchTab
    let wantsFocus: Bool
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 2) {
            ForEach(NotchTab.allCases) { t in
                Button { tab = t } label: {
                    Text(t.title)
                        .font(.system(size: 12, weight: t == tab ? .semibold : .medium))
                        .foregroundStyle(t == tab ? NotchStyle.primary : NotchStyle.secondary)
                        .frame(width: 92, height: 24)
                        .background(RoundedRectangle(cornerRadius: 6, style: .continuous)
                            .fill(t == tab ? Color.white.opacity(0.13) : .clear))
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(t.title)
                .accessibilityHint(t.hint)
                .accessibilityAddTraits(t == tab ? .isSelected : [])
            }
        }
        .padding(2)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Color.white.opacity(0.06)))
        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous)
            .strokeBorder(Color.white.opacity(focused ? 0.4 : 0), lineWidth: 1))
        .focusable()
        .focused($focused)
        .focusEffectDisabled()
        .onKeyPress(.leftArrow) { step(-1); return .handled }
        .onKeyPress(.rightArrow) { step(1); return .handled }
        .onAppear { focused = wantsFocus }
        .onChange(of: wantsFocus) { focused = wantsFocus }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Sections")
    }

    private func step(_ delta: Int) {
        let all = NotchTab.allCases
        guard let i = all.firstIndex(of: tab) else { return }
        tab = all[(i + delta + all.count) % all.count]
    }
}

// MARK: - Building blocks

/// A large figure over a small caption; an em dash with the reason as help when there is no value.
struct MetricColumn: View {
    let value: String?
    let label: String
    var reason: String?
    var note: String?
    var help: String?

    var body: some View {
        VStack(spacing: 2) {
            Text(value ?? "—")
                .font(NotchStyle.number(26))
                .foregroundStyle(value == nil ? NotchStyle.tertiary : NotchStyle.primary)
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            Text(label).font(.system(size: 11)).foregroundStyle(NotchStyle.secondary).lineLimit(1)
            if let note {
                Text(note).font(.system(size: 9)).foregroundStyle(NotchStyle.tertiary)
                    .lineLimit(2).multilineTextAlignment(.center)
            }
        }
        .frame(minWidth: 72)
        .help(help ?? (value == nil ? reason ?? "" : ""))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue([value ?? reason ?? "unavailable", note, help].compactMap { $0 }.joined(separator: ", "))
    }
}

struct ColumnDivider: View {
    var body: some View { Rectangle().fill(NotchStyle.hairline).frame(width: 1, height: 36) }
}

/// One labelled row: label (and optional muted lines) on the left, value block on the right.
struct NotchRow<Value: View>: View {
    let label: String
    var details: [String] = []
    @ViewBuilder var value: Value

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 1) {
                Text(label).font(.system(size: 12)).foregroundStyle(NotchStyle.primary)
                ForEach(details, id: \.self) { line in
                    Text(line).font(.system(size: 10).monospacedDigit()).foregroundStyle(NotchStyle.tertiary).lineLimit(1)
                }
            }
            Spacer(minLength: 12)
            value
        }
        .padding(.vertical, 5)
        .accessibilityElement(children: .combine)
    }
}

struct Hairline: View {
    var body: some View { Rectangle().fill(NotchStyle.hairline).frame(height: 1) }
}

struct ThinGauge: View {
    let fraction: Double

    var body: some View {
        GeometryReader { g in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.12))
                Capsule().fill(NotchStyle.primary.opacity(0.8))
                    .frame(width: g.size.width * min(max(fraction, 0), 1))
            }
        }
        .frame(height: 4)
        .accessibilityHidden(true)
    }
}

struct SectionLabel: View {
    let text: String
    var body: some View {
        Text(text).font(.system(size: 10, weight: .semibold)).foregroundStyle(NotchStyle.tertiary)
            .textCase(.uppercase)
    }
}

// MARK: - Charts

/// Maps time points to a rect: x over the 60 s that end at the newest point, y from 0 to `top`.
private struct SeriesScale {
    let end: Date
    let span: TimeInterval
    let top: Double

    /// Points inside the window, oldest first.
    static func window(_ points: [TimePoint]) -> [TimePoint] {
        guard let end = points.map(\.at).max() else { return [] }
        return points.filter { end.timeIntervalSince($0.at) <= NotchSnapshot.rateWindow }.sorted { $0.at < $1.at }
    }

    func point(_ p: TimePoint, in size: CGSize) -> CGPoint {
        let x = 1 - min(max(end.timeIntervalSince(p.at) / span, 0), 1)
        let y = top > 0 ? min(max(p.value / top, 0), 1) : 0
        return CGPoint(x: x * size.width, y: size.height - y * size.height)
    }

    func line(_ points: [TimePoint], in size: CGSize) -> Path {
        var path = Path()
        for (i, p) in points.enumerated() {
            let pt = point(p, in: size)
            if i == 0 { path.move(to: pt) } else { path.addLine(to: pt) }
        }
        return path
    }

    func area(_ points: [TimePoint], in size: CGSize) -> Path {
        guard let first = points.first, let last = points.last else { return Path() }
        var path = line(points, in: size)
        path.addLine(to: CGPoint(x: point(last, in: size).x, y: size.height))
        path.addLine(to: CGPoint(x: point(first, in: size).x, y: size.height))
        path.closeSubpath()
        return path
    }

    /// A rounded ceiling with three equal steps (1, 2, 2.5 or 5 × 10ⁿ).
    static func niceStep(for maxValue: Double) -> Double {
        let raw = max(maxValue, 1) / 3
        let magnitude = pow(10, floor(log10(raw)))
        let step = [1, 2, 2.5, 5, 10].map { $0 * magnitude }.first { $0 >= raw } ?? raw
        return step
    }
}

struct Sparkline: View {
    let points: [TimePoint]

    var body: some View {
        GeometryReader { g in
            let sorted = SeriesScale.window(points)
            if let last = sorted.last, sorted.count >= 2 {
                let top = max(sorted.map(\.value).max() ?? 0, 0)
                let scale = SeriesScale(end: last.at, span: NotchSnapshot.rateWindow, top: top > 0 ? top * 1.1 : 1)
                scale.line(sorted, in: g.size)
                    .stroke(NotchStyle.mint, style: StrokeStyle(lineWidth: 1.25, lineCap: .round, lineJoin: .round))
            }
        }
        .accessibilityHidden(true)
    }
}

/// Completed-output rate over the last 60 s: a thin mint line with a faint fill and real axes.
/// With fewer than two points it shows the reason instead of a line.
struct RateChart: View {
    let points: [TimePoint]
    let emptyReason: String

    private let gutter: CGFloat = 30

    var body: some View {
        let sorted = SeriesScale.window(points)
        VStack(spacing: 3) {
            if let last = sorted.last, sorted.count >= 2 {
                let step = SeriesScale.niceStep(for: sorted.map(\.value).max() ?? 0)
                let scale = SeriesScale(end: last.at, span: NotchSnapshot.rateWindow, top: step * 3)
                HStack(spacing: 6) {
                    VStack(alignment: .trailing, spacing: 0) {
                        ForEach((0...3).reversed(), id: \.self) { i in
                            Text(axis(step * Double(i)))
                                .font(.system(size: 9).monospacedDigit())
                                .foregroundStyle(NotchStyle.tertiary)
                                .frame(maxHeight: .infinity, alignment: i == 3 ? .top : i == 0 ? .bottom : .center)
                        }
                    }
                    .frame(width: gutter - 6)
                    GeometryReader { g in
                        ZStack {
                            ForEach(0...3, id: \.self) { i in
                                Path { p in
                                    let y = g.size.height * CGFloat(i) / 3
                                    p.move(to: CGPoint(x: 0, y: y))
                                    p.addLine(to: CGPoint(x: g.size.width, y: y))
                                }
                                .stroke(NotchStyle.hairline, lineWidth: 1)
                            }
                            scale.area(sorted, in: g.size)
                                .fill(LinearGradient(colors: [NotchStyle.mint.opacity(0.22), NotchStyle.mint.opacity(0.02)],
                                                     startPoint: .top, endPoint: .bottom))
                            scale.line(sorted, in: g.size)
                                .stroke(NotchStyle.mint, style: StrokeStyle(lineWidth: 1.5, lineCap: .round, lineJoin: .round))
                        }
                    }
                }
            } else {
                Text(emptyReason)
                    .font(.system(size: 11))
                    .foregroundStyle(NotchStyle.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            if sorted.count >= 2 {
                HStack {
                    Text("60s ago")
                    Spacer()
                    Text("now")
                }
                .font(.system(size: 9))
                .foregroundStyle(NotchStyle.tertiary)
                .padding(.leading, gutter)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Completed output rate, last 60 seconds")
        .accessibilityValue(summary(sorted))
    }

    private func axis(_ v: Double) -> String {
        v >= 1_000 ? NotchFormat.tokens(Int64(v)) : (v == v.rounded() ? String(format: "%.0f", v) : String(format: "%.1f", v))
    }

    private func summary(_ sorted: [TimePoint]) -> String {
        guard sorted.count >= 2, let last = sorted.last else { return emptyReason }
        let values = sorted.map(\.value)
        return "Latest \(NotchFormat.rate(last.value)) tokens per second, range \(NotchFormat.rate(values.min() ?? 0)) to \(NotchFormat.rate(values.max() ?? 0))"
    }
}

// MARK: - Tabs

struct ProductionTab: View {
    let display: NotchDisplay
    private var s: NotchSnapshot { display.snapshot }

    var body: some View {
        let headline = NotchDisplay.opacity(display.freshness)
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center, spacing: 14) {
                rate.opacity(headline)
                Spacer(minLength: 8)
                Group {
                    ColumnDivider()
                    MetricColumn(value: s.activeRequests.value.map(String.init), label: "Active", reason: s.activeRequests.reason)
                    ColumnDivider()
                    MetricColumn(value: s.apps.value.map(String.init), label: "Apps (labelled)", reason: s.apps.reason,
                                 help: appsHelp(s))
                }
                .opacity(NotchDisplay.opacity(display.engineFreshness))
            }

            RateChart(points: s.rateHistory, emptyReason: s.outputRate.reason ?? "No output history yet")
                .frame(height: 84)
                .opacity(headline)

            HStack(alignment: .top, spacing: 16) {
                totals.opacity(headline).frame(maxWidth: .infinity)
                if !s.sources.isEmpty {
                    Rectangle().fill(NotchStyle.hairline).frame(width: 1)
                    sources.frame(maxWidth: .infinity)
                }
            }
            .fixedSize(horizontal: false, vertical: true)

            if let note = s.usageNote {
                Text(note).font(.system(size: 10)).foregroundStyle(NotchStyle.tertiary).lineLimit(1).help(note)
            }
        }
    }

    private var caption: String {
        s.productionSource.isEmpty ? "Completed output, last 60 s" : "Completed output, last 60 s · \(s.productionSource)"
    }

    private var rate: some View {
        VStack(alignment: .leading, spacing: 1) {
            if let rate = s.outputRate.value {
                HStack(alignment: .firstTextBaseline, spacing: 5) {
                    Text(NotchFormat.rate(rate)).font(NotchStyle.number(40)).foregroundStyle(NotchStyle.primary)
                    Text("tok/s").font(.system(size: 16, weight: .medium)).foregroundStyle(NotchStyle.secondary)
                }
            } else {
                Text("—").font(NotchStyle.number(40)).foregroundStyle(NotchStyle.tertiary)
            }
            Text(caption).font(.system(size: 11)).foregroundStyle(NotchStyle.secondary).lineLimit(1)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(caption)
        .accessibilityValue(s.outputRate.value.map { "\(NotchFormat.rate($0)) tokens per second" }
            ?? s.outputRate.reason ?? "unavailable")
    }

    private var totals: some View {
        VStack(spacing: 0) {
            NotchRow(label: "Produced today",
                     details: [s.productionSource.isEmpty ? nil : s.productionSource, inputDetail].compactMap { $0 }) {
                VStack(alignment: .trailing, spacing: 1) {
                    if let out = s.outputToday.value {
                        Text("\(NotchFormat.tokens(out)) tokens").font(NotchStyle.number(13)).foregroundStyle(NotchStyle.primary)
                        if let coverage { Text(coverage).font(.system(size: 10).monospacedDigit()).foregroundStyle(NotchStyle.tertiary) }
                    } else {
                        Text(s.outputToday.reason ?? "").font(.system(size: 11)).foregroundStyle(NotchStyle.secondary)
                            .lineLimit(1)
                    }
                }
            }
            Hairline()
            NotchRow(label: "By midnight") {
                VStack(alignment: .trailing, spacing: 1) {
                    if let e = s.estimate.value {
                        Text("\(NotchFormat.range(e)) est.").font(NotchStyle.number(13)).foregroundStyle(NotchStyle.primary)
                        Text("Based on today's pace").font(.system(size: 10)).foregroundStyle(NotchStyle.tertiary)
                    } else {
                        Text(s.estimate.reason ?? "").font(.system(size: 11)).foregroundStyle(NotchStyle.secondary)
                            .lineLimit(1)
                    }
                }
            }
        }
    }

    /// Each source on its own row with its own freshness; never summed (a request can be in both).
    private var sources: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionLabel(text: "Sources").padding(.bottom, 2)
            ForEach(s.sources) { src in
                Hairline()
                SourceRow(source: src)
                    .opacity(NotchDisplay.opacity(display.freshness(ofSource: src.id)))
            }
        }
    }

    private var coverage: String? {
        guard let start = s.coverageStart else { return nil }
        let gaps = s.coverageGaps > 0 ? " · \(plural(s.coverageGaps, "gap"))" : ""
        return "since \(NotchFormat.time(start))\(gaps)"
    }

    private var inputDetail: String? {
        let parts = [s.promptToday.value.map { "prompt \(NotchFormat.tokens($0))" },
                     s.cachedToday.value.map { "cached \(NotchFormat.tokens($0))" }].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// Apps are client-supplied labels, not verified identities.
@MainActor func appsHelp(_ s: NotchSnapshot) -> String {
    [s.apps.value.map { "\($0) labelled apps" } ?? s.apps.reason, s.appsNote].compactMap { $0 }.joined(separator: ". ")
}

struct SourceRow: View {
    let source: ProductionSource

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(source.name).font(.system(size: 12)).foregroundStyle(NotchStyle.primary).lineLimit(1)
                Spacer(minLength: 6)
                if let r = source.rate.value {
                    Text("\(NotchFormat.rate(r)) tok/s").font(NotchStyle.number(12)).foregroundStyle(NotchStyle.primary)
                } else {
                    Text(source.rate.reason ?? "").font(.system(size: 10)).foregroundStyle(NotchStyle.secondary)
                        .lineLimit(1).truncationMode(.tail)
                }
            }
            Text(secondary).font(.system(size: 10).monospacedDigit()).foregroundStyle(NotchStyle.tertiary).lineLimit(1)
        }
        .padding(.vertical, 4)
        .help(([source.scope] + [source.note].compactMap { $0 }).joined(separator: "\n"))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(source.name)
        .accessibilityValue(([source.rate.value.map { "\(NotchFormat.rate($0)) tokens per second" } ?? source.rate.reason,
                              secondary, source.scope, source.note].compactMap { $0 }).joined(separator: ", "))
    }

    private var secondary: String {
        [source.today.value.map { "\(NotchFormat.tokens($0)) today" } ?? source.today.reason,
         source.prompt.value.map { "prompt \(NotchFormat.tokens($0))" },
         source.cached.value.map { "cached \(NotchFormat.tokens($0))" }].compactMap { $0 }.joined(separator: " · ")
    }
}

struct DemandTab: View {
    let display: NotchDisplay
    private var s: NotchSnapshot { display.snapshot }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 0) {
                MetricColumn(value: s.activeRequests.value.map(String.init), label: "Active", reason: s.activeRequests.reason)
                    .frame(maxWidth: .infinity)
                MetricColumn(value: s.slots.value.map { "\($0.occupied)/\($0.capacity)" }, label: "Slots in use",
                             reason: s.slots.reason, note: s.slots.value == nil ? s.slots.reason : "ready models")
                    .frame(maxWidth: .infinity)
                MetricColumn(value: "\(s.loadingModels.count)", label: "Loading")
                    .frame(maxWidth: .infinity)
                MetricColumn(value: s.rejections.value.map { "\($0)" }, label: "Rejections",
                             reason: s.rejections.reason, note: "since engine start, this node")
                    .frame(maxWidth: .infinity)
                MetricColumn(value: s.apps.value.map(String.init), label: "Apps (labelled)", reason: s.apps.reason,
                             help: appsHelp(s))
                    .frame(maxWidth: .infinity)
            }
            .opacity(NotchDisplay.opacity(display.engineFreshness))

            HStack(alignment: .top, spacing: 18) {
                models.frame(maxWidth: .infinity, alignment: .leading)
                VStack(alignment: .leading, spacing: 10) {
                    nodes
                    if !s.appLabels.isEmpty || s.labelledActive != nil || s.unidentifiedActive != nil { apps }
                }
                .frame(width: 190, alignment: .leading)
                .opacity(NotchDisplay.opacity(display.engineFreshness))
            }
        }
    }

    private var models: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionLabel(text: "Models").padding(.bottom, 2)
            if s.models.isEmpty {
                Hairline()
                Text(s.loadingModels.isEmpty ? "No model loaded" : "Loading " + s.loadingModels.joined(separator: ", "))
                    .font(.system(size: 11)).foregroundStyle(NotchStyle.secondary).lineLimit(1).padding(.vertical, 5)
            }
            ForEach(s.models) { line in
                Hairline()
                ModelRow(line: line)
                    .opacity(NotchDisplay.opacity(display.freshness(ofSource: line.source)))
            }
        }
    }

    /// Client-supplied labels: "labelled", never a verified identity.
    private var apps: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionLabel(text: "Labelled apps").padding(.bottom, 2)
            ForEach(s.appLabels, id: \.self) { label in
                Hairline()
                Text(label).font(.system(size: 12)).foregroundStyle(NotchStyle.primary).lineLimit(1)
                    .padding(.vertical, 4)
            }
            let inFlight = [s.labelledActive.map { "\($0) labelled" }, s.unidentifiedActive.map { "\($0) unidentified" }]
                .compactMap { $0 }
            if !inFlight.isEmpty {
                Hairline()
                Text(inFlight.joined(separator: " · ") + " in flight")
                    .font(.system(size: 10).monospacedDigit()).foregroundStyle(NotchStyle.tertiary).padding(.vertical, 4)
            }
        }
        .help(s.appsNote ?? "")
        .accessibilityElement(children: .combine)
    }

    private var nodes: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionLabel(text: "Nodes").padding(.bottom, 2)
            if s.nodes.isEmpty {
                Hairline()
                Text(s.activeRequests.reason ?? "No node reported yet")
                    .font(.system(size: 11)).foregroundStyle(NotchStyle.secondary).lineLimit(2).padding(.vertical, 5)
            }
            ForEach(s.nodes) { node in
                Hairline()
                NodeRow(node: node)
            }
        }
    }
}

struct ModelRow: View {
    let line: ModelLine

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("\(line.model) · \(line.host)").font(.system(size: 12)).foregroundStyle(NotchStyle.primary)
                    .lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 8)
                Text(load).font(NotchStyle.number(12, .medium)).foregroundStyle(NotchStyle.primary).lineLimit(1)
            }
            Text(([line.state] + [line.detail].compactMap { $0 }).joined(separator: " · "))
                .font(.system(size: 10).monospacedDigit()).foregroundStyle(NotchStyle.tertiary).lineLimit(1)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(line.model) on \(line.host)")
        .accessibilityValue(([line.state, load] + [line.detail].compactMap { $0 }).joined(separator: ", "))
    }

    /// "active/capacity" when the capacity is known, else "n active"; queued requests in brackets.
    private var load: String {
        var text = line.capacity.map { "\(line.active)/\($0)" } ?? "\(line.active) active"
        if let waiting = line.waiting, waiting > 0 { text += " (+\(waiting) waiting)" }
        return text
    }
}

struct NodeRow: View {
    let node: NodeLine

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            HStack(spacing: 6) {
                Circle().fill(node.reachable ? NotchStyle.primary : NotchStyle.tertiary)
                    .frame(width: 5, height: 5)
                    .accessibilityHidden(true)
                Text(node.host).font(.system(size: 12)).foregroundStyle(NotchStyle.primary).lineLimit(1)
                Spacer(minLength: 6)
                if node.reachable {
                    Text("\(node.activeRequests) active").font(NotchStyle.number(12, .medium)).foregroundStyle(NotchStyle.primary)
                }
            }
            Text(status).font(.system(size: 10)).foregroundStyle(NotchStyle.tertiary).padding(.leading, 11)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(node.host)
        .accessibilityValue(node.reachable ? "\(node.activeRequests) active, \(status)" : status)
    }

    private var status: String {
        guard node.reachable else { return "unreachable" }
        switch node.usageReported {
        case nil: return "update pending"
        case true?: return "usage reporting"
        case false?: return "usage partial"
        }
    }
}

struct EnergyTab: View {
    let display: NotchDisplay
    private var s: NotchSnapshot { display.snapshot }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 0) {
                SectionLabel(text: "Power").padding(.bottom, 2)
                ForEach(s.power) { r in
                    Hairline()
                    PowerRow(reading: r)
                }
                if !s.powerCoverage.isEmpty {
                    Text(s.powerCoverage)
                        .font(.system(size: 10)).foregroundStyle(NotchStyle.tertiary)
                        .lineLimit(2).padding(.top, 4)
                }
            }
            .opacity(NotchDisplay.opacity(display.engineFreshness))

            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .firstTextBaseline) {
                    SectionLabel(text: "Memory")
                    Spacer()
                    if let pressure = s.omlxPressure {
                        Text("oMLX memory pressure: \(pressure)")
                            .font(.system(size: 10)).foregroundStyle(NotchStyle.secondary)
                            .opacity(NotchDisplay.opacity(display.omlxFreshness))
                    }
                }
                .padding(.bottom, 2)
                if s.memory.isEmpty {
                    Text("Not reported").font(.system(size: 11)).foregroundStyle(NotchStyle.secondary).padding(.vertical, 4)
                }
                ForEach(s.memory) { m in
                    Hairline()
                    MemoryRow(gauge: m)
                        .opacity(NotchDisplay.opacity(display.freshness(of: m)))
                }
            }
        }
    }
}

struct PowerRow: View {
    let reading: PowerReading

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            VStack(alignment: .leading, spacing: 1) {
                Text("\(reading.host) · \(reading.device)").font(.system(size: 12)).foregroundStyle(NotchStyle.primary)
                    .lineLimit(1).truncationMode(.middle)
                Text(detail).font(.system(size: 10).monospacedDigit()).foregroundStyle(NotchStyle.tertiary).lineLimit(1)
            }
            Spacer(minLength: 12)
            if let w = reading.watts {
                Text(NotchFormat.watts(w)).font(NotchStyle.number(13)).foregroundStyle(NotchStyle.primary)
            } else {
                Text(reading.reason ?? "Not measured").font(.system(size: 11)).foregroundStyle(NotchStyle.secondary)
                    .lineLimit(2).multilineTextAlignment(.trailing).frame(maxWidth: 240, alignment: .trailing)
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }

    private var detail: String {
        ([reading.scope] + [reading.wattHoursToday.map { "\(NotchFormat.wattHours($0)) today" }].compactMap { $0 })
            .joined(separator: " · ")
    }
}

struct MemoryRow: View {
    let gauge: MemoryGauge

    var body: some View {
        HStack(spacing: 10) {
            Text("\(gauge.label) · \(gauge.host)").font(.system(size: 12)).foregroundStyle(NotchStyle.primary)
                .lineLimit(1).truncationMode(.middle)
            Spacer(minLength: 8)
            ThinGauge(fraction: gauge.totalBytes > 0 ? Double(gauge.usedBytes) / Double(gauge.totalBytes) : 0)
                .frame(width: 140)
            Text(NotchFormat.memory(gauge.usedBytes, gauge.totalBytes))
                .font(NotchStyle.number(12, .medium)).foregroundStyle(NotchStyle.primary)
                .frame(width: 96, alignment: .trailing)
        }
        .padding(.vertical, 5)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(gauge.label) on \(gauge.host)")
        .accessibilityValue("\(NotchFormat.memory(gauge.usedBytes, gauge.totalBytes)) used")
    }
}


// MARK: - Footer

struct NotchFooter: View {
    let display: NotchDisplay
    let actions: NotchActions
    private var s: NotchSnapshot { display.snapshot }

    var body: some View {
        HStack(spacing: 12) {
            power.frame(maxWidth: .infinity, alignment: .leading)
                .opacity(NotchDisplay.opacity(display.engineFreshness))
            memory.frame(maxWidth: .infinity, alignment: .center)
                .opacity(s.memory.first.map { NotchDisplay.opacity(display.freshness(of: $0)) } ?? 1)
            Button { actions.details() } label: {
                Text("Details ↗").font(.system(size: 12, weight: .medium)).foregroundStyle(NotchStyle.primary)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Details")
            .accessibilityHint("Opens LocalFleet settings")
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
    }

    @ViewBuilder private var power: some View {
        if let r = s.power.first(where: { $0.watts != nil }), let w = r.watts {
            HStack(alignment: .firstTextBaseline, spacing: 5) {
                Image(systemName: "bolt.fill").font(.system(size: 11)).foregroundStyle(NotchStyle.secondary)
                    .accessibilityHidden(true)
                Text(Self.sensorLabel(r)).font(.system(size: 11)).foregroundStyle(NotchStyle.secondary)
                Text(NotchFormat.watts(w)).font(NotchStyle.number(12)).foregroundStyle(NotchStyle.primary)
                Text(r.host).font(.system(size: 11)).foregroundStyle(NotchStyle.tertiary)
            }
            .lineLimit(1)
            .help("\(r.host) · \(r.device) · \(r.scope)")
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Power, \(r.device) on \(r.host)")
            .accessibilityValue("\(NotchFormat.watts(w)), \(r.scope)")
        } else {
            HStack(spacing: 5) {
                Image(systemName: "bolt.slash").font(.system(size: 11)).accessibilityHidden(true)
                Text("Power unavailable").font(.system(size: 11))
            }
            .foregroundStyle(NotchStyle.tertiary)
            .help(s.power.first?.reason ?? s.powerCoverage)
            .accessibilityElement(children: .combine)
        }
    }

    @ViewBuilder private var memory: some View {
        if let m = s.memory.first {
            HStack(spacing: 8) {
                ThinGauge(fraction: m.totalBytes > 0 ? Double(m.usedBytes) / Double(m.totalBytes) : 0)
                    .frame(width: 70)
                Text(NotchFormat.memory(m.usedBytes, m.totalBytes))
                    .font(NotchStyle.number(11, .medium)).foregroundStyle(NotchStyle.secondary).lineLimit(1)
            }
            .help("\(m.label) · \(m.host)")
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(m.label) on \(m.host)")
            .accessibilityValue("\(NotchFormat.memory(m.usedBytes, m.totalBytes)) used")
        } else {
            Text("Memory unavailable").font(.system(size: 11)).foregroundStyle(NotchStyle.tertiary)
        }
    }

    /// "GPU" / "SoC" style label for a short scope; the device name otherwise.
    static func sensorLabel(_ r: PowerReading) -> String {
        let scope = r.scope.trimmingCharacters(in: .whitespaces)
        switch scope.lowercased() {
        case "gpu": return "GPU"
        case "soc": return "SoC"
        case "cpu": return "CPU"
        case "system": return "System"
        default: return scope.count <= 8 && !scope.contains(" ") && !scope.isEmpty ? scope : r.device
        }
    }
}
