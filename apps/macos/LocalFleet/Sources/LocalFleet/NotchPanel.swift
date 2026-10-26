import AppKit
import SwiftUI

/// State the notch views observe; owned by NotchController.
@MainActor
final class NotchModel: ObservableObject {
    @Published var layout = NotchLayout(cameraWidth: 0, barHeight: 24, wingWidth: NotchLayout.defaultWing,
                                        expandedSize: CGSize(width: 650, height: 380))
    @Published var expanded = false
    @Published var tab: NotchTab = .production
    @Published var pinned = false
    var actions = NotchActions()
}

extension NotchTelemetry {
    var display: NotchDisplay {
        NotchDisplay(snapshot: snapshot, freshness: freshness, activeHistory: activeHistory,
                     engineFreshness: engineFreshness, omlxFreshness: omlxFreshness)
    }
}

/// Live trigger root. The two roots are the only views that read NotchTelemetry.
struct NotchTriggerRoot: View {
    @ObservedObject var telemetry: NotchTelemetry
    @ObservedObject var model: NotchModel

    var body: some View {
        NotchTrigger(display: telemetry.display, layout: model.layout, joined: model.expanded, actions: model.actions)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }
}

/// Live details-body root.
struct NotchBodyRoot: View {
    @ObservedObject var telemetry: NotchTelemetry
    @ObservedObject var model: NotchModel

    var body: some View {
        NotchBody(display: telemetry.display, layout: model.layout, tab: $model.tab,
                  pinned: model.pinned, actions: model.actions)
    }
}

/// Borderless, non-activating panel. Only the body can become key, only while it is open, and only
/// a click makes it key, so hovering never takes focus from the frontmost app.
final class NotchWindow: NSPanel {
    var keyable = false

    override var canBecomeKey: Bool { keyable }
    override var canBecomeMain: Bool { false }
    /// The trigger sits in the menu-bar strip; don't let AppKit push it below.
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}

/// Hosts a notch view, accepts the first click without activation and reports hover through an
/// always-active tracking area (the panels are not key while hovered).
final class NotchHostingView<Content: View>: NSHostingView<Content> {
    var onHover: ((Bool) -> Void)?
    private var hoverArea: NSTrackingArea?

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hoverArea { removeTrackingArea(hoverArea) }
        let area = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                                  owner: self, userInfo: nil)
        addTrackingArea(area)
        hoverArea = area
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        if event.trackingArea === hoverArea { onHover?(true) }
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        if event.trackingArea === hoverArea { onHover?(false) }
    }
}

/// The notch monitor, as two panels. The trigger sits in the menu-bar strip: on a notched screen
/// the left wing plus the camera housing, ending at the housing's right edge, so no window covers
/// the status items right of the camera; elsewhere a small centred pill. The details body is a
/// second panel entirely below the strip. Hover the trigger for 0.35 s or click it to open the
/// body; the body stays open while the pointer is over either panel. A click pins it open until a
/// click elsewhere, Escape or another click on the trigger.
@MainActor
final class NotchController {
    static let shared = NotchController()

    private static let enabledKey = "notch.enabled"
    private static let hoverDelay: Duration = .milliseconds(350)
    private static let leaveDelay: Duration = .milliseconds(300)
    private static let fadeDuration: Duration = .milliseconds(150)

    private enum Phase { case collapsed, hover, pinned }

    private let model = NotchModel()
    private var trigger: NotchWindow?
    private var bodyPanel: NotchWindow?
    private var phase: Phase = .collapsed
    private var shown = false
    private var overTrigger = false
    private var overBody = false
    /// After a deliberate collapse with the pointer still on the trigger, hover waits for it to leave.
    private var hoverSuppressed = false
    private var hoverTask: Task<Void, Never>?
    private var fadeTask: Task<Void, Never>?
    private var observers: [(NotificationCenter, NSObjectProtocol)] = []
    private var clickMonitor: Any?
    private var pinMonitors: [Any] = []
    /// Right edge of the camera housing (notched screen) or the screen's centre (pill).
    private var anchorX: CGFloat = 0
    private var screenFrame: NSRect = .zero

    private init() {
        UserDefaults.standard.register(defaults: [Self.enabledKey: true])
        model.actions = NotchActions(toggle: { [weak self] in self?.toggle() },
                                     details: { [weak self] in self?.openDetails() })
    }

    var isEnabled: Bool {
        get { UserDefaults.standard.bool(forKey: Self.enabledKey) }
        set { setEnabled(newValue) }
    }

    /// Persists the choice; on shows the notch and starts telemetry, off hides it and stops polling.
    func setEnabled(_ on: Bool) {
        UserDefaults.standard.set(on, forKey: Self.enabledKey)
        if on { show() } else { hide() }
    }

    // MARK: Offscreen render

    /// Renders the notch for the given data offscreen (2x), for evidence images from fixtures.
    /// Collapsed: the trigger at `size`. Expanded: the body alone at `size`, every row drawn
    /// without scrolling, so `size.height` is a minimum and the image grows to fit. `engineFreshness` and `omlxFreshness` default to `freshness`.
    static func renderPreview(snapshot: NotchSnapshot, freshness: Freshness, activeHistory: [TimePoint],
                              expanded: Bool, tab: NotchTab, size: CGSize,
                              engineFreshness: Freshness? = nil, omlxFreshness: Freshness? = nil) -> NSImage? {
        let view = NotchSurface(display: NotchDisplay(snapshot: snapshot, freshness: freshness, activeHistory: activeHistory,
                                                      engineFreshness: engineFreshness, omlxFreshness: omlxFreshness),
                                layout: .preview(size: size, expanded: expanded), expanded: expanded,
                                tab: .constant(tab))
            .frame(width: size.width, height: expanded ? nil : size.height)
        let renderer = ImageRenderer(content: view)
        renderer.scale = 2
        return renderer.nsImage
    }

    // MARK: Show / hide

    private func show() {
        guard !shown else { return }
        shown = true
        if trigger == nil { makePanels() }
        relayout()
        trigger?.orderFrontRegardless()
        installObservers()
        NotchTelemetry.shared.start(state: AppModel.shared)
    }

    private func hide() {
        guard shown else { return }
        shown = false
        collapse(animated: false)
        trigger?.orderOut(nil)
        removeObservers()
        NotchTelemetry.shared.stop()
    }

    private static func makePanel(_ content: NSView) -> NotchWindow {
        let panel = NotchWindow(contentRect: NSRect(x: 0, y: 0, width: 300, height: 32),
                                styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.hidesOnDeactivate = false
        panel.isMovable = false
        panel.isMovableByWindowBackground = false
        panel.isReleasedWhenClosed = false
        panel.animationBehavior = .none
        content.autoresizingMask = [.width, .height]
        panel.contentView = content
        return panel
    }

    private func makePanels() {
        let triggerView = NotchHostingView(rootView: NotchTriggerRoot(telemetry: .shared, model: model))
        triggerView.sizingOptions = []
        triggerView.onHover = { [weak self] inside in self?.hoverChanged(trigger: inside) }
        let trigger = Self.makePanel(triggerView)
        trigger.setAccessibilityLabel("LocalFleet notch")
        self.trigger = trigger

        let bodyView = NotchHostingView(rootView: NotchBodyRoot(telemetry: .shared, model: model))
        bodyView.sizingOptions = []
        bodyView.onHover = { [weak self] inside in self?.hoverChanged(body: inside) }
        let body = Self.makePanel(bodyView)
        body.setAccessibilityLabel("LocalFleet monitor")
        bodyPanel = body
    }

    // MARK: Geometry

    /// The built-in notched screen when there is one, else the main screen.
    private static func preferredScreen() -> NSScreen? {
        let notched = NSScreen.screens.filter { $0.safeAreaInsets.top > 0 }
        return notched.first(where: isBuiltIn) ?? notched.first ?? NSScreen.main ?? NSScreen.screens.first
    }

    private static func isBuiltIn(_ screen: NSScreen) -> Bool {
        guard let id = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber else { return false }
        return CGDisplayIsBuiltin(id.uint32Value) != 0
    }

    /// Camera housing from the safe-area inset (height) and the two auxiliary top areas (the
    /// unobscured menu-bar strips either side; the housing is what lies between them). The
    /// returned x is the housing's right edge, where the trigger is anchored. Without a notch, a pill
    /// as tall as the menu bar, centred in the menu bar's middle gap.
    static func geometry(for screen: NSScreen) -> (layout: NotchLayout, anchorX: CGFloat) {
        let frame = screen.frame
        let inset = screen.safeAreaInsets.top
        if inset > 0 {
            var width: CGFloat = 185
            var maxX = frame.midX + width / 2
            if let left = screen.auxiliaryTopLeftArea, let right = screen.auxiliaryTopRightArea {
                let between = frame.width - left.width - right.width
                if between > 0 {
                    width = between
                    maxX = frame.minX + left.width + between
                }
            }
            let body = CGSize(width: min(650, frame.width - 32), height: min(380, frame.height - inset - 40))
            return (NotchLayout(cameraWidth: width, barHeight: inset, wingWidth: NotchLayout.defaultWing,
                                expandedSize: body), maxX)
        }
        let menuBar = frame.maxY - screen.visibleFrame.maxY
        let bar = menuBar >= 18 ? min(menuBar, 32) : 24
        let body = CGSize(width: min(650, frame.width - 32), height: min(380, frame.height - bar - 40))
        return (NotchLayout(cameraWidth: 0, barHeight: bar, wingWidth: NotchLayout.defaultWing,
                            expandedSize: body), frame.midX)
    }

    /// Trigger: on a notched screen its right edge is the housing's right edge; a pill is centred.
    static func triggerFrame(_ layout: NotchLayout, anchorX: CGFloat, screen: NSRect) -> NSRect {
        let size = layout.collapsedSize
        let proposed = layout.hasNotch ? anchorX - size.width : anchorX - size.width / 2
        let x = min(max(proposed, screen.minX), screen.maxX - size.width)
        return NSRect(x: x, y: screen.maxY - size.height, width: size.width, height: size.height)
    }

    /// Body: top edge at the bottom of the menu-bar strip, so it never reaches into the strip. On a
    /// notched screen its right edge is the housing's right edge; under a pill it is centred.
    /// Clamped to the screen.
    static func bodyFrame(_ layout: NotchLayout, anchorX: CGFloat, screen: NSRect) -> NSRect {
        let size = layout.expandedSize
        let proposed = layout.hasNotch ? anchorX - size.width : anchorX - size.width / 2
        let x = min(max(proposed, screen.minX), screen.maxX - size.width)
        return NSRect(x: x, y: screen.maxY - layout.barHeight - size.height, width: size.width, height: size.height)
    }

    private func relayout() {
        guard let trigger, let screen = Self.preferredScreen() else { return }
        let g = Self.geometry(for: screen)
        screenFrame = screen.frame
        anchorX = g.anchorX
        if model.layout != g.layout { model.layout = g.layout }
        trigger.setFrame(Self.triggerFrame(g.layout, anchorX: anchorX, screen: screenFrame), display: true)
        bodyPanel?.setFrame(Self.bodyFrame(g.layout, anchorX: anchorX, screen: screenFrame), display: true)
    }

    // MARK: Interaction

    private var reduceMotion: Bool { NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }

    private func hoverChanged(trigger inside: Bool) {
        overTrigger = inside
        if !inside { hoverSuppressed = false }
        hoverChanged()
    }

    private func hoverChanged(body inside: Bool) {
        overBody = inside
        hoverChanged()
    }

    /// Opens after the pointer rests on the trigger; a hover-opened body closes once the pointer
    /// has left both panels for `leaveDelay` (crossing from one to the other cancels it).
    private func hoverChanged() {
        hoverTask?.cancel()
        let inside = overTrigger || overBody
        switch (phase, inside) {
        case (.collapsed, true) where overTrigger && !hoverSuppressed:
            hoverTask = delayed(Self.hoverDelay) { $0.expand(pinned: false) }
        case (.hover, false):
            hoverTask = delayed(Self.leaveDelay) { $0.collapse() }
        default:
            break
        }
    }

    private func delayed(_ delay: Duration, _ action: @escaping @MainActor (NotchController) -> Void) -> Task<Void, Never> {
        Task { @MainActor [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled, let self else { return }
            action(self)
        }
    }

    /// Every click in the app. A left click on the trigger opens and pins, or closes a pinned body;
    /// a left click in a hover-opened body pins it; a click anywhere else in the app closes a
    /// pinned body.
    private func handleClick(_ event: NSEvent) {
        let isLeft = event.type == .leftMouseDown
        if let trigger, event.window === trigger {
            guard isLeft else { return }
            if phase == .pinned { collapse(deliberate: true) } else { expand(pinned: true) }
        } else if let bodyPanel, event.window === bodyPanel {
            if isLeft, phase == .hover { expand(pinned: true) }
        } else if phase == .pinned {
            collapse()
        }
    }

    private func toggle() {
        if phase == .collapsed { expand(pinned: true) } else { collapse(deliberate: true) }
    }

    private func expand(pinned: Bool) {
        guard let bodyPanel else { return }
        hoverTask?.cancel()
        fadeTask?.cancel()
        let wasCollapsed = phase == .collapsed
        phase = pinned ? .pinned : .hover
        bodyPanel.keyable = true
        if wasCollapsed {
            model.expanded = true
            bodyPanel.setFrame(Self.bodyFrame(model.layout, anchorX: anchorX, screen: screenFrame), display: true)
            bodyPanel.alphaValue = reduceMotion ? 1 : 0
            bodyPanel.orderFrontRegardless()
            if !reduceMotion {
                NSAnimationContext.runAnimationGroup { context in
                    context.duration = 0.15
                    bodyPanel.animator().alphaValue = 1
                }
            }
        }
        if pinned {
            installPinMonitors()
            bodyPanel.makeKey()
            model.pinned = true
        }
    }

    private func collapse(animated: Bool = true, deliberate: Bool = false) {
        hoverTask?.cancel()
        guard phase != .collapsed, let bodyPanel else { return }
        phase = .collapsed
        model.pinned = false
        model.expanded = false
        hoverSuppressed = deliberate && overTrigger
        overBody = false
        removePinMonitors()
        bodyPanel.keyable = false
        fadeTask?.cancel()
        // Ordering the body out hands keyboard focus back to the frontmost app.
        if animated && !reduceMotion {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.15
                bodyPanel.animator().alphaValue = 0
            }
            fadeTask = delayed(Self.fadeDuration) { c in
                guard c.phase == .collapsed else { return }
                c.bodyPanel?.orderOut(nil)
            }
        } else {
            bodyPanel.orderOut(nil)
        }
    }

    private func openDetails() {
        collapse(animated: false)
        PreferencesController.shared.show()
    }

    // MARK: Observers and monitors

    private func installObservers() {
        let center = NotificationCenter.default
        let workspace = NSWorkspace.shared.notificationCenter
        observers = [
            (center, center.addObserver(forName: NSApplication.didChangeScreenParametersNotification, object: nil,
                                        queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.relayout() }
            }),
            (workspace, workspace.addObserver(forName: NSWorkspace.activeSpaceDidChangeNotification, object: nil,
                                              queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self, self.shown else { return }
                    self.relayout()
                    self.trigger?.orderFrontRegardless()
                    if self.phase != .collapsed { self.bodyPanel?.orderFrontRegardless() }
                }
            }),
        ]
        clickMonitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { [weak self] event in
            MainActor.assumeIsolated { self?.handleClick(event) }
            return event
        }
    }

    private func removeObservers() {
        for (center, token) in observers { center.removeObserver(token) }
        observers = []
        if let clickMonitor { NSEvent.removeMonitor(clickMonitor) }
        clickMonitor = nil
    }

    /// While pinned: a click in another app dismisses, and Escape collapses.
    private func installPinMonitors() {
        guard pinMonitors.isEmpty else { return }
        if let global = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown],
                                                          handler: { [weak self] _ in
            MainActor.assumeIsolated { self?.collapse() }
        }) {
            pinMonitors.append(global)
        }
        if let escape = NSEvent.addLocalMonitorForEvents(matching: .keyDown, handler: { [weak self] event in
            let handled = MainActor.assumeIsolated {
                guard let self, event.keyCode == 53,
                      event.window === self.bodyPanel || event.window === self.trigger else { return false }
                self.collapse(deliberate: true)
                return true
            }
            return handled ? nil : event
        }) {
            pinMonitors.append(escape)
        }
    }

    private func removePinMonitors() {
        pinMonitors.forEach(NSEvent.removeMonitor)
        pinMonitors = []
    }
}
