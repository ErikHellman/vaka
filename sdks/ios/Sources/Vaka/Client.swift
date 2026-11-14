import Foundation
import VakaNative
#if canImport(UIKit)
import UIKit
#endif
#if canImport(AppKit)
import AppKit
#endif

final class Client {
    let options: Options
    typealias Options = Vaka.Options

    private let q = DispatchQueue(label: "se.hellsoft.vaka")
    private let dir: URL
    private let transport: Transport
    let feedback: FeedbackSender
    private var buffer: [[String: Any]] = []
    private var timer: DispatchSourceTimer?
    private var observers: [NSObjectProtocol] = []
    private var watchdog: HangWatchdog?
    /// Set while the main thread is hung, so a kill during the hang ends the session as abnormal.
    private var hangSince: Date?
    /// Event id of the hang report for the current hang.
    private var hangEventId: String?
    private var lastEvent: String?
    private var lastScreen: String?
    // Crash/hang reports for feedback prompts. Handlers run on the main queue.
    private var crashHandlers: [(Vaka.CrashReport) -> Void] = []
    private var pendingCrash: Vaka.CrashReport?

    // Scope: persisted on every change so a crash report can carry the state of the run that crashed.
    private var breadcrumbs: [[String: Any]] = []
    private var userId: String?
    private var tags: [String: String]
    private var session: Session?
    private let anonId: String
    private let device: [String: Any]
    private let contexts: [String: Any]

    struct Session {
        var id: String
        var started: Date
        var errors: Int
        var backgroundSince: Date?
    }

    static let maxBreadcrumbs = 100

    var crashPath: URL { dir.appendingPathComponent("crash.txt") }
    var scopePath: URL { dir.appendingPathComponent("scope.json") }

    init(options: Options) {
        self.options = options
        let base = options.storageDirectory
            ?? (FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
                ?? URL(fileURLWithPath: NSTemporaryDirectory())).appendingPathComponent("Vaka", isDirectory: true)
        dir = base
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let debug = options.debug
        transport = Transport(endpoint: options.endpoint, key: options.key, directory: dir) { msg in
            if debug { print("[Vaka] \(msg)") }
        }
        feedback = FeedbackSender(endpoint: options.endpoint, key: options.key, directory: dir) { msg in
            if debug { print("[Vaka] \(msg)") }
        }
        tags = options.tags
        anonId = Client.loadAnonId()
        device = DeviceInfo.device(appVersion: options.release)
        contexts = DeviceInfo.contexts()
    }

    func log(_ msg: String) {
        if options.debug { print("[Vaka] \(msg)") }
    }

    // MARK: lifecycle

    func start() {
        // Take over the previous run's crash report and scope before anything can overwrite them.
        let crashText = (try? String(contentsOf: crashPath, encoding: .utf8))
        try? FileManager.default.removeItem(at: crashPath)
        let previous = (try? Data(contentsOf: scopePath)).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }

        if options.crashReporting {
            Client.installExceptionHandler()
            if vaka_crash_install(crashPath.path) != 0 { log("could not install crash handler") }
        }

        q.sync {
            if let prev = previous { userId = prev["user_id"] as? String }
            reportPreviousRun(crashText: crashText, previous: previous)
            writeScope()
        }

        let t = DispatchSource.makeTimerSource(queue: q)
        t.schedule(deadline: .now() + options.flushInterval, repeating: options.flushInterval)
        t.setEventHandler { [weak self] in self?.flushLocked() }
        t.resume()
        timer = t

        if options.sessionTracking { observeLifecycle() }
        if options.appHangDetection && options.appHangThreshold > 0 { startHangDetection() }
        transport.sendPending()
        feedback.sendPending()
    }

    private func reportPreviousRun(crashText: String?, previous: [String: Any]?) {
        var items: [[String: Any]] = []
        let prevSession = previous?["session"] as? [String: Any]
        let prevRelease = previous?["release"] as? String ?? options.release
        let prevEnv = previous?["environment"] as? String ?? options.environment
        if let text = crashText, let report = Native.parseReport(text) {
            var item = errorItem(level: .fatal, handled: false, fatal: true, timestamp: report.time)
            let (frames, images) = Native.frames(report.frames, images: report.images, current: Native.loadedImages())
            let exc = report.exception
            item["exceptions"] = [["type": exc.type, "value": exc.value, "stacktrace": frames]]
            item["debug_images"] = images
            item["release"] = prevRelease
            item["environment"] = prevEnv
            item["breadcrumbs"] = previous?["breadcrumbs"] as? [[String: Any]] ?? []
            if let t = previous?["tags"] as? [String: String] { item["tags"] = t }
            if let u = previous?["user_id"] as? String { item["user"] = ["id": u] } else { item.removeValue(forKey: "user") }
            if let s = prevSession?["id"] as? String { item["session_id"] = s } else { item.removeValue(forKey: "session_id") }
            if let sig = report.signalName {
                var c = contexts
                c["crash"] = ["signal": sig, "code": report.code ?? 0, "fault_addr": Native.hex(report.faultAddr ?? 0)]
                item["contexts"] = c
            }
            items.append(item)
            if let eid = item["event_id"] as? String {
                pendingCrash = Vaka.CrashReport(kind: .crash, eventId: eid, date: report.time, previousRun: true)
            }
            if let s = prevSession, let id = s["id"] as? String {
                let started = Date(timeIntervalSince1970: (s["started"] as? Double ?? 0) / 1000)
                items.append(sessionItem(id: id, status: "crashed", started: started, end: report.time,
                                         errors: (s["errors"] as? Int ?? 0) + 1, release: prevRelease, environment: prevEnv,
                                         userId: previous?["user_id"] as? String))
            }
        } else if let s = prevSession, let id = s["id"] as? String {
            // The previous run never ended its session: it was killed.
            let started = Date(timeIntervalSince1970: (s["started"] as? Double ?? 0) / 1000)
            let errors = s["errors"] as? Int ?? 0
            let bg = (s["background_since"] as? Double).map { Date(timeIntervalSince1970: $0 / 1000) }
            let updated = (previous?["updated"] as? Double).map { Date(timeIntervalSince1970: $0 / 1000) } ?? started
            let hang = (previous?["hang_since"] as? Double).map { Date(timeIntervalSince1970: $0 / 1000) }
            // Killed while hung (e.g. by the iOS watchdog) is abnormal even if it happened in background.
            if let h = hang, let eid = previous?["hang_event_id"] as? String {
                pendingCrash = Vaka.CrashReport(kind: .hang, eventId: eid, date: h, previousRun: true)
            }
            let status = hang != nil ? "abnormal" : bg != nil ? (errors > 0 ? "errored" : "exited") : "abnormal"
            items.append(sessionItem(id: id, status: status, started: started, end: hang ?? bg ?? updated, errors: errors,
                                     release: prevRelease, environment: prevEnv, userId: previous?["user_id"] as? String))
        }
        if !items.isEmpty { transport.persist(items) }
    }

    // MARK: app hangs

    private func startHangDetection() {
        let threshold = options.appHangThreshold
        let w = HangWatchdog(
            threshold: threshold,
            isPaused: { [weak self] in self?.q.sync { self?.session?.backgroundSince != nil } ?? true },
            onHang: { [weak self] addrs in self?.q.sync { self?.reportHang(addrs: addrs, threshold: threshold) } },
            onRecover: { [weak self] duration in
                self?.q.async {
                    guard let self else { return }
                    if let eid = self.hangEventId, let since = self.hangSince {
                        self.deliverCrash(Vaka.CrashReport(kind: .hang, eventId: eid, date: since, previousRun: false))
                    }
                    self.hangSince = nil
                    self.hangEventId = nil
                    self.log("main thread responsive again after \(Int(duration * 1000)) ms")
                    self.writeScope()
                }
            })
        watchdog = w
        w.start()
    }

    /// Runs on `q`, called from the watchdog thread while the main thread is still blocked.
    private func reportHang(addrs: [UInt64], threshold: TimeInterval) {
        let now = Date()
        hangSince = now
        var item = errorItem(level: .error, handled: false, fatal: false, timestamp: now)
        let (frames, images) = Native.frames(addrs, images: Native.loadedImages())
        let ms = Int((threshold * 1000).rounded())
        item["exceptions"] = [["type": "AppHang", "value": "Application not responding for \(ms) ms", "stacktrace": frames]]
        item["debug_images"] = images
        item["tags"] = (item["tags"] as? [String: String] ?? [:]).merging(["mechanism": "hang"]) { $1 }
        hangEventId = item["event_id"] as? String
        lastEvent = hangEventId
        if var s = session {
            s.errors += 1
            session = s
        }
        writeScope()
        log("app hang: main thread blocked for \(ms) ms")
        // Straight to disk: the app may be killed before it recovers.
        transport.persist([item])
        transport.sendPending()
    }

    /// Stop hang detection (tests).
    func stopHangDetection() {
        watchdog?.stop()
        watchdog = nil
    }

    private func observeLifecycle() {
        #if canImport(UIKit) && !os(watchOS)
        let nc = NotificationCenter.default
        observers.append(nc.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: nil) { [weak self] _ in
            self?.q.async { self?.foreground() }
        })
        observers.append(nc.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: nil) { [weak self] _ in
            self?.q.sync { self?.background() }
        })
        observers.append(nc.addObserver(forName: UIApplication.willTerminateNotification, object: nil, queue: nil) { [weak self] _ in
            self?.q.sync { self?.terminate() }
        })
        #else
        q.async { self.foreground() }
        #if canImport(AppKit)
        observers.append(NotificationCenter.default.addObserver(forName: NSApplication.willTerminateNotification, object: nil, queue: nil) { [weak self] _ in
            self?.q.sync { self?.terminate() }
        })
        #endif
        #endif
    }

    private func foreground() {
        let now = Date()
        if var s = session {
            if let bg = s.backgroundSince {
                if now.timeIntervalSince(bg) >= options.sessionTimeout {
                    endSession(at: bg)
                    startSession()
                } else {
                    s.backgroundSince = nil
                    session = s
                    writeScope()
                }
            }
        } else {
            startSession()
        }
        feedback.sendPending()
    }

    private func background() {
        if var s = session, s.backgroundSince == nil {
            s.backgroundSince = Date()
            session = s
        }
        writeScope()
        flushLocked()
    }

    private func terminate() {
        if let s = session { endSession(at: s.backgroundSince ?? Date()) }
        writeScope()
        transport.persist(buffer)
        buffer.removeAll()
    }

    private func startSession() {
        let s = Session(id: UUID().uuidString.lowercased(), started: Date(), errors: 0, backgroundSince: nil)
        session = s
        enqueue(sessionItem(id: s.id, status: "ok", started: s.started, end: s.started, errors: 0,
                            release: options.release, environment: options.environment, userId: userId))
        writeScope()
    }

    private func endSession(at end: Date) {
        guard let s = session else { return }
        session = nil
        enqueue(sessionItem(id: s.id, status: s.errors > 0 ? "errored" : "exited", started: s.started, end: end,
                            errors: s.errors, release: options.release, environment: options.environment, userId: userId))
        writeScope()
    }

    // MARK: public API (called from any thread)

    func screen(_ name: String, props: [String: Any]) {
        let ts = Date()
        q.async {
            var item = self.eventItem(name: "screen_view", props: props, timestamp: ts)
            item["screen"] = name
            self.lastScreen = name
            self.enqueue(item)
            self.addBreadcrumbLocked(["timestamp": Client.ms(ts), "category": "navigation", "message": name, "level": "info"])
        }
    }

    func track(_ name: String, props: [String: Any]) {
        let ts = Date()
        q.async { self.enqueue(self.eventItem(name: name, props: props, timestamp: ts)) }
    }

    @inline(never)
    func capture(error: Error, level: Vaka.Level, tags extra: [String: String], skipFrames: Int) {
        let addrs = Native.captureStack(skip: skipFrames + 1)
        let ts = Date()
        let (type, value) = Client.describe(error)
        q.async { self.captureLocked(type: type, value: value, message: nil, addrs: addrs, level: level, tags: extra, ts: ts) }
    }

    @inline(never)
    func capture(message: String, level: Vaka.Level, tags extra: [String: String], skipFrames: Int) {
        let addrs = Native.captureStack(skip: skipFrames + 1)
        let ts = Date()
        q.async { self.captureLocked(type: nil, value: nil, message: message, addrs: addrs, level: level, tags: extra, ts: ts) }
    }

    private func captureLocked(type: String?, value: String?, message: String?, addrs: [UInt64], level: Vaka.Level,
                               tags extra: [String: String], ts: Date) {
        var item = errorItem(level: level, handled: true, fatal: false, timestamp: ts)
        let (frames, images) = Native.frames(addrs, images: Native.loadedImages())
        if let type {
            item["exceptions"] = [["type": type, "value": value ?? "", "stacktrace": frames]]
        } else {
            item["message"] = message ?? ""
        }
        if type != nil { item["debug_images"] = images }
        lastEvent = item["event_id"] as? String
        if !extra.isEmpty { item["tags"] = (item["tags"] as? [String: String] ?? [:]).merging(extra) { $1 } }
        if level == .error || level == .fatal, var s = session {
            s.errors += 1
            session = s
            writeScope()
        }
        enqueue(item)
    }

    func addBreadcrumb(category: String, message: String, level: Vaka.Level, data: [String: Any]) {
        let ts = Date()
        q.async {
            var b: [String: Any] = ["timestamp": Client.ms(ts), "category": category, "message": message, "level": level.rawValue]
            if !data.isEmpty { b["data"] = Client.sanitize(data) }
            self.addBreadcrumbLocked(b)
        }
    }

    private func addBreadcrumbLocked(_ b: [String: Any]) {
        breadcrumbs.append(b)
        if breadcrumbs.count > Client.maxBreadcrumbs { breadcrumbs.removeFirst(breadcrumbs.count - Client.maxBreadcrumbs) }
        writeScope()
    }

    func identify(_ id: String?) {
        q.async {
            self.userId = id
            self.writeScope()
        }
    }

    func setTag(_ key: String, _ value: String?) {
        q.async {
            self.tags[key] = value
            self.writeScope()
        }
    }

    func flush() {
        q.async { self.flushLocked() }
    }

    // MARK: feedback

    var lastEventId: String? { q.sync { lastEvent } }

    func captureFeedback(_ f: Vaka.Feedback, completion: @escaping (Result<Vaka.FeedbackResult, Vaka.FeedbackError>) -> Void) {
        let message = f.message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty else { completion(.failure(.emptyMessage)); return }
        let ts = Date()
        let attachments = FeedbackSender.validAttachments(f.attachments, log: log)
        q.async {
            let item = self.feedbackItem(f, message: message, timestamp: ts)
            self.addBreadcrumbLocked(["timestamp": Client.ms(ts), "category": "feedback", "message": "user feedback sent", "level": "info"])
            self.feedback.send(item: item, attachments: attachments, completion: completion)
        }
    }

    func feedbackItem(_ f: Vaka.Feedback, message: String, timestamp: Date) -> [String: Any] {
        var item: [String: Any] = [
            "type": "feedback",
            "id": UUID().uuidString.lowercased(),
            "timestamp": Client.ms(timestamp),
            "platform": "ios",
            "message": String(message.prefix(FeedbackSender.maxMessage)),
            "source": f.source.rawValue,
            "release": options.release ?? "",
            "environment": options.environment,
            "anon_id": anonId,
            "device": device,
        ]
        if let e = f.email?.trimmingCharacters(in: .whitespacesAndNewlines), !e.isEmpty { item["email"] = e }
        if let n = f.name?.trimmingCharacters(in: .whitespacesAndNewlines), !n.isEmpty { item["name"] = n }
        if let e = f.eventId, !e.isEmpty { item["event_id"] = e }
        if let s = lastScreen { item["screen"] = s }
        let t = tags.merging(f.tags) { $1 }
        if !t.isEmpty { item["tags"] = t }
        if let u = userId { item["user_id"] = u }
        if let s = session { item["session_id"] = s.id }
        return item
    }

    func addCrashHandler(_ handler: @escaping (Vaka.CrashReport) -> Void) {
        q.async {
            self.crashHandlers.append(handler)
            if let p = self.pendingCrash {
                self.pendingCrash = nil
                DispatchQueue.main.async { handler(p) }
            }
        }
    }

    /// Runs on `q`.
    private func deliverCrash(_ r: Vaka.CrashReport) {
        if crashHandlers.isEmpty {
            if r.previousRun { pendingCrash = r }
            return
        }
        let handlers = crashHandlers
        DispatchQueue.main.async { handlers.forEach { $0(r) } }
    }

    /// For tests: flush and wait until the queue is empty (or `timeout` passes).
    func flushAndWait(timeout: TimeInterval = 10) -> Bool {
        q.sync {
            transport.persist(buffer)
            buffer.removeAll()
        }
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            let sem = DispatchSemaphore(value: 0)
            transport.sendPending { sem.signal() }
            _ = sem.wait(timeout: .now() + timeout)
            if transport.pendingCount == 0 { return true }
            Thread.sleep(forTimeInterval: 0.2)
        }
        return false
    }

    var pendingCount: Int { transport.pendingCount }

    private func flushLocked() {
        if !buffer.isEmpty {
            transport.persist(buffer)
            buffer.removeAll()
        }
        transport.sendPending()
        feedback.sendPending()
    }

    private func enqueue(_ item: [String: Any]) {
        buffer.append(item)
        if buffer.count >= options.maxBatchSize { flushLocked() }
    }

    // MARK: items

    private func errorItem(level: Vaka.Level, handled: Bool, fatal: Bool, timestamp: Date) -> [String: Any] {
        var item: [String: Any] = [
            "type": "error",
            "event_id": UUID().uuidString.lowercased(),
            "timestamp": Client.ms(timestamp),
            "platform": "ios",
            "level": level.rawValue,
            "handled": handled,
            "fatal": fatal,
            "release": options.release ?? "",
            "environment": options.environment,
            "breadcrumbs": breadcrumbs,
            "contexts": contexts,
            "anon_id": anonId,
            "device": device,
        ]
        if !tags.isEmpty { item["tags"] = tags }
        if let u = userId { item["user"] = ["id": u] }
        if let s = session { item["session_id"] = s.id }
        return item
    }

    private func eventItem(name: String, props: [String: Any], timestamp: Date) -> [String: Any] {
        var item: [String: Any] = [
            "type": "event",
            "event_id": UUID().uuidString.lowercased(),
            "name": name,
            "timestamp": Client.ms(timestamp),
            "platform": "ios",
            "release": options.release ?? "",
            "environment": options.environment,
            "anon_id": anonId,
            "device": device,
        ]
        if !props.isEmpty { item["props"] = Client.sanitize(props, limit: 64) }
        if let u = userId { item["user_id"] = u }
        if let s = session { item["session_id"] = s.id }
        return item
    }

    private func sessionItem(id: String, status: String, started: Date, end: Date, errors: Int, release: String?,
                             environment: String, userId: String?) -> [String: Any] {
        var item: [String: Any] = [
            "type": "session",
            "session_id": id,
            "status": status,
            "started": Client.ms(started),
            "timestamp": Client.ms(end),
            "duration_ms": max(0, Int64(end.timeIntervalSince(started) * 1000)),
            "errors": errors,
            "release": release ?? "",
            "environment": environment,
            "platform": "ios",
            "anon_id": anonId,
        ]
        if let u = userId { item["user_id"] = u }
        return item
    }

    private func writeScope() {
        var scope: [String: Any] = [
            "release": options.release ?? "",
            "environment": options.environment,
            "breadcrumbs": breadcrumbs,
            "tags": tags,
            "updated": Client.ms(Date()),
        ]
        if let u = userId { scope["user_id"] = u }
        if let h = hangSince { scope["hang_since"] = Client.ms(h) }
        if let e = hangEventId { scope["hang_event_id"] = e }
        if let s = session {
            var d: [String: Any] = ["id": s.id, "started": Client.ms(s.started), "errors": s.errors]
            if let bg = s.backgroundSince { d["background_since"] = Client.ms(bg) }
            scope["session"] = d
        }
        if let data = try? JSONSerialization.data(withJSONObject: scope) {
            try? data.write(to: scopePath, options: .atomic)
        }
    }

    // MARK: helpers

    static func ms(_ d: Date) -> Int64 { Int64((d.timeIntervalSince1970 * 1000).rounded()) }

    static func describe(_ error: Error) -> (String, String) {
        let ns = error as NSError
        // Local types print as "Mod.(unknown context at $1a2b3c).Name"; the address changes per
        // build and would split one issue into many, so drop those segments.
        let typeName = String(reflecting: Swift.type(of: error))
            .replacingOccurrences(of: #"\(unknown context at \$[0-9a-fA-F]+\)\."#, with: "", options: .regularExpression)
        if typeName == "__SwiftNativeNSError" || typeName.hasPrefix("NS") || typeName.hasPrefix("__") || typeName == "Foundation.NSError" {
            return (ns.domain, "Code=\(ns.code) \(ns.localizedDescription)")
        }
        return (typeName, String(describing: error))
    }

    static func sanitize(_ props: [String: Any], limit: Int = 64) -> [String: Any] {
        var out: [String: Any] = [:]
        for (k, v) in props.sorted(by: { $0.key < $1.key }).prefix(limit) {
            switch v {
            case let b as Bool: out[k] = b
            case let i as Int: out[k] = i
            case let d as Double: out[k] = d.isFinite ? d : String(d)
            case let f as Float: out[k] = Double(f)
            case let s as String: out[k] = s
            default: out[k] = String(describing: v)
            }
        }
        return out
    }

    static func loadAnonId() -> String {
        let key = "se.hellsoft.vaka.anon_id"
        if let id = UserDefaults.standard.string(forKey: key) { return id }
        let id = UUID().uuidString.lowercased()
        UserDefaults.standard.set(id, forKey: key)
        return id
    }

    // MARK: uncaught Objective-C exceptions

    static func installExceptionHandler() {
        #if canImport(ObjectiveC)
        previousExceptionHandler = NSGetUncaughtExceptionHandler()
        NSSetUncaughtExceptionHandler { exception in
            let addrs = exception.callStackReturnAddresses.map { UInt($0.uintValue) }
            addrs.withUnsafeBufferPointer { buf in
                _ = vaka_crash_write_exception(exception.name.rawValue, exception.reason ?? "", buf.baseAddress, Int32(buf.count))
            }
            previousExceptionHandler?(exception)
        }
        #endif
    }
}

#if canImport(ObjectiveC)
private var previousExceptionHandler: (@convention(c) (NSException) -> Void)?
#endif

enum DeviceInfo {
    static func model() -> String {
        #if os(iOS) || os(tvOS)
        // The simulator reports the host machine; prefer the simulated model ("iPhone16,2").
        if let sim = ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] { return sim }
        #endif
        #if os(macOS)
        var size = 0
        if sysctlbyname("hw.model", nil, &size, nil, 0) == 0, size > 0 {
            var buf = [CChar](repeating: 0, count: size)
            if sysctlbyname("hw.model", &buf, &size, nil, 0) == 0 { return String(cString: buf) }
        }
        #endif
        var u = utsname()
        uname(&u)
        return withUnsafeBytes(of: &u.machine) { String(cString: $0.bindMemory(to: CChar.self).baseAddress!) }
    }

    static func osName() -> String {
        #if os(iOS)
        return "iOS"
        #elseif os(tvOS)
        return "tvOS"
        #elseif os(macOS)
        return "macOS"
        #elseif os(Linux)
        return "Linux"
        #else
        return "unknown"
        #endif
    }

    static func osVersion() -> String {
        let v = ProcessInfo.processInfo.operatingSystemVersion
        return "\(v.majorVersion).\(v.minorVersion)" + (v.patchVersion > 0 ? ".\(v.patchVersion)" : "")
    }

    static func device(appVersion: String?) -> [String: Any] {
        var d: [String: Any] = ["model": model(), "os_name": osName(), "os_version": osVersion()]
        if let v = appVersion { d["app_version"] = v }
        return d
    }

    static func contexts() -> [String: Any] {
        let info = Bundle.main.infoDictionary ?? [:]
        var app: [String: Any] = [:]
        if let b = info["CFBundleVersion"] as? String { app["build"] = b }
        if let id = Bundle.main.bundleIdentifier { app["bundle_id"] = id }
        #if arch(arm64)
        let arch = "arm64"
        #elseif arch(x86_64)
        let arch = "x86_64"
        #else
        let arch = "unknown"
        #endif
        return [
            "app": app,
            "os": ["name": osName(), "version": osVersion()],
            "device": ["model": model(), "arch": arch],
            "runtime": ["name": "Swift"],
        ]
    }
}
