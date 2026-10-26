import AppKit
import SwiftUI

// MARK: - Inputs

enum NotchTab: String, CaseIterable, Identifiable, Sendable {
    case today, now, energy

    var id: String { rawValue }
    var title: String {
        switch self {
        case .today: "Today"
        case .now: "Now"
        case .energy: "Energy"
        }
    }
    var hint: String {
        switch self {
        case .today: "Shows today's completed jobs by capability"
        case .now: "Shows requests, slots, rejections, models and nodes"
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
    /// The activity report behind the job counts (trigger and Production).
    var activityFreshness: Freshness

    init(snapshot: NotchSnapshot, freshness: Freshness, activeHistory: [TimePoint],
         engineFreshness: Freshness? = nil, omlxFreshness: Freshness? = nil, activityFreshness: Freshness? = nil) {
        self.snapshot = snapshot
        self.freshness = freshness
        self.activeHistory = activeHistory
        self.engineFreshness = engineFreshness ?? freshness
        self.omlxFreshness = omlxFreshness ?? freshness
        self.activityFreshness = activityFreshness ?? engineFreshness ?? freshness
    }

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
    var expandPreviewDetails = true

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

    /// Whole numbers from 10 up, one decimal below 10, two below 1; a positive rate never reads 0.
    static func rate(_ r: Double) -> String {
        guard r.isFinite else { return "—" }
        if r == 0 { return "0" }
        if r > 0, r < 0.005 { return "<0.01" }
        let digits = abs(r) >= 10 ? 0 : abs(r) >= 1 ? 1 : 2
        return r.formatted(.number.precision(.fractionLength(digits)))
    }

    /// Counts (jobs, vectors, steps): exact below 1,000, then 1.2k / 842k / 1.2M.
    static func count(_ n: Int64) -> String { tokens(n) }

    /// Audio length: "42 s", "12 min 5 s", "1 h 12 min".
    static func duration(_ seconds: Double) -> String {
        if seconds < 10 { return String(format: "%.1f s", seconds) }
        if seconds < 60 { return String(format: "%.0f s", seconds) }
        let total = Int(seconds.rounded())
        if total < 3_600 { return total % 60 == 0 ? "\(total / 60) min" : "\(total / 60) min \(total % 60) s" }
        return "\(total / 3_600) h \((total % 3_600) / 60) min"
    }

    /// One work unit: "1.2M output tokens", "12 min 5 s of audio".
    static func unit(_ name: String, _ value: Double) -> String {
        let label = ActivityCatalog.unitLabel(name)
        if name == "audio_seconds" { return "\(duration(value)) \(label)" }
        return "\(count(Int64(value.rounded()))) \(label)"
    }
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
    /// Short state for the trigger when there is no job count to show.
    var jobsState: String {
        if case .offline = activityFreshness { return "offline" }
        if case .stale = activityFreshness { return "stale" }
        if case .collecting = snapshot.activity.jobsToday { return "waiting" }
        return "no data"
    }

    /// Today reports the activity report; Demand and Energy report the fleet engine.
    func statusLine(for tab: NotchTab) -> String? {
        let (f, prefix) = tab == .today ? (activityFreshness, "Activity report ") : (engineFreshness, "Fleet ")
        switch f {
        case .live: return nil
        case .stale(let since): return "\(prefix)stale since \(NotchFormat.time(since))"
        case .offline(let why): return "\(prefix)offline · \(why)"
        }
    }

    var accessibilitySummary: String {
        let a = snapshot.activity
        let jobs = a.jobsToday.value.map { "\(plural(Int(clamping: $0), "completed job")) today" }
            ?? "completed jobs today: \(a.jobsToday.reason ?? "unavailable")"
        let recent = a.recentJob ? ", a job finished in the last minute" : ""
        let partial = a.partial ? ", partial coverage" : ""
        let active = snapshot.activeRequests.value.map { plural($0, "active request") }
            ?? "active requests \(snapshot.activeRequests.reason ?? "unavailable")"
        let fresh = switch activityFreshness {
        case .live: "activity live"
        case .stale(let since): "activity stale since \(NotchFormat.time(since))"
        case .offline(let why): "activity offline: \(why)"
        }
        return "LocalFleet: \(jobs)\(recent)\(partial), \(active), \(fresh)"
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
            NotchTabBar(tab: $tab, wantsFocus: pinned)
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
            case .today: TodayTab(display: display, expandAll: layout.isPreview && layout.expandPreviewDetails)
            case .now: NowTab(display: display)
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

/// Mark + today's completed jobs (and a dot when a job finished in the last minute), then the
/// camera housing, which stays empty at the right end of the shape.
struct NotchWings: View {
    let display: NotchDisplay
    let cameraWidth: CGFloat
    var wingWidth: CGFloat?

    var body: some View {
        HStack(spacing: 0) {
            left.frame(maxWidth: wingWidth ?? .infinity, alignment: .leading)
            Color.clear.frame(width: cameraWidth)
        }
        .opacity(NotchDisplay.opacity(display.activityFreshness))
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

            if let n = display.snapshot.activity.jobsToday.value {
                HStack(alignment: .firstTextBaseline, spacing: 3) {
                    Text(NotchFormat.count(n)).font(NotchStyle.number(13, .bold)).foregroundStyle(NotchStyle.primary)
                    Text(n == 1 ? "job" : "jobs").font(.system(size: 11, weight: .medium)).foregroundStyle(NotchStyle.secondary)
                }
                if display.snapshot.activity.recentJob {
                    Circle().fill(NotchStyle.mint).frame(width: 5, height: 5).accessibilityHidden(true)
                }
            } else {
                Text("— \(display.jobsState)").font(.system(size: 11, weight: .medium)).foregroundStyle(NotchStyle.secondary)
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

// MARK: - Tabs

/// What Today shows, as plain values: the view draws only this, and tests read it. One headline
/// (completed jobs), one line per capability with completed jobs and its key unit, the activity
/// chart's time labels and one coverage line. Everything else is in Diagnostics.
struct TodayPresentation: Equatable {
    struct Line: Identifiable, Equatable {
        var id: String
        var title: String
        var value: String
        var detail: String?
    }

    struct AxisMark: Equatable {
        var label: String
        /// Position from local midnight (0) to now (1).
        var at: Double
    }

    var headline: String
    var headlineUnit: String
    var breakdown: [Line]
    var axis: [AxisMark]
    var footer: String?
    /// All 13 families (and any unrecognised one): completed jobs, or why there are none.
    var all: [Line]

    init(_ a: ActivitySummary, calendar: Calendar = .current) {
        if let n = a.jobsToday.value {
            headline = NotchFormat.count(n)
            headlineUnit = n == 1 ? "completed job" : "completed jobs"
        } else {
            headline = "—"
            headlineUnit = a.jobsToday.reason ?? "unavailable"
        }
        breakdown = a.rows.filter { $0.succeeded > 0 }.map { row in
            let detail = row.keyUnit.map { unit in
                Self.unit(unit) + (row.unitsUnavailable > 0 && unit.value != nil ? " · partial" : "")
            }
            return Line(id: row.id, title: row.title, value: Self.jobs(row.succeeded), detail: detail)
        }
        all = a.rows.map { row in
            let value = row.succeeded > 0 ? Self.jobs(row.succeeded) : row.used ? "no completed jobs" : row.idleStatus
            return Line(id: row.id, title: row.title, value: value)
        }
        axis = Self.axis(since: a.dayStart, until: a.until, calendar: calendar)
        footer = Self.footer(a)
    }

    /// Every string the primary Today view draws (the disclosures excluded).
    var primaryTexts: [String] {
        ["\(headline) \(headlineUnit)"] + breakdown.flatMap { [$0.title, $0.value] + [$0.detail].compactMap { $0 } }
            + axis.map(\.label) + [footer].compactMap { $0 }
    }

    static func jobs(_ n: Int64) -> String { "\(NotchFormat.count(n)) \(n == 1 ? "job" : "jobs")" }

    static func unit(_ u: UnitValue) -> String {
        u.value.map { NotchFormat.unit(u.name, $0) } ?? "\(ActivityCatalog.unitNoun(u.name)) unavailable"
    }

    /// Local midnight, the 06:00 / 12:00 / 18:00 marks that fit, and "now".
    static func axis(since: Date?, until: Date?, calendar: Calendar) -> [AxisMark] {
        guard let since, let until, until > since else { return [] }
        let span = until.timeIntervalSince(since)
        let marks: [AxisMark] = [6, 12, 18].compactMap { hour in
            guard let mark = calendar.date(bySettingHour: hour, minute: 0, second: 0, of: since) else { return nil }
            let at = mark.timeIntervalSince(since) / span
            return at > 0.12 && at < 0.85 ? AxisMark(label: NotchFormat.time(mark), at: at) : nil
        }
        return [AxisMark(label: NotchFormat.time(since), at: 0)] + marks + [AxisMark(label: "now", at: 1)]
    }

    static func footer(_ a: ActivitySummary) -> String? {
        if let first = a.partialReasons.first {
            return "Partial · \(first)" + (a.partialReasons.count > 1 ? " · more in Diagnostics" : "")
        }
        let reporting = a.coverage.filter(\.reporting)
        guard let since = reporting.compactMap(\.since).min() else { return nil }
        let gaps = reporting.reduce(0) { $0 + $1.gaps }
        if Set(reporting.compactMap(\.since)).count > 1 {
            return "Coverage varies by node · see Diagnostics"
        }
        let nodes = reporting.count == 1 ? "1 node" : "all \(reporting.count) nodes"
        return "Covered since \(NotchFormat.time(since)) on \(nodes)" + (gaps > 0 ? " · \(plural(gaps, "gap"))" : "")
    }
}

/// One level deeper than Today: chat tokens, failed and cancelled jobs, per-node coverage and each
/// token source (the fleet and oMLX, never summed), with oMLX's measured phases and memory
/// pressure. Nothing here is estimated except the labelled by-midnight pace on a source.
struct DiagnosticsPresentation: Equatable {
    var chatTokens: [String]
    var failures: [String]
    var coverage: [String]
    var sources: [ProductionSource]
    var omlxModels: [ModelLine]
    var omlxPressure: String?

    init(_ s: NotchSnapshot) {
        let a = s.activity
        if let chat = a.rows.first(where: { $0.id == "chat" }), chat.succeeded > 0 {
            var lines: [String] = []
            if let out = chat.unit("output_tokens") { lines.append(TodayPresentation.unit(out)) }
            if let reasoning = chat.unit("reasoning_tokens")?.value {
                lines.append("of which reasoning \(NotchFormat.count(Int64(reasoning.rounded())))")
            }
            if let prompt = chat.unit("prompt_tokens") { lines.append(TodayPresentation.unit(prompt)) }
            if chat.unitsUnavailable > 0 { lines.append("units missing for \(plural(Int(clamping: chat.unitsUnavailable), "job"))") }
            chatTokens = lines
        } else {
            chatTokens = []
        }
        failures = a.rows.compactMap { row in
            let parts = [row.failed > 0 ? "\(row.failed) failed" : nil, row.cancelled > 0 ? "\(row.cancelled) cancelled" : nil]
                .compactMap { $0 }
            return parts.isEmpty ? nil : "\(row.title): " + parts.joined(separator: ", ")
        }
        let hostReasons = Set(a.coverage.map(\.host))
        coverage = a.coverage.map(Self.coverageLine)
            + a.partialReasons.filter { reason in !hostReasons.contains { reason.hasPrefix($0) } }
        sources = s.sources
        omlxModels = s.models.filter { $0.source == NotchSnapshot.omlxSourceName }
        omlxPressure = s.omlxPressure
    }

    static func coverageLine(_ c: HostCoverage) -> String {
        if let note = c.notReporting { return "\(c.host) \(note)" }
        guard let since = c.since else { return "\(c.host) no coverage today" }
        return (["\(c.host) since \(NotchFormat.time(since))", c.gaps > 0 ? plural(c.gaps, "gap") : nil, c.incomplete]
            as [String?]).compactMap { $0 }.joined(separator: " · ")
    }
}

struct TodayTab: View {
    let display: NotchDisplay
    @State private var showAll: Bool
    @State private var showDiagnostics: Bool

    /// `expandAll` opens both disclosures (offscreen renders show every row).
    init(display: NotchDisplay, expandAll: Bool = false) {
        self.display = display
        _showAll = State(initialValue: expandAll)
        _showDiagnostics = State(initialValue: expandAll)
    }

    var body: some View {
        let a = display.snapshot.activity
        let p = TodayPresentation(a)
        let dim = NotchDisplay.opacity(display.activityFreshness)
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(p.headline).font(NotchStyle.number(28)).foregroundStyle(a.jobsToday.value == nil ? NotchStyle.tertiary : NotchStyle.primary)
                Text(p.headlineUnit).font(.system(size: 13, weight: .medium)).foregroundStyle(NotchStyle.secondary).lineLimit(2)
            }
            .opacity(dim)
            .accessibilityElement(children: .combine)

            if !p.breakdown.isEmpty {
                VStack(spacing: 0) {
                    ForEach(Array(p.breakdown.enumerated()), id: \.element.id) { i, line in
                        if i > 0 { Hairline() }
                        TodayLine(line: line)
                    }
                }
                .opacity(dim)
            }

            ActivityChart(series: a.series, axis: p.axis, since: a.dayStart, until: a.until,
                          emptyReason: a.jobsToday.reason ?? "No coverage today yet")
                .frame(height: 56)
                .opacity(dim)

            if let footer = p.footer {
                Text(footer).font(.system(size: 10)).foregroundStyle(a.partial ? NotchStyle.secondary : NotchStyle.tertiary)
                    .lineLimit(2)
            }

            VStack(alignment: .leading, spacing: 0) {
                Hairline()
                DisclosureRow(title: "Show all capabilities", isOn: $showAll)
                if showAll {
                    ForEach(p.all) { line in
                        TodayLine(line: line, quiet: !a.rows.contains { $0.id == line.id && $0.succeeded > 0 })
                    }
                    .opacity(dim)
                }
                Hairline()
                DisclosureRow(title: "Diagnostics", isOn: $showDiagnostics)
                if showDiagnostics { DiagnosticsView(display: display).padding(.bottom, 4) }
            }
        }
    }
}

/// A title on the left; a muted detail and the value on the right, on one line.
struct TodayLine: View {
    let line: TodayPresentation.Line
    var quiet = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(line.title).font(.system(size: 12)).foregroundStyle(quiet ? NotchStyle.secondary : NotchStyle.primary)
            Spacer(minLength: 8)
            if let detail = line.detail {
                Text(detail).font(.system(size: 11).monospacedDigit()).foregroundStyle(quiet ? NotchStyle.tertiary : NotchStyle.secondary).lineLimit(1)
            }
            Text(line.value)
                .font(quiet ? .system(size: 11) : NotchStyle.number(12))
                .foregroundStyle(quiet ? NotchStyle.tertiary : NotchStyle.primary)
                .lineLimit(1)
        }
        .padding(.vertical, 5)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(line.title)
        .accessibilityValue([line.value, line.detail].compactMap { $0 }.joined(separator: ", "))
    }
}

struct DisclosureRow: View {
    let title: String
    @Binding var isOn: Bool

    var body: some View {
        Button { isOn.toggle() } label: {
            HStack(spacing: 6) {
                Image(systemName: isOn ? "chevron.down" : "chevron.right").font(.system(size: 9, weight: .semibold))
                    .frame(width: 10)
                Text(title).font(.system(size: 12))
                Spacer(minLength: 0)
            }
            .foregroundStyle(NotchStyle.secondary)
            .padding(.vertical, 6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .accessibilityValue(isOn ? "expanded" : "collapsed")
    }
}

struct DiagnosticsView: View {
    let display: NotchDisplay

    var body: some View {
        let d = DiagnosticsPresentation(display.snapshot)
        VStack(alignment: .leading, spacing: 10) {
            section("Chat tokens today", d.chatTokens, empty: "No chat jobs today")
                .opacity(NotchDisplay.opacity(display.activityFreshness))
            section("Failed and cancelled", d.failures, empty: "None today")
                .opacity(NotchDisplay.opacity(display.activityFreshness))
            section("Coverage", d.coverage, empty: "No activity report yet")
            VStack(alignment: .leading, spacing: 0) {
                SectionLabel(text: "Token sources").padding(.bottom, 2)
                if d.sources.isEmpty {
                    Text("No token source reports usage yet").font(.system(size: 11)).foregroundStyle(NotchStyle.secondary)
                }
                ForEach(d.sources) { src in
                    Hairline()
                    SourceRow(source: src)
                        .opacity(NotchDisplay.opacity(display.freshness(ofSource: src.id)))
                }
                ForEach(d.omlxModels) { m in
                    Text(([m.model, m.state] + [m.detail].compactMap { $0 }).joined(separator: " · "))
                        .font(.system(size: 10).monospacedDigit()).foregroundStyle(NotchStyle.tertiary).lineLimit(1)
                        .padding(.leading, 8)
                }
                .opacity(NotchDisplay.opacity(display.omlxFreshness))
                if let pressure = d.omlxPressure {
                    Text("oMLX memory pressure: \(pressure)").font(.system(size: 10)).foregroundStyle(NotchStyle.tertiary)
                        .padding(.leading, 8).opacity(NotchDisplay.opacity(display.omlxFreshness))
                }
                if let note = display.snapshot.usageNote {
                    Text(note).font(.system(size: 10)).foregroundStyle(NotchStyle.tertiary).lineLimit(2).padding(.top, 2)
                }
            }
        }
        .padding(.leading, 16)
    }

    private func section(_ title: String, _ lines: [String], empty: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            SectionLabel(text: title).padding(.bottom, 1)
            ForEach(lines.isEmpty ? [empty] : lines, id: \.self) { line in
                Text(line).font(.system(size: 11).monospacedDigit())
                    .foregroundStyle(lines.isEmpty ? NotchStyle.tertiary : NotchStyle.secondary)
                    .lineLimit(2)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// Completed jobs per 15-minute bucket from local midnight to now, in the single accent colour.
/// A bucket no node covered draws nothing (unknown); a covered bucket with no jobs draws a faint
/// baseline.
struct ActivityChart: View {
    let series: [ActivityBucket]
    let axis: [TodayPresentation.AxisMark]
    let since: Date?
    let until: Date?
    let emptyReason: String

    var body: some View {
        let covered = series.compactMap(\.jobs)
        VStack(spacing: 3) {
            if covered.isEmpty || since == nil || until == nil {
                Text(emptyReason)
                    .font(.system(size: 11))
                    .foregroundStyle(NotchStyle.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let since, let until {
                let peak = max(covered.max() ?? 0, 1)
                let span = max(until.timeIntervalSince(since), 1)
                Text("\(NotchFormat.count(peak)) per 15 min")
                    .font(.system(size: 9).monospacedDigit()).foregroundStyle(NotchStyle.tertiary)
                    .frame(maxWidth: .infinity, alignment: .trailing)
                    .frame(height: 11)
                GeometryReader { g in
                    let w = g.size.width * CGFloat(Double(LocalDay.bucketMs) / 1_000 / span)
                    let x = { (b: ActivityBucket) in g.size.width * CGFloat(b.start.timeIntervalSince(since) / span) }
                    ZStack(alignment: .topTrailing) {
                        Path { p in
                            p.move(to: CGPoint(x: 0, y: g.size.height))
                            p.addLine(to: CGPoint(x: g.size.width, y: g.size.height))
                        }
                        .stroke(NotchStyle.hairline, lineWidth: 1)
                        Path { p in
                            for b in series where b.jobs == 0 {
                                p.addRect(CGRect(x: x(b), y: g.size.height - 1, width: w, height: 1))
                            }
                        }
                        .fill(NotchStyle.mint.opacity(0.3))
                        Path { p in
                            for b in series {
                                guard let jobs = b.jobs, jobs > 0 else { continue }
                                let h = max(g.size.height * CGFloat(jobs) / CGFloat(peak), 1)
                                p.addRect(CGRect(x: x(b) + w * 0.15, y: g.size.height - h, width: max(w * 0.7, 1), height: h))
                            }
                        }
                        .fill(NotchStyle.mint)
                    }
                }
                GeometryReader { g in
                    ForEach(Array(axis.enumerated()), id: \.offset) { _, mark in
                        Text(mark.label)
                            .font(.system(size: 9).monospacedDigit())
                            .foregroundStyle(NotchStyle.tertiary)
                            .fixedSize()
                            .frame(width: 60, alignment: mark.at == 0 ? .leading : mark.at == 1 ? .trailing : .center)
                            .position(x: min(max(g.size.width * CGFloat(mark.at), 30), g.size.width - 30), y: g.size.height / 2)
                    }
                }
                .frame(height: 11)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Completed jobs per 15 minutes today")
        .accessibilityValue(summary)
    }

    private var summary: String {
        guard let peak = series.filter({ $0.jobs != nil }).max(by: { ($0.jobs ?? 0) < ($1.jobs ?? 0) }),
              let jobs = peak.jobs else { return emptyReason }
        let gaps = series.filter { $0.jobs == nil }.count
        return "Peak \(plural(Int(clamping: jobs), "job")) at \(NotchFormat.time(peak.start))"
            + (gaps > 0 ? ", \(plural(gaps, "interval")) without coverage" : "")
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
         source.estimate.value.map { "by midnight \(NotchFormat.range($0)) est. at today's pace" },
         source.prompt.value.map { "prompt \(NotchFormat.tokens($0))" },
         source.cached.value.map { "cached \(NotchFormat.tokens($0))" }].compactMap { $0 }.joined(separator: " · ")
    }
}

struct NowTab: View {
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
