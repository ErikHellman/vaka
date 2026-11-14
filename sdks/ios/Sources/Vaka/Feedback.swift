import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

extension Vaka {
    /// Feedback a user wrote, sent with `Vaka.captureFeedback(_:)` or by the built-in feedback form.
    public struct Feedback {
        public enum Source: String {
            case widget
            case crashPrompt = "crash_prompt"
            case api
        }

        /// What the user wrote. Required; at most 5000 characters are kept.
        public var message: String
        /// Contact email, only when the user typed it.
        public var email: String?
        public var name: String?
        /// The error event this feedback is about (see `Vaka.lastEventId`). Vaka links the feedback
        /// to that event's issue.
        public var eventId: String?
        public var source: Source
        public var tags: [String: String]
        /// Up to 3 PNG, JPEG or WebP images, 5 MB each.
        public var attachments: [Attachment]

        public init(message: String, email: String? = nil, name: String? = nil, eventId: String? = nil,
                    source: Source = .api, tags: [String: String] = [:], attachments: [Attachment] = []) {
            self.message = message
            self.email = email
            self.name = name
            self.eventId = eventId
            self.source = source
            self.tags = tags
            self.attachments = attachments
        }
    }

    /// An image sent with feedback.
    public struct Attachment {
        public static let maxSize = 5 * 1024 * 1024
        public static let maxCount = 3
        public static let maxPixels = 4096

        public var data: Data
        /// `image/png`, `image/jpeg` or `image/webp`.
        public var contentType: String
        public var filename: String

        public init(data: Data, contentType: String, filename: String? = nil) {
            self.data = data
            self.contentType = contentType
            let ext = contentType == "image/png" ? "png" : contentType == "image/webp" ? "webp" : "jpg"
            self.filename = filename ?? "screenshot.\(ext)"
        }

        public static func jpeg(_ data: Data, filename: String? = nil) -> Attachment {
            Attachment(data: data, contentType: "image/jpeg", filename: filename)
        }

        public static func png(_ data: Data, filename: String? = nil) -> Attachment {
            Attachment(data: data, contentType: "image/png", filename: filename)
        }
    }

    public struct FeedbackResult {
        /// Server id of the stored feedback, or the client id while it is still queued.
        public let id: String
        /// Short id such as `IOS-F12`; nil while queued.
        public let shortId: String?
        /// True when the device was offline (or the server unavailable): the feedback is saved
        /// on disk and sent on the next launch, foreground or `Vaka.flush()`.
        public let queued: Bool
    }

    public enum FeedbackError: Error, CustomStringConvertible {
        /// `Vaka.start` was not called, or the SDK is disabled.
        case notStarted
        /// The message is empty.
        case emptyMessage
        /// The per-device or per-session hourly limit was hit. Not retried.
        case rateLimited
        /// The server refused the feedback (bad request, unknown key).
        case rejected(status: Int, message: String)

        public var description: String {
            switch self {
            case .notStarted: return "Vaka is not started"
            case .emptyMessage: return "The feedback message is empty"
            case .rateLimited: return "Too much feedback from this device; try again later"
            case let .rejected(status, message): return "Feedback rejected (HTTP \(status)): \(message)"
            }
        }
    }

    /// A crash or app hang the user could tell you about. See `Vaka.onCrashOrHang(_:)`.
    public struct CrashReport {
        public enum Kind: String {
            /// The previous run crashed.
            case crash
            /// The main thread was blocked: either just now (the app recovered) or the previous run
            /// was killed while hung.
            case hang
        }

        public let kind: Kind
        /// The error event to pass as `Feedback.eventId`.
        public let eventId: String
        public let date: Date
        /// True when it happened in an earlier run of the app (the user has just relaunched it).
        public let previousRun: Bool
    }

    /// Send user feedback with the current session, user, release, screen and tags.
    ///
    /// `completion` runs on the main queue. Without a network the feedback is saved and sent later,
    /// and the result has `queued == true`.
    public static func captureFeedback(_ feedback: Feedback,
                                       completion: ((Result<FeedbackResult, FeedbackError>) -> Void)? = nil) {
        guard let c = client else {
            DispatchQueue.main.async { completion?(.failure(.notStarted)) }
            return
        }
        c.captureFeedback(feedback) { r in DispatchQueue.main.async { completion?(r) } }
    }

    /// Async variant of `captureFeedback(_:completion:)`.
    @discardableResult
    public static func captureFeedback(_ feedback: Feedback) async throws -> FeedbackResult {
        try await withCheckedThrowingContinuation { cont in
            captureFeedback(feedback) { cont.resume(with: $0) }
        }
    }

    /// Id of the last error, message or app hang reported in this run, e.g. to link feedback to it.
    public static var lastEventId: String? { client?.lastEventId }

    /// Called on the main queue when the user could tell you what happened before a crash or hang:
    /// at launch after the previous run crashed (or was killed while hung), and when the app recovers
    /// from a hang. A report from the previous run is delivered to the first handler registered.
    /// The built-in feedback form registers one when `FeedbackOptions.crashPrompt` is on.
    public static func onCrashOrHang(_ handler: @escaping (CrashReport) -> Void) {
        client?.addCrashHandler(handler)
    }
}

/// Builds feedback requests, sends them, and keeps the ones that could not be sent on disk.
final class FeedbackSender {
    private let endpoint: URL
    private let key: String
    private let dir: URL
    private let log: (String) -> Void
    private let q = DispatchQueue(label: "se.hellsoft.vaka.feedback")
    private let session: URLSession
    private var inFlight: Set<String> = []
    static let maxPending = 20
    static let maxMessage = 5000

    init(endpoint: URL, key: String, directory: URL, log: @escaping (String) -> Void) {
        self.endpoint = endpoint
        self.key = key
        self.dir = directory.appendingPathComponent("feedback", isDirectory: true)
        self.log = log
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 60
        session = URLSession(configuration: config)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    }

    /// Valid attachments (count, size and type limits applied), in order.
    static func validAttachments(_ list: [Vaka.Attachment], log: (String) -> Void) -> [Vaka.Attachment] {
        let types: Set<String> = ["image/png", "image/jpeg", "image/webp"]
        var out: [Vaka.Attachment] = []
        for a in list {
            if out.count == Vaka.Attachment.maxCount { log("feedback: more than 3 attachments; extra dropped"); break }
            guard types.contains(a.contentType) else { log("feedback: attachment type \(a.contentType) not supported; dropped"); continue }
            guard !a.data.isEmpty, a.data.count <= Vaka.Attachment.maxSize else { log("feedback: attachment over 5 MB; dropped"); continue }
            out.append(a)
        }
        return out
    }

    static func multipart(item: Data, attachments: [Vaka.Attachment], boundary: String) -> Data {
        var body = Data()
        func add(_ s: String) { body.append(s.data(using: .utf8)!) }
        add("--\(boundary)\r\nContent-Disposition: form-data; name=\"feedback\"\r\nContent-Type: application/json\r\n\r\n")
        body.append(item)
        add("\r\n")
        for (i, a) in attachments.enumerated() {
            let name = a.filename.replacingOccurrences(of: "\"", with: "").replacingOccurrences(of: "\r", with: "")
                .replacingOccurrences(of: "\n", with: "")
            add("--\(boundary)\r\nContent-Disposition: form-data; name=\"screenshot\"; filename=\"\(name.isEmpty ? "image-\(i + 1)" : name)\"\r\n")
            add("Content-Type: \(a.contentType)\r\n\r\n")
            body.append(a.data)
            add("\r\n")
        }
        add("--\(boundary)--\r\n")
        return body
    }

    func request(item: Data, attachments: [Vaka.Attachment]) -> URLRequest {
        var req = URLRequest(url: endpoint.appendingPathComponent("ingest/v1/feedback"))
        req.httpMethod = "POST"
        req.setValue(key, forHTTPHeaderField: "X-Vaka-Key")
        req.setValue("\(Vaka.sdkName)/\(Vaka.sdkVersion)", forHTTPHeaderField: "X-Vaka-Sdk")
        if attachments.isEmpty {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = item
        } else {
            let boundary = "vaka-\(UUID().uuidString)"
            req.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
            req.httpBody = FeedbackSender.multipart(item: item, attachments: attachments, boundary: boundary)
        }
        return req
    }

    /// Send now; on a network error or 5xx, save to disk and report `queued`.
    func send(item: [String: Any], attachments: [Vaka.Attachment],
              completion: @escaping (Result<Vaka.FeedbackResult, Vaka.FeedbackError>) -> Void) {
        let id = item["id"] as? String ?? UUID().uuidString.lowercased()
        guard let json = try? JSONSerialization.data(withJSONObject: item) else {
            completion(.failure(.rejected(status: 0, message: "could not encode feedback")))
            return
        }
        q.async {
            self.inFlight.insert(id)
            self.post(id: id, json: json, attachments: attachments) { outcome in
                self.q.async {
                    self.inFlight.remove(id)
                    switch outcome {
                    case let .done(r): completion(r)
                    case .retry:
                        self.save(id: id, json: json, attachments: attachments)
                        completion(.success(Vaka.FeedbackResult(id: id, shortId: nil, queued: true)))
                    }
                }
            }
        }
    }

    private enum Outcome {
        case done(Result<Vaka.FeedbackResult, Vaka.FeedbackError>)
        case retry
    }

    private func post(id: String, json: Data, attachments: [Vaka.Attachment], completion: @escaping (Outcome) -> Void) {
        session.dataTask(with: request(item: json, attachments: attachments)) { body, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let text = body.flatMap { String(data: $0, encoding: .utf8) } ?? ""
            switch status {
            case 200..<300:
                let obj = body.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
                let serverId = obj["id"] as? String ?? id
                completion(.done(.success(Vaka.FeedbackResult(id: serverId, shortId: obj["short_id"] as? String, queued: false))))
            case 429:
                self.log("feedback rate limited: \(text)")
                completion(.done(.failure(.rateLimited)))
            case 0, 500..<600:
                self.log("feedback send failed (\(status == 0 ? (error.map { "\($0)" } ?? "network") : "HTTP \(status)")); saved for later")
                completion(.retry)
            default:
                self.log("feedback rejected (HTTP \(status)): \(text)")
                completion(.done(.failure(.rejected(status: status, message: text))))
            }
        }.resume()
    }

    // MARK: disk queue

    private func save(id: String, json: Data, attachments: [Vaka.Attachment]) {
        let name = String(format: "%013.0f-%@", Date().timeIntervalSince1970 * 1000, FeedbackSender.safe(id))
        let d = dir.appendingPathComponent(name, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
            var meta: [[String: String]] = []
            for (i, a) in attachments.enumerated() {
                try a.data.write(to: d.appendingPathComponent("\(i).bin"), options: .atomic)
                meta.append(["content_type": a.contentType, "filename": a.filename])
            }
            try JSONSerialization.data(withJSONObject: meta).write(to: d.appendingPathComponent("attachments.json"), options: .atomic)
            // Written last: a directory without item.json is incomplete and gets cleaned up.
            try json.write(to: d.appendingPathComponent("item.json"), options: .atomic)
        } catch {
            log("could not save feedback: \(error)")
            try? FileManager.default.removeItem(at: d)
        }
        trim()
    }

    static func safe(_ s: String) -> String {
        String(s.unicodeScalars.filter { CharacterSet.alphanumerics.contains($0) || $0 == "-" }.prefix(64))
    }

    private func pendingDirs() -> [URL] {
        let all = (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? []
        return all.filter { $0.hasDirectoryPath || (try? $0.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    var pendingCount: Int { q.sync { pendingDirs().count } }

    private func trim() {
        let dirs = pendingDirs()
        if dirs.count > FeedbackSender.maxPending {
            for d in dirs.prefix(dirs.count - FeedbackSender.maxPending) { try? FileManager.default.removeItem(at: d) }
        }
    }

    /// Send saved feedback, oldest first. Stops at the first item that still can't be sent.
    func sendPending(completion: (() -> Void)? = nil) {
        q.async { self.sendNextPending(skip: [], completion: completion) }
    }

    private func sendNextPending(skip: Set<String>, completion: (() -> Void)?) {
        guard let d = pendingDirs().first(where: { !skip.contains($0.lastPathComponent) }) else { completion?(); return }
        guard let json = try? Data(contentsOf: d.appendingPathComponent("item.json")) else {
            // Incomplete write (killed mid-save); drop it unless it is brand new.
            let created = (try? FileManager.default.attributesOfItem(atPath: d.path)[.creationDate] as? Date) ?? .distantPast
            if Date().timeIntervalSince(created) > 60 { try? FileManager.default.removeItem(at: d) }
            sendNextPending(skip: skip.union([d.lastPathComponent]), completion: completion)
            return
        }
        let id = (try? JSONSerialization.jsonObject(with: json) as? [String: Any])?["id"] as? String ?? d.lastPathComponent
        if inFlight.contains(id) { sendNextPending(skip: skip.union([d.lastPathComponent]), completion: completion); return }
        let meta = (try? Data(contentsOf: d.appendingPathComponent("attachments.json")))
            .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [[String: String]] } ?? []
        var attachments: [Vaka.Attachment] = []
        for (i, m) in meta.enumerated() {
            if let data = try? Data(contentsOf: d.appendingPathComponent("\(i).bin")) {
                attachments.append(Vaka.Attachment(data: data, contentType: m["content_type"] ?? "image/jpeg", filename: m["filename"]))
            }
        }
        inFlight.insert(id)
        post(id: id, json: json, attachments: attachments) { outcome in
            self.q.async {
                self.inFlight.remove(id)
                switch outcome {
                case .done:
                    // Sent, or permanently refused: either way it leaves the queue.
                    try? FileManager.default.removeItem(at: d)
                    self.sendNextPending(skip: skip, completion: completion)
                case .retry:
                    completion?()
                }
            }
        }
    }
}
