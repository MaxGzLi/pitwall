import AppKit
import WebKit

/// Borderless, non-activating panel the user drags and resizes themselves.
///
/// There is no system title bar on purpose: it would eat vertical space and
/// occlude the page. Instead `ChromeView` sits on top of the web view and claims
/// only two kinds of pixel — a thin band around the edges (resize) and the top
/// strip that the page already spends on its section labels (move). Everything
/// else falls through to the page, so summaries stay clickable.
final class StripController: NSObject, WKNavigationDelegate {
    private let config: StripConfig
    private let panel: NSPanel
    private let webView: WKWebView
    private var retryTimer: Timer?
    private var saveTimer: Timer?

    /// `--bg` from web/strip.css, so the panel and the page agree while loading.
    private static let background = NSColor(srgbRed: 0x0a / 255, green: 0x0c / 255, blue: 0x0f / 255, alpha: 1)
    private static let frameKey = "panelFrame"

    init(config: StripConfig) {
        self.config = config

        let webConfig = WKWebViewConfiguration()
        webConfig.suppressesIncrementalRendering = true
        webView = WKWebView(frame: .zero, configuration: webConfig)
        webView.underPageBackgroundColor = Self.background // no white flash before the page paints
        webView.autoresizingMask = [.width, .height]
        webView.wantsLayer = true
        webView.layer?.cornerRadius = Self.cornerRadius
        webView.layer?.masksToBounds = true

        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: config.height),
            styleMask: [.nonactivatingPanel, .borderless, .resizable],
            backing: .buffered,
            defer: false
        )
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        panel.isFloatingPanel = true
        panel.level = .statusBar // must follow isFloatingPanel: its setter forces .floating
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.isRestorable = false
        panel.hasShadow = true
        panel.isMovable = false            // ChromeView moves it, so a stray page drag cannot
        panel.isMovableByWindowBackground = false
        panel.acceptsMouseMovedEvents = true // cursor feedback without being the key window
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.minSize = NSSize(width: 380, height: 110)

        let chrome = ChromeView(frame: .zero)
        chrome.wantsLayer = true
        chrome.layer?.cornerRadius = Self.cornerRadius
        chrome.layer?.masksToBounds = true
        chrome.layer?.backgroundColor = Self.background.cgColor
        chrome.addSubview(webView)
        panel.contentView = chrome
        webView.frame = chrome.bounds

        super.init()
        webView.navigationDelegate = self
        panel.delegate = self
    }

    /// A 10pt radius needs the panel itself to be transparent, hence `isOpaque = false`
    /// above; the rounding is done by the two layers, not by AppKit's frame view.
    private static let cornerRadius: CGFloat = 10

    func show() {
        panel.setFrame(restoredFrame() ?? defaultFrame(), display: false)
        ensureOnScreen()
        panel.orderFrontRegardless()
        load()
    }

    /// Frame persistence is done by hand. `setFrameAutosaveName` re-scales the
    /// saved rect against whatever it recorded as the screen size, which on this
    /// machine turned a 1080x260 window into 1080x335 across a restart.
    /// Coalesced: a live resize posts `didResize` on every frame.
    private func scheduleSave() {
        saveTimer?.invalidate()
        saveTimer = Timer.scheduledTimer(withTimeInterval: 0.4, repeats: false) { [weak self] _ in
            self?.saveFrame()
        }
    }

    private func saveFrame() {
        let f = panel.frame
        let store = UserDefaults.standard
        store.set([f.minX, f.minY, f.width, f.height], forKey: Self.frameKey)
        // There is no orderly quit — this app is stopped with `pkill` — so the
        // deferred write cfprefsd would normally coalesce has to be forced out now.
        store.synchronize()
    }

    private func restoredFrame() -> NSRect? {
        guard let v = UserDefaults.standard.array(forKey: Self.frameKey) as? [Double],
              v.count == 4, v[2] >= 1, v[3] >= 1 else { return nil }
        return NSRect(x: v[0], y: v[1], width: v[2], height: v[3])
    }

    /// First run only: the bottom edge of the portrait display, full width.
    /// `frame`, not `visibleFrame` — the portrait screen reserves nothing, and the
    /// window should start on the physical edge.
    private func defaultFrame() -> NSRect {
        let screen = NSScreen.screens.first { $0.frame.height > $0.frame.width } ?? NSScreen.main
        guard let screen else { return NSRect(x: 0, y: 0, width: 900, height: config.height) }
        let f = screen.frame
        return NSRect(x: f.minX, y: f.minY, width: f.width, height: config.height)
    }

    /// Called when displays change. A saved frame can name a screen that is gone,
    /// which would strand the window off-canvas with no way to grab it back.
    func ensureOnScreen() {
        let frame = panel.frame
        let visible = NSScreen.screens.contains { $0.frame.intersects(frame.insetBy(dx: 40, dy: 20)) }
        if !visible { panel.setFrame(defaultFrame(), display: true) }
    }

    private func load() {
        var request = URLRequest(url: config.url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 5
        webView.load(request)
    }

    // MARK: - daemon-down handling

    private func showError(_ reason: String) {
        webView.loadHTMLString(Self.errorHTML(url: config.url, reason: reason), baseURL: nil)
        guard retryTimer == nil else { return }
        let timer = Timer(timeInterval: 5, repeats: true) { [weak self] _ in self?.probe() }
        RunLoop.main.add(timer, forMode: .common)
        retryTimer = timer
    }

    /// Probe with URLSession before touching the web view, so a still-down daemon
    /// leaves the error page untouched instead of flashing a failed navigation.
    private func probe() {
        var request = URLRequest(url: config.url)
        request.httpMethod = "HEAD"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 3
        URLSession.shared.dataTask(with: request) { [weak self] _, response, _ in
            guard response != nil else { return }
            DispatchQueue.main.async { self?.load() }
        }.resume()
    }

    private func stopRetrying() {
        retryTimer?.invalidate()
        retryTimer = nil
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if webView.url?.scheme?.hasPrefix("http") == true {
            stopRetrying()
        }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        showError(error.localizedDescription)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        showError(error.localizedDescription)
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        load()
    }

    private static func errorHTML(url: URL, reason: String) -> String {
        let escaped = reason
            .replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
        return """
        <!doctype html><meta charset="utf-8"><style>
        html,body{height:100%;margin:0;background:#0a0c0f;color:#838d9c;overflow:hidden;
          font:12px/1.5 -apple-system,"SF Pro Text",Helvetica,Arial,sans-serif;
          -webkit-font-smoothing:antialiased;user-select:none;cursor:default}
        div{height:100%;display:flex;flex-direction:column;justify-content:center;padding:0 14px;gap:3px}
        b{color:#ff5a5f;font-weight:600}
        code{color:#b3bcc9;font-family:ui-monospace,Menlo,monospace}
        </style><div>
        <span><b>daemon unreachable</b> &nbsp;<code>\(url.absoluteString)</code></span>
        <span>\(escaped)</span>
        <span>retrying every 5s</span>
        </div>
        """
    }
}

extension StripController: NSWindowDelegate {
    // Covers both gestures: the move ChromeView performs and the edge resize
    // AppKit performs before any of our code sees the event.
    func windowDidMove(_ notification: Notification) { scheduleSave() }
    func windowDidResize(_ notification: Notification) { scheduleSave() }
}

/// The only view that competes with the page for mouse input, and it claims one
/// band: the top strip the page already spends on section labels, which acts as
/// the title bar this window deliberately does not have. `hitTest` hands every
/// other point back to the web view, and the outer 6pt is left to AppKit so its
/// native edge-resize (which runs before hit testing) keeps working.
private final class ChromeView: NSView {
    private let dragBar: CGFloat = 26
    private let edge: CGFloat = 6

    private var anchor: NSPoint = .zero
    private var startOrigin: NSPoint = .zero
    private var dragging = false
    private var showingOpenHand = false

    // AppKit's default bottom-left origin is what the frame maths below assumes.
    override var isFlipped: Bool { false }

    private func isDragBar(_ p: NSPoint) -> Bool {
        let b = bounds
        return p.y <= b.maxY - edge && p.y >= b.maxY - dragBar
            && p.x > edge && p.x < b.maxX - edge
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        if isDragBar(convert(point, from: superview)) { return self }
        return super.hitTest(point)
    }

    override func mouseDown(with event: NSEvent) {
        guard let window, isDragBar(convert(event.locationInWindow, from: nil)) else { return }
        dragging = true
        anchor = NSEvent.mouseLocation
        startOrigin = window.frame.origin
    }

    override func mouseDragged(with event: NSEvent) {
        guard dragging, let window else { return }
        let now = NSEvent.mouseLocation
        window.setFrameOrigin(NSPoint(
            x: startOrigin.x + now.x - anchor.x,
            y: startOrigin.y + now.y - anchor.y
        ))
    }

    override func mouseUp(with event: NSEvent) {
        guard dragging else { return }
        mouseDragged(with: event) // land on the release point; the last drag event can be coalesced away
        dragging = false
    }

    // Cursor rects only track the key window and this panel never becomes key,
    // so the hint comes from a tracking area instead.

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas { removeTrackingArea(area) }
        addTrackingArea(NSTrackingArea(
            rect: .zero,
            options: [.activeAlways, .mouseMoved, .mouseEnteredAndExited, .inVisibleRect],
            owner: self
        ))
    }

    override func mouseMoved(with event: NSEvent) {
        hint(isDragBar(convert(event.locationInWindow, from: nil)))
    }

    override func mouseExited(with event: NSEvent) {
        hint(false)
    }

    /// Only touch the cursor on a transition, so the page keeps control of its own
    /// everywhere this view is not claiming pixels.
    private func hint(_ overBar: Bool) {
        guard overBar != showingOpenHand else { return }
        showingOpenHand = overBar
        (overBar ? NSCursor.openHand : NSCursor.arrow).set()
    }
}
