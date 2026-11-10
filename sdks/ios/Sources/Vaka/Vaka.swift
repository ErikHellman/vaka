import Foundation

/// Vaka crash reporting and analytics.
///
/// ```swift
/// Vaka.start(Vaka.Options(key: "vk_...", endpoint: URL(string: "https://vaka.example.com")!))
/// Vaka.screen("Checkout")
/// Vaka.track("purchase", ["plan": "pro"])
/// Vaka.capture(error)
/// ```
///
/// All methods are thread-safe and do nothing until `start` has been called.
public enum Vaka {
    public static let sdkName = "vaka-ios"
    public static let sdkVersion = "0.1.0"

    public struct Options {
        /// Project ingest key (`vk_...`). Write-only, safe to embed in the app.
        public var key: String
        /// Base URL of the Vaka server, e.g. `https://vaka.example.com`.
        public var endpoint: URL
        /// App version as users see it. Must match the `--release` used for `vaka upload dsym`.
        /// Defaults to `CFBundleShortVersionString`.
        public var release: String?
        /// Defaults to `production`.
        public var environment: String
        /// Install signal and uncaught-exception handlers.
        public var crashReporting: Bool
        /// Track release-health sessions (start on foreground, end after 30 s in background).
        public var sessionTracking: Bool
        /// Seconds in background after which the session ends.
        public var sessionTimeout: TimeInterval
        /// Seconds between batched sends.
        public var flushInterval: TimeInterval
        /// Send as soon as this many items are queued.
        public var maxBatchSize: Int
        /// Print SDK diagnostics to the console.
        public var debug: Bool
        /// Set to false to turn the SDK into a no-op (e.g. for debug builds or opted-out users).
        public var enabled: Bool
        /// Extra tags added to every error.
        public var tags: [String: String]
        /// Where the SDK keeps its queue and crash reports. Defaults to Application Support/Vaka.
        public var storageDirectory: URL?

        public init(
            key: String,
            endpoint: URL,
            release: String? = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String,
            environment: String = "production",
            crashReporting: Bool = true,
            sessionTracking: Bool = true,
            sessionTimeout: TimeInterval = 30,
            flushInterval: TimeInterval = 10,
            maxBatchSize: Int = 50,
            debug: Bool = false,
            enabled: Bool = true,
            tags: [String: String] = [:],
            storageDirectory: URL? = nil
        ) {
            self.key = key
            self.endpoint = endpoint
            self.release = release
            self.environment = environment
            self.crashReporting = crashReporting
            self.sessionTracking = sessionTracking
            self.sessionTimeout = sessionTimeout
            self.flushInterval = flushInterval
            self.maxBatchSize = maxBatchSize
            self.debug = debug
            self.enabled = enabled
            self.tags = tags
            self.storageDirectory = storageDirectory
        }
    }

    public enum Level: String {
        case fatal, error, warning, info
    }

    private static let lock = NSLock()
    private static var _client: Client?

    static var client: Client? {
        lock.lock(); defer { lock.unlock() }
        return _client
    }

    /// Start the SDK. Call once, as early as possible (App `init` or
    /// `application(_:didFinishLaunchingWithOptions:)`). Later calls are ignored.
    public static func start(_ options: Options) {
        guard options.enabled else { return }
        lock.lock()
        if _client != nil { lock.unlock(); return }
        let c = Client(options: options)
        _client = c
        lock.unlock()
        c.start()
    }

    /// Record a `screen_view`.
    public static func screen(_ name: String, _ props: [String: Any] = [:]) {
        client?.screen(name, props: props)
    }

    /// Record a custom analytics event. `props` values should be String, number or Bool.
    public static func track(_ name: String, _ props: [String: Any] = [:]) {
        client?.track(name, props: props)
    }

    /// Report a handled error with the current stack trace.
    @inline(never)
    public static func capture(_ error: Error, level: Level = .error, tags: [String: String] = [:]) {
        client?.capture(error: error, level: level, tags: tags, skipFrames: 1)
    }

    /// Report a message (no exception) with the current stack trace.
    @inline(never)
    public static func capture(message: String, level: Level = .info, tags: [String: String] = [:]) {
        client?.capture(message: message, level: level, tags: tags, skipFrames: 1)
    }

    /// Add a breadcrumb that is attached to later errors and crashes (last 100 are kept).
    public static func addBreadcrumb(category: String, message: String, level: Level = .info, data: [String: Any] = [:]) {
        client?.addBreadcrumb(category: category, message: message, level: level, data: data)
    }

    /// Opt in to user identification. `nil` clears it. Analytics stay anonymous until this is called.
    public static func identify(_ userId: String?) {
        client?.identify(userId)
    }

    /// Set a tag on all later errors. `nil` removes it.
    public static func setTag(_ key: String, _ value: String?) {
        client?.setTag(key, value)
    }

    /// Send queued items now.
    public static func flush() {
        client?.flush()
    }
}
