import Foundation
import VakaNative

/// Detects app hangs: a watchdog thread pings the main queue and, when the ping is not answered
/// within `threshold`, samples the main thread's stack and reports it once per hang.
final class HangWatchdog {
    let threshold: TimeInterval
    private let isPaused: () -> Bool
    private let onHang: ([UInt64]) -> Void
    private let onRecover: (TimeInterval) -> Void
    private let lock = NSLock()
    private var running = false

    init(threshold: TimeInterval, isPaused: @escaping () -> Bool,
         onHang: @escaping ([UInt64]) -> Void, onRecover: @escaping (TimeInterval) -> Void) {
        self.threshold = threshold
        self.isPaused = isPaused
        self.onHang = onHang
        self.onRecover = onRecover
    }

    /// Call on the main thread if possible; otherwise the sampler is installed on the next main-queue turn.
    func start() {
        if Thread.isMainThread {
            _ = vaka_sampler_install_main_thread()
        } else {
            DispatchQueue.main.async { _ = vaka_sampler_install_main_thread() }
        }
        lock.lock()
        defer { lock.unlock() }
        guard !running else { return }
        running = true
        let t = Thread { [weak self] in self?.run() }
        t.name = "se.hellsoft.vaka.hang-watchdog"
        t.qualityOfService = .utility
        t.start()
    }

    func stop() {
        lock.lock()
        running = false
        lock.unlock()
    }

    private var isRunning: Bool {
        lock.lock()
        defer { lock.unlock() }
        return running
    }

    private func run() {
        // Between pings: short enough to notice a hang soon after it starts, long enough to stay cheap.
        let idle = min(0.5, threshold / 2)
        while isRunning {
            if isPaused() {
                Thread.sleep(forTimeInterval: idle)
                continue
            }
            let answered = DispatchSemaphore(value: 0)
            let sent = DispatchTime.now()
            DispatchQueue.main.async { answered.signal() }
            if answered.wait(timeout: sent + threshold) == .success {
                Thread.sleep(forTimeInterval: idle)
                continue
            }
            // A paused debugger or a suspended app looks like a hang; neither is one.
            if !isRunning || isPaused() || HangWatchdog.debuggerAttached() {
                answered.wait()
                continue
            }
            onHang(HangWatchdog.sampleMainThread())
            answered.wait()
            let elapsed = Double(DispatchTime.now().uptimeNanoseconds - sent.uptimeNanoseconds) / 1e9
            onRecover(elapsed)
        }
    }

    static func sampleMainThread() -> [UInt64] {
        let cap = 128
        let buf = UnsafeMutablePointer<UInt>.allocate(capacity: cap)
        defer { buf.deallocate() }
        let n = Int(vaka_sample_main_thread(buf, Int32(cap), 200))
        return n > 0 ? (0..<n).map { UInt64(buf[$0]) } : []
    }

    static func debuggerAttached() -> Bool {
        #if canImport(Darwin)
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, 4, &info, &size, nil, 0) == 0 else { return false }
        return (info.kp_proc.p_flag & P_TRACED) != 0
        #else
        return false
        #endif
    }
}
