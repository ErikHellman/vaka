import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
import VakaNative

/// Disk-backed batch queue. Every batch is written to `pending/` before it is sent and deleted
/// only after the server accepted (or permanently rejected) it, so nothing is lost when the app
/// is killed or offline.
final class Transport {
    private let endpoint: URL
    private let key: String
    private let dir: URL
    private let log: (String) -> Void
    private let q = DispatchQueue(label: "se.hellsoft.vaka.transport")
    private let session: URLSession
    private var sending = false
    private var disabled = false
    private var backoff: TimeInterval = 0
    private var retryScheduled = false
    static let maxPendingFiles = 200
    static let maxItemsPerRequest = 1000

    init(endpoint: URL, key: String, directory: URL, log: @escaping (String) -> Void) {
        self.endpoint = endpoint
        self.key = key
        self.dir = directory.appendingPathComponent("pending", isDirectory: true)
        self.log = log
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 30
        config.httpMaximumConnectionsPerHost = 1
        self.session = URLSession(configuration: config)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    }

    /// Write a batch to disk synchronously. Safe to call from any thread.
    @discardableResult
    func persist(_ items: [[String: Any]]) -> Bool {
        guard !items.isEmpty else { return true }
        var ok = true
        for start in stride(from: 0, to: items.count, by: Transport.maxItemsPerRequest) {
            let chunk = Array(items[start..<min(start + Transport.maxItemsPerRequest, items.count)])
            ok = write(chunk) && ok
        }
        trim()
        return ok
    }

    private func write(_ items: [[String: Any]]) -> Bool {
        let body: [String: Any] = ["sdk": ["name": Vaka.sdkName, "version": Vaka.sdkVersion], "items": items]
        guard let data = try? JSONSerialization.data(withJSONObject: body) else {
            log("could not encode batch")
            return false
        }
        let name = String(format: "%013.0f-%@.json", Date().timeIntervalSince1970 * 1000, UUID().uuidString)
        do {
            try data.write(to: dir.appendingPathComponent(name), options: .atomic)
            return true
        } catch {
            log("could not write batch: \(error)")
            return false
        }
    }

    private func pendingFiles() -> [URL] {
        let files = (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? []
        return files.filter { $0.pathExtension == "json" }.sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    var pendingCount: Int { pendingFiles().count }

    private func trim() {
        let files = pendingFiles()
        if files.count > Transport.maxPendingFiles {
            for f in files.prefix(files.count - Transport.maxPendingFiles) { try? FileManager.default.removeItem(at: f) }
        }
    }

    /// Send pending batches, oldest first. `completion` runs when the queue is drained or
    /// sending stopped (offline, backoff, disabled).
    func sendPending(completion: (() -> Void)? = nil) {
        q.async {
            guard !self.sending, !self.disabled, !self.retryScheduled else { completion?(); return }
            self.sending = true
            self.sendNext(completion: completion)
        }
    }

    private func sendNext(completion: (() -> Void)?) {
        guard let file = pendingFiles().first, let data = try? Data(contentsOf: file) else {
            sending = false
            completion?()
            return
        }
        var req = URLRequest(url: endpoint.appendingPathComponent("ingest/v1/batch"))
        req.httpMethod = "POST"
        req.setValue(key, forHTTPHeaderField: "X-Vaka-Key")
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("\(Vaka.sdkName)/\(Vaka.sdkVersion)", forHTTPHeaderField: "X-Vaka-Sdk")
        if let gz = gzip(data) {
            req.setValue("gzip", forHTTPHeaderField: "Content-Encoding")
            req.httpBody = gz
        } else {
            req.httpBody = data
        }
        session.dataTask(with: req) { body, response, error in
            self.q.async {
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                self.handle(status: status, error: error, body: body, file: file, data: data, completion: completion)
            }
        }.resume()
    }

    private func handle(status: Int, error: Error?, body: Data?, file: URL, data: Data, completion: (() -> Void)?) {
        let message = body.flatMap { String(data: $0, encoding: .utf8) } ?? ""
        switch status {
        case 200..<300:
            backoff = 0
            try? FileManager.default.removeItem(at: file)
            if message.contains("\"rejected\":[{") { log("server rejected items: \(message)") }
            sendNext(completion: completion)
        case 401, 403:
            log("ingest key rejected (\(status)); not sending until the app restarts: \(message)")
            disabled = true
            sending = false
            completion?()
        case 413:
            split(file: file, data: data)
            sendNext(completion: completion)
        case 429, 500..<600, 0:
            backoff = backoff == 0 ? 5 : min(backoff * 2, 300)
            log("send failed (\(status == 0 ? (error.map { "\($0)" } ?? "network") : "HTTP \(status)")); retrying in \(Int(backoff)) s")
            sending = false
            retryScheduled = true
            q.asyncAfter(deadline: .now() + backoff) {
                self.retryScheduled = false
                self.sendPending()
            }
            completion?()
        default:
            log("batch dropped (HTTP \(status)): \(message)")
            try? FileManager.default.removeItem(at: file)
            sendNext(completion: completion)
        }
    }

    private func split(file: URL, data: Data) {
        defer { try? FileManager.default.removeItem(at: file) }
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let items = obj["items"] as? [[String: Any]], items.count > 1 else {
            log("batch too large to send; dropped")
            return
        }
        let mid = items.count / 2
        _ = write(Array(items[..<mid]))
        _ = write(Array(items[mid...]))
    }

    private func gzip(_ data: Data) -> Data? {
        let cap = vaka_gzip_bound(data.count)
        var out = Data(count: cap)
        let n = out.withUnsafeMutableBytes { (dst: UnsafeMutableRawBufferPointer) -> Int in
            data.withUnsafeBytes { (src: UnsafeRawBufferPointer) -> Int in
                vaka_gzip(src.bindMemory(to: UInt8.self).baseAddress, data.count,
                          dst.bindMemory(to: UInt8.self).baseAddress, cap)
            }
        }
        guard n > 0 else { return nil }
        out.count = n
        return out
    }
}
