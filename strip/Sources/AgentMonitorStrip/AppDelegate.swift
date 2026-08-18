import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var strip: StripController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let strip = StripController(config: StripConfig.fromEnvironment())
        self.strip = strip
        strip.show()

        NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil,
            queue: .main
        ) { [weak strip] _ in
            strip?.ensureOnScreen()
        }
    }
}

struct StripConfig {
    var url: URL
    /// First-run height only. After that the window remembers whatever the user
    /// dragged it to.
    var height: CGFloat

    static func fromEnvironment() -> StripConfig {
        let env = ProcessInfo.processInfo.environment
        let url = env["AGENT_MONITOR_STRIP_URL"].flatMap(URL.init(string:))
            ?? URL(string: "http://127.0.0.1:39917/")!
        let height = env["AGENT_MONITOR_STRIP_HEIGHT"].flatMap { Double($0) } ?? 260
        return StripConfig(url: url, height: max(110, height))
    }
}
