import Foundation
import VakaNative

/// Stack traces, loaded images and crash reports, mapped to the ingest wire format.
enum Native {
    struct Image {
        let addr: UInt64
        let size: UInt64
        let kind: Int32
        let id: [UInt8]
        let path: String

        var json: [String: Any] {
            var d: [String: Any] = [
                "code_file": path,
                "image_addr": hex(addr),
                "image_size": size,
            ]
            if kind == VAKA_IMAGE_MACHO {
                d["type"] = "macho"
                if id.count == 16 { d["debug_id"] = uuidString(id) }
            } else {
                d["type"] = "elf"
                if !id.isEmpty { d["code_id"] = id.map { String(format: "%02x", $0) }.joined() }
            }
            return d
        }

        func contains(_ a: UInt64) -> Bool { a >= addr && a < addr + size }
    }

    static func hex(_ v: UInt64) -> String { "0x" + String(v, radix: 16) }

    static func uuidString(_ b: [UInt8]) -> String {
        let h = b.map { String(format: "%02x", $0) }.joined()
        let c = Array(h)
        return [c[0..<8], c[8..<12], c[12..<16], c[16..<20], c[20..<32]].map { String($0) }.joined(separator: "-")
    }

    static func loadedImages() -> [Image] {
        vaka_images_init()
        var out: [Image] = []
        let n = vaka_image_count()
        out.reserveCapacity(Int(n))
        for i in 0..<n {
            var img = vaka_image_t()
            guard vaka_image_get(i, &img) == 0 else { continue }
            let id = withUnsafeBytes(of: img.id) { Array($0.prefix(Int(img.id_len))) }
            out.append(Image(addr: img.addr, size: img.size, kind: img.kind, id: id,
                             path: img.path.map { String(cString: $0) } ?? ""))
        }
        return out
    }

    /// Return addresses of the calling thread. `skip` drops the caller's own frames.
    @inline(never)
    static func captureStack(skip: Int) -> [UInt64] {
        // Called directly (no closure) so the frame count to skip is exact: +1 for this function.
        let buf = UnsafeMutablePointer<UInt>.allocate(capacity: 128)
        defer { buf.deallocate() }
        let n = Int(vaka_capture_stack(buf, 128, Int32(skip + 1)))
        return (0..<n).map { UInt64(buf[$0]) }
    }

    static let appPaths: [String] = {
        var paths = [normalize(Bundle.main.bundlePath)]
        if let exe = Bundle.main.executablePath { paths.append(normalize(exe)) }
        return paths
    }()

    static func normalize(_ p: String) -> String {
        p.hasPrefix("/private/") ? String(p.dropFirst("/private".count)) : p
    }

    static func isInApp(_ image: Image?) -> Bool {
        guard let image else { return false }
        let p = normalize(image.path)
        return appPaths.contains { p == $0 || p.hasPrefix($0 + "/") }
    }

    /// Function name for `addr` in this process, demangled. Best effort: release builds are
    /// stripped, so the server's dSYM symbolication is what gives reliable names and lines.
    static func symbolName(_ addr: UInt64) -> String? {
        guard addr > 0, let cname = vaka_symbol_name(UInt(addr), nil) else { return nil }
        let name = String(cString: cname)
        return demangle(name) ?? name
    }

    static func demangle(_ name: String) -> String? {
        var mangled = name
        if mangled.hasPrefix("_$s") || mangled.hasPrefix("_$S") { mangled.removeFirst() }
        guard mangled.hasPrefix("$s") || mangled.hasPrefix("$S") || mangled.hasPrefix("_T") else { return nil }
        return mangled.withCString { ptr -> String? in
            guard let out = _swift_demangle(ptr, UInt(strlen(ptr)), nil, nil, 0) else { return nil }
            defer { free(out) }
            return String(cString: out)
        }
    }

    /// Frames (crash site first) plus the images they live in.
    /// `resolveFrom` maps addresses from a previous process (crash report) onto images loaded now,
    /// so function names can still be looked up in-process.
    static func frames(_ addrs: [UInt64], images: [Image], current: [Image]? = nil) -> ([[String: Any]], [[String: Any]]) {
        var used: [String: [String: Any]] = [:]
        var order: [String] = []
        var frames: [[String: Any]] = []
        let now = current ?? images
        for (i, a) in addrs.enumerated() {
            var f: [String: Any] = ["instruction_addr": hex(a)]
            let img = images.filter { $0.contains(a) }.max { $0.addr < $1.addr }
            f["in_app"] = isInApp(img)
            if let img {
                f["package"] = img.path
                f["image_addr"] = hex(img.addr)
                let key = hex(img.addr)
                if used[key] == nil { used[key] = img.json; order.append(key) }
                // Same binary in this process (matched by id): look the symbol up after rebasing.
                if let live = now.first(where: { $0.size == img.size && (img.id.isEmpty ? $0.path == img.path : $0.id == img.id) }) {
                    // Return addresses point after the call; look up the call instruction.
                    let rebased = a - img.addr + live.addr - (i == 0 ? 0 : 1)
                    if let name = symbolName(rebased) { f["function"] = name }
                }
            }
            frames.append(f)
        }
        return (frames, order.compactMap { used[$0] })
    }

    // MARK: crash reports

    struct CrashReport {
        var time: Date
        var signal: Int32?
        var signalName: String?
        var code: Int?
        var faultAddr: UInt64?
        var exceptionType: String?
        var exceptionReason: String?
        var frames: [UInt64] = []
        var images: [Image] = []

        var exception: (type: String, value: String) {
            if let t = exceptionType { return (t, exceptionReason ?? "") }
            let sig = signalName ?? "SIGNAL"
            let addr = faultAddr.map { " at \(Native.hex($0))" } ?? ""
            switch sig {
            case "SIGSEGV", "SIGBUS": return ("EXC_BAD_ACCESS", "\(sig)\(addr)")
            case "SIGILL": return ("EXC_BAD_INSTRUCTION", sig)
            case "SIGTRAP": return ("EXC_BREAKPOINT", "\(sig) (Swift runtime failure, e.g. force unwrap of nil or index out of range)")
            case "SIGFPE": return ("EXC_ARITHMETIC", sig)
            case "SIGABRT": return ("SIGABRT", "abort() called")
            default: return ("EXC_CRASH", sig)
            }
        }
    }

    /// Parse the line-based report written by crash.c. Returns nil for empty/truncated files.
    static func parseReport(_ text: String) -> CrashReport? {
        var lines = text.split(separator: "\n", omittingEmptySubsequences: true).map(String.init)
        guard lines.first == "vaka-crash 1" else { return nil }
        lines.removeFirst()
        var r = CrashReport(time: Date())
        for line in lines {
            let parts = line.split(separator: " ", maxSplits: 1).map(String.init)
            guard parts.count == 2 else { continue }
            let (k, v) = (parts[0], parts[1])
            switch k {
            case "time": if let t = Double(v) { r.time = Date(timeIntervalSince1970: t) }
            case "signal":
                let sp = v.split(separator: " ")
                r.signal = sp.first.flatMap { Int32($0) }
                r.signalName = sp.count > 1 ? String(sp[1]) : nil
            case "code": r.code = Int(v)
            case "fault_addr": r.faultAddr = parseHex(v)
            case "exception_type": r.exceptionType = v
            case "exception_reason": r.exceptionReason = v
            case "frame": if let a = parseHex(v) { r.frames.append(a) }
            case "image":
                let sp = v.split(separator: " ", maxSplits: 4).map(String.init)
                guard sp.count >= 4, let addr = parseHex(sp[0]), let size = parseHex(sp[1]) else { continue }
                let id: [UInt8] = sp[3] == "-" ? [] : stride(from: 0, to: sp[3].count - 1, by: 2).compactMap {
                    let s = sp[3].index(sp[3].startIndex, offsetBy: $0)
                    return UInt8(sp[3][s..<sp[3].index(s, offsetBy: 2)], radix: 16)
                }
                r.images.append(Image(addr: addr, size: size, kind: sp[2] == "macho" ? VAKA_IMAGE_MACHO : VAKA_IMAGE_ELF,
                                      id: id, path: sp.count > 4 ? sp[4] : ""))
            default: break
            }
        }
        // A report without frames is from a handler that died mid-write; still worth sending.
        return r
    }

    static func parseHex(_ s: String) -> UInt64? {
        UInt64(s.hasPrefix("0x") ? String(s.dropFirst(2)) : s, radix: 16)
    }
}

@_silgen_name("swift_demangle")
private func _swift_demangle(
    _ mangledName: UnsafePointer<CChar>?,
    _ mangledNameLength: UInt,
    _ outputBuffer: UnsafeMutablePointer<CChar>?,
    _ outputBufferSize: UnsafeMutablePointer<UInt>?,
    _ flags: UInt32
) -> UnsafeMutablePointer<CChar>?
