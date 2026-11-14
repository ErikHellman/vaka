import Foundation
import XCTest
@testable import Vaka
import VakaNative

final class NativeTests: XCTestCase {
    func tmpDir() -> URL {
        let d = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("vaka-test-\(UUID().uuidString)")
        try! FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    func testLoadedImagesHaveIds() {
        let images = Native.loadedImages()
        XCTAssertFalse(images.isEmpty)
        XCTAssertTrue(images.contains { !$0.id.isEmpty && $0.size > 0 }, "\(images.prefix(3))")
    }

    @inline(never)
    func testCaptureStackStartsInCaller() {
        let addrs = Native.captureStack(skip: 0)
        XCTAssertGreaterThan(addrs.count, 2)
        let name = Native.symbolName(addrs[0] - 1) ?? ""
        XCTAssertTrue(name.contains("testCaptureStackStartsInCaller"), "first frame was \(name); all: \(addrs.prefix(5).map { Native.symbolName($0 - 1) ?? Native.hex($0) })")
    }

    func testGzipProducesGzipStream() {
        let input = Array(String(repeating: "vaka ", count: 1000).utf8)
        var out = [UInt8](repeating: 0, count: vaka_gzip_bound(input.count))
        let n = vaka_gzip(input, input.count, &out, out.count)
        XCTAssertGreaterThan(n, 10)
        XCTAssertLessThan(n, input.count / 10)
        XCTAssertEqual(Array(out.prefix(2)), [0x1f, 0x8b])
    }

    func testSegfaultWritesReport() throws {
        let path = tmpDir().appendingPathComponent("crash.txt")
        let status = vaka_test_fork_crash(path.path, SIGSEGV)
        XCTAssertNotEqual(status, 0)
        let text = try String(contentsOf: path, encoding: .utf8)
        let report = try XCTUnwrap(Native.parseReport(text), text)
        XCTAssertTrue(text.hasSuffix("end\n"), text)
        XCTAssertEqual(report.signalName, "SIGSEGV")
        XCTAssertEqual(report.faultAddr, 8)
        XCTAssertEqual(report.exception.type, "EXC_BAD_ACCESS")
        XCTAssertGreaterThan(report.frames.count, 2, text)
        XCTAssertFalse(report.images.isEmpty, text)
        // Frame 0 is the faulting instruction inside vaka_test_crash_segv.
        let (frames, images) = Native.frames(report.frames, images: report.images, current: Native.loadedImages())
        // (Release builds inline it into a static, unexported caller, so only check debug builds.)
        #if DEBUG
        XCTAssertEqual(frames.first?["function"] as? String, "vaka_test_crash_segv", "\(frames.prefix(3))")
        #endif
        XCTAssertTrue(frames.contains { ($0["function"] as? String)?.contains("testSegfaultWritesReport") == true }, "\(frames.prefix(5))")
        XCTAssertTrue(frames.contains { ($0["function"] as? String)?.contains("vaka_test_fork_crash") == true }, "\(frames.prefix(5))")
        XCTAssertFalse(images.isEmpty)
        XCTAssertNotNil(images.first?["image_addr"])
    }

    func testAbortWritesReport() throws {
        let path = tmpDir().appendingPathComponent("crash.txt")
        _ = vaka_test_fork_crash(path.path, SIGABRT)
        let report = try XCTUnwrap(Native.parseReport(try String(contentsOf: path, encoding: .utf8)))
        XCTAssertEqual(report.signalName, "SIGABRT")
        XCTAssertEqual(report.exception.type, "SIGABRT")
    }

    func testParseExceptionReport() throws {
        let text = """
        vaka-crash 1
        time 1762761600.123
        exception_type NSInvalidArgumentException
        exception_reason -[NSNull length]: unrecognized selector
        frame 0x104a3c1f4
        frame 0x104a3b0e8
        image 0x104a20000 0x100000 macho 6a8e2b3c1f0d3e4a9b8c7d6e5f4a3b2c /var/containers/Bundle/Application/X/My App.app/My App
        end
        """
        let r = try XCTUnwrap(Native.parseReport(text))
        XCTAssertEqual(r.exception.type, "NSInvalidArgumentException")
        XCTAssertEqual(r.exception.value, "-[NSNull length]: unrecognized selector")
        XCTAssertEqual(r.frames, [0x104a3c1f4, 0x104a3b0e8])
        XCTAssertEqual(r.time.timeIntervalSince1970, 1762761600.123, accuracy: 0.001)
        let img = try XCTUnwrap(r.images.first)
        XCTAssertEqual(img.path, "/var/containers/Bundle/Application/X/My App.app/My App")
        XCTAssertEqual(img.json["debug_id"] as? String, "6a8e2b3c-1f0d-3e4a-9b8c-7d6e5f4a3b2c")
        XCTAssertEqual(img.json["type"] as? String, "macho")
        let (frames, images) = Native.frames(r.frames, images: r.images, current: [])
        XCTAssertEqual(frames.count, 2)
        XCTAssertEqual(images.count, 1)
        XCTAssertEqual(frames[0]["instruction_addr"] as? String, "0x104a3c1f4")
    }

    func testTruncatedReportIsIgnored() {
        XCTAssertNil(Native.parseReport(""))
        XCTAssertNil(Native.parseReport("garbage"))
    }

    func testDescribeErrors() {
        enum CartError: Error { case empty }
        let (t, v) = Client.describe(CartError.empty)
        XCTAssertTrue(t.hasSuffix("CartError"), t)
        XCTAssertEqual(v, "empty")
        let (t2, v2) = Client.describe(NSError(domain: "NSURLErrorDomain", code: -1009))
        XCTAssertEqual(t2, "NSURLErrorDomain")
        XCTAssertTrue(v2.hasPrefix("Code=-1009"), v2)
    }

    func testSanitizeProps() {
        let p = Client.sanitize(["a": 1, "b": true, "c": "x", "d": 1.5, "e": [1, 2]])
        XCTAssertEqual(p["a"] as? Int, 1)
        XCTAssertEqual(p["b"] as? Bool, true)
        XCTAssertEqual(p["e"] as? String, "[1, 2]")
    }
}

/// End-to-end against a running server. Set VAKA_TEST_ENDPOINT and VAKA_TEST_KEY to run.
@inline(never)
func blockMainThreadForHangTest(_ seconds: TimeInterval) {
    Thread.sleep(forTimeInterval: seconds)
}

final class HangTests: XCTestCase {
    func tmpDir() -> URL {
        let d = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("vaka-hang-\(UUID().uuidString)")
        try! FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    func pendingItems(_ dir: URL) -> [[String: Any]] {
        let pending = dir.appendingPathComponent("pending")
        let files = (try? FileManager.default.contentsOfDirectory(at: pending, includingPropertiesForKeys: nil)) ?? []
        return files.flatMap { f -> [[String: Any]] in
            guard let d = try? Data(contentsOf: f), let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any] else { return [] }
            return o["items"] as? [[String: Any]] ?? []
        }
    }

    func options(_ dir: URL, threshold: TimeInterval = 0.5) -> Vaka.Options {
        // Unreachable endpoint: batches stay on disk where the test can read them.
        Vaka.Options(key: "vk_test", endpoint: URL(string: "http://127.0.0.1:9")!, release: "1.0.0", environment: "test",
                     crashReporting: false, appHangThreshold: threshold, flushInterval: 3600, storageDirectory: dir)
    }

    func testSampleMainThreadFromBackground() {
        XCTAssertTrue(Thread.isMainThread)
        XCTAssertEqual(vaka_sampler_install_main_thread(), 0)
        var addrs: [UInt64] = []
        let done = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            Thread.sleep(forTimeInterval: 0.1)
            addrs = HangWatchdog.sampleMainThread()
            done.signal()
        }
        blockMainThreadForHangTest(0.3)
        done.wait()
        XCTAssertGreaterThan(addrs.count, 2)
        let (frames, _) = Native.frames(addrs, images: Native.loadedImages())
        XCTAssertTrue(frames.contains { ($0["function"] as? String)?.contains("testSampleMainThreadFromBackground") == true },
                      "\(frames.prefix(8).map { $0["function"] ?? $0["instruction_addr"]! })")
    }

    func testReportsMainThreadHang() throws {
        let dir = tmpDir()
        let client = Client(options: options(dir))
        client.start()
        defer { client.stopHangDetection() }
        RunLoop.main.run(until: Date().addingTimeInterval(1))
        XCTAssertTrue(pendingItems(dir).filter { $0["type"] as? String == "error" }.isEmpty, "false hang while responsive")

        blockMainThreadForHangTest(1.5)
        let scope = try JSONSerialization.jsonObject(with: Data(contentsOf: dir.appendingPathComponent("scope.json"))) as? [String: Any]
        XCTAssertNotNil(scope?["hang_since"], "hang marker set while still hung")
        RunLoop.main.run(until: Date().addingTimeInterval(0.5))

        let errors = pendingItems(dir).filter { $0["type"] as? String == "error" }
        XCTAssertEqual(errors.count, 1, "one report per hang")
        let item = try XCTUnwrap(errors.first)
        XCTAssertEqual(item["fatal"] as? Bool, false)
        XCTAssertEqual(item["handled"] as? Bool, false)
        XCTAssertEqual((item["tags"] as? [String: String])?["mechanism"], "hang")
        let exc = try XCTUnwrap((item["exceptions"] as? [[String: Any]])?.first)
        XCTAssertEqual(exc["type"] as? String, "AppHang")
        XCTAssertEqual(exc["value"] as? String, "Application not responding for 500 ms")
        let frames = exc["stacktrace"] as? [[String: Any]] ?? []
        // On Linux, glibc has no frame pointers, so the frame right above nanosleep can be skipped;
        // Apple system libraries keep them and the whole chain is there.
        XCTAssertTrue(frames.contains { ($0["function"] as? String)?.contains("blockMainThreadForHangTest") == true
                          || ($0["function"] as? String)?.contains("testReportsMainThreadHang") == true },
                      "\(frames.prefix(8).map { $0["function"] ?? $0["instruction_addr"]! })")
        XCTAssertNotNil(item["session_id"])

        client.flush()
        Thread.sleep(forTimeInterval: 0.2)
        let after = try JSONSerialization.jsonObject(with: Data(contentsOf: dir.appendingPathComponent("scope.json"))) as? [String: Any]
        XCTAssertNil(after?["hang_since"], "hang marker cleared after recovery")
    }

    func testKilledWhileHungEndsSessionAbnormal() throws {
        let dir = tmpDir()
        let started = Client.ms(Date().addingTimeInterval(-60))
        let hang = Client.ms(Date().addingTimeInterval(-10))
        let scope: [String: Any] = [
            "release": "1.0.0", "environment": "test", "breadcrumbs": [], "tags": [:],
            // Backgrounded during the hang, which would otherwise end the session as "exited".
            "session": ["id": "prev-session", "started": started, "errors": 1, "background_since": hang + 1000],
            "hang_since": hang,
        ]
        try JSONSerialization.data(withJSONObject: scope).write(to: dir.appendingPathComponent("scope.json"))
        var opts = options(dir)
        opts.appHangDetection = false
        let client = Client(options: opts)
        client.start()
        let session = try XCTUnwrap(pendingItems(dir).first { $0["session_id"] as? String == "prev-session" })
        XCTAssertEqual(session["status"] as? String, "abnormal")
        XCTAssertEqual(session["timestamp"] as? Int64, hang)
    }
}

final class FeedbackTests: XCTestCase {
    func tmpDir() -> URL {
        let d = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("vaka-fb-\(UUID().uuidString)")
        try! FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    func options(_ dir: URL) -> Vaka.Options {
        Vaka.Options(key: "vk_test", endpoint: URL(string: "http://127.0.0.1:9")!, release: "1.0.0", environment: "test",
                     crashReporting: false, appHangDetection: false, flushInterval: 3600, storageDirectory: dir)
    }

    func testFeedbackItemCarriesScope() throws {
        let client = Client(options: options(tmpDir()))
        client.start()
        client.identify("u-1")
        client.setTag("plan", "pro")
        client.screen("Checkout", props: [:])
        enum E: Error { case boom }
        client.capture(error: E.boom, level: .error, tags: [:], skipFrames: 0)
        let eventId = try XCTUnwrap(client.lastEventId)
        let f = Vaka.Feedback(message: "  It broke  ", email: " ann@example.com ", eventId: eventId, source: .widget, tags: ["k": "v"])
        let item = client.feedbackItem(f, message: "It broke", timestamp: Date())
        XCTAssertEqual(item["type"] as? String, "feedback")
        XCTAssertEqual(item["message"] as? String, "It broke")
        XCTAssertEqual(item["email"] as? String, "ann@example.com")
        XCTAssertEqual(item["event_id"] as? String, eventId)
        XCTAssertEqual(item["source"] as? String, "widget")
        XCTAssertEqual(item["screen"] as? String, "Checkout")
        XCTAssertEqual(item["user_id"] as? String, "u-1")
        XCTAssertEqual(item["release"] as? String, "1.0.0")
        XCTAssertEqual(item["tags"] as? [String: String], ["plan": "pro", "k": "v"])
        XCTAssertNotNil(item["session_id"])
        XCTAssertNotNil(item["id"])
        XCTAssertNil(item["name"])
    }

    func testMultipartBody() throws {
        let png = Data([0x89, 0x50, 0x4e, 0x47, 1, 2, 3])
        let body = FeedbackSender.multipart(item: Data(#"{"message":"hi"}"#.utf8), attachments: [.png(png), .jpeg(Data([0xff, 0xd8]))], boundary: "B")
        let text = String(decoding: body, as: UTF8.self)
        XCTAssertTrue(text.hasPrefix("--B\r\nContent-Disposition: form-data; name=\"feedback\"\r\nContent-Type: application/json\r\n\r\n{\"message\":\"hi\"}\r\n"), text)
        XCTAssertTrue(text.contains("name=\"screenshot\"; filename=\"screenshot.png\"\r\nContent-Type: image/png\r\n\r\n"))
        XCTAssertTrue(text.contains("filename=\"screenshot.jpg\"\r\nContent-Type: image/jpeg"))
        XCTAssertTrue(text.hasSuffix("\r\n--B--\r\n"))
    }

    func testAttachmentLimits() {
        let big = Vaka.Attachment.jpeg(Data(count: Vaka.Attachment.maxSize + 1))
        let ok = Vaka.Attachment.jpeg(Data([1]))
        let heic = Vaka.Attachment(data: Data([1]), contentType: "image/heic")
        let out = FeedbackSender.validAttachments([big, ok, heic, ok, ok, ok], log: { _ in })
        XCTAssertEqual(out.count, 3)
    }

    func testEmptyMessageIsRejected() {
        let client = Client(options: options(tmpDir()))
        let done = expectation(description: "completion")
        client.captureFeedback(Vaka.Feedback(message: " \n ")) { r in
            if case .failure(.emptyMessage) = r {} else { XCTFail("\(r)") }
            done.fulfill()
        }
        wait(for: [done], timeout: 5)
    }

    func testOfflineFeedbackIsQueuedWithAttachments() throws {
        let dir = tmpDir()
        let client = Client(options: options(dir))
        client.start()
        let done = expectation(description: "completion")
        var result: Vaka.FeedbackResult?
        client.captureFeedback(Vaka.Feedback(message: "offline", attachments: [.jpeg(Data([0xff, 0xd8, 0xff]))])) { r in
            result = try? r.get()
            done.fulfill()
        }
        wait(for: [done], timeout: 30)
        XCTAssertEqual(result?.queued, true)
        XCTAssertNil(result?.shortId)
        XCTAssertEqual(client.feedback.pendingCount, 1)
        let saved = try XCTUnwrap(try FileManager.default.contentsOfDirectory(at: dir.appendingPathComponent("feedback"), includingPropertiesForKeys: nil).first)
        let item = try JSONSerialization.jsonObject(with: Data(contentsOf: saved.appendingPathComponent("item.json"))) as? [String: Any]
        XCTAssertEqual(item?["message"] as? String, "offline")
        XCTAssertEqual(try Data(contentsOf: saved.appendingPathComponent("0.bin")), Data([0xff, 0xd8, 0xff]))

        // Still offline: a retry keeps it.
        let retried = expectation(description: "retry")
        client.feedback.sendPending { retried.fulfill() }
        wait(for: [retried], timeout: 30)
        XCTAssertEqual(client.feedback.pendingCount, 1)
    }

    func testPreviousCrashIsOfferedForFeedback() throws {
        let dir = tmpDir()
        _ = vaka_test_fork_crash(dir.appendingPathComponent("crash.txt").path, SIGSEGV)
        let client = Client(options: options(dir))
        client.start()
        let got = expectation(description: "crash report")
        client.addCrashHandler { r in
            XCTAssertEqual(r.kind, .crash)
            XCTAssertTrue(r.previousRun)
            XCTAssertFalse(r.eventId.isEmpty)
            got.fulfill()
        }
        wait(for: [got], timeout: 5)
    }

    func testKilledWhileHungIsOfferedAsHang() throws {
        let dir = tmpDir()
        let scope: [String: Any] = [
            "release": "1.0.0", "environment": "test", "breadcrumbs": [], "tags": [:],
            "session": ["id": "s", "started": Client.ms(Date().addingTimeInterval(-60)), "errors": 1],
            "hang_since": Client.ms(Date().addingTimeInterval(-5)), "hang_event_id": "hang-event",
        ]
        try JSONSerialization.data(withJSONObject: scope).write(to: dir.appendingPathComponent("scope.json"))
        let client = Client(options: options(dir))
        client.start()
        let got = expectation(description: "hang report")
        client.addCrashHandler { r in
            XCTAssertEqual(r.kind, .hang)
            XCTAssertEqual(r.eventId, "hang-event")
            got.fulfill()
        }
        wait(for: [got], timeout: 5)
    }
}

final class FeedbackMockServerTests: XCTestCase {
    /// Run with VAKA_TEST_MOCK=http://127.0.0.1:8765 and a server that answers 503 once, then 202.
    func testQueuedThenSent() throws {
        guard let ep = ProcessInfo.processInfo.environment["VAKA_TEST_MOCK"] else { throw XCTSkip("VAKA_TEST_MOCK not set") }
        let dir = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("vaka-mock-\(UUID().uuidString)")
        let client = Client(options: Vaka.Options(key: "vk_mock", endpoint: URL(string: ep)!, release: "1.0.0", crashReporting: false,
                                                  appHangDetection: false, flushInterval: 3600, storageDirectory: dir))
        let first = expectation(description: "first")
        client.captureFeedback(Vaka.Feedback(message: "with image", attachments: [.png(Data([0x89, 0x50, 0x4e, 0x47]))])) { r in
            XCTAssertEqual(try? r.get().queued, true)
            first.fulfill()
        }
        wait(for: [first], timeout: 30)
        let resent = expectation(description: "resent")
        client.feedback.sendPending { resent.fulfill() }
        wait(for: [resent], timeout: 30)
        XCTAssertEqual(client.feedback.pendingCount, 0)
        let second = expectation(description: "second")
        client.captureFeedback(Vaka.Feedback(message: "plain")) { r in
            let res = try? r.get()
            XCTAssertEqual(res?.shortId, "IOS-F1")
            XCTAssertEqual(res?.id, "srv-1")
            second.fulfill()
        }
        wait(for: [second], timeout: 30)
    }
}

final class IntegrationTests: XCTestCase {
    func testSendsToServer() throws {
        let env = ProcessInfo.processInfo.environment
        guard let ep = env["VAKA_TEST_ENDPOINT"], let key = env["VAKA_TEST_KEY"] else {
            throw XCTSkip("VAKA_TEST_ENDPOINT / VAKA_TEST_KEY not set")
        }
        let dir = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("vaka-it-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        // A crash report from a "previous run", so the next start sends it.
        let crash = dir.appendingPathComponent("crash.txt")
        _ = vaka_test_fork_crash(crash.path, SIGSEGV)
        let prevSession = UUID().uuidString.lowercased()
        let scope: [String: Any] = [
            "release": "1.0.0-it", "environment": "test",
            "breadcrumbs": [["timestamp": Client.ms(Date()), "category": "test", "message": "before crash", "level": "info"]],
            "session": ["id": prevSession, "started": Client.ms(Date().addingTimeInterval(-60)), "errors": 0],
            "user_id": "it-user",
        ]
        try JSONSerialization.data(withJSONObject: scope).write(to: dir.appendingPathComponent("scope.json"))

        let client = Client(options: Vaka.Options(key: key, endpoint: URL(string: ep)!, release: "1.0.0-it", environment: "test",
                                                  crashReporting: false, appHangDetection: false, flushInterval: 3600, debug: true, storageDirectory: dir))
        client.start()
        client.screen("Checkout", props: ["step": 2])
        client.track("purchase", props: ["plan": "pro", "amount": 9.5])
        client.addBreadcrumb(category: "cart", message: "Added item 42", level: .info, data: [:])
        enum CartError: Error { case empty }
        client.capture(error: CartError.empty, level: .error, tags: ["flavor": "it"], skipFrames: 0)
        client.capture(message: "hello from the iOS SDK test", level: .warning, tags: [:], skipFrames: 0)
        XCTAssertTrue(client.flushAndWait(timeout: 20))
        XCTAssertEqual(client.pendingCount, 0, "batches left in the queue: server did not accept them")
        XCTAssertFalse(FileManager.default.fileExists(atPath: crash.path))
    }
}
