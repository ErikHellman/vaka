package se.hellsoft.vaka.internal

import java.io.File

/**
 * A report written by the native signal handler (`src/main/cpp/vaka_crash.c`), read on the next
 * launch. Addresses stay raw; the server symbolicates them with uploaded debug files.
 */
internal class NativeCrashReport(
    val timestampMillis: Long,
    val signal: Int,
    val code: Int,
    val faultAddr: Long,
    val tid: Long,
    val thread: String?,
    val sessionId: String?,
    val release: String?,
    val frames: List<Long>,
    val images: List<Image>,
) {
    class Image(val start: Long, val size: Long, val buildId: String?, val path: String) {
        operator fun contains(addr: Long): Boolean =
            java.lang.Long.compareUnsigned(addr, start) >= 0 && java.lang.Long.compareUnsigned(addr - start, size) < 0
    }

    /** The ingest `exceptions` entry, crash site first. */
    fun exception(): Map<String, Any?> {
        val name = SIGNAL_NAMES[signal] ?: "SIG$signal"
        val codeName = codeName()
        val value = StringBuilder(SIGNAL_DESCRIPTIONS[signal] ?: "Fatal signal $signal")
        if (codeName != null) value.append(" (").append(codeName).append(')')
        if (hasFaultAddr()) value.append(" at ").append(hex(faultAddr))
        return linkedMapOf(
            "type" to name,
            "value" to value.toString(),
            "stacktrace" to frames.map { addr ->
                val image = images.firstOrNull { addr in it }
                linkedMapOf<String, Any?>(
                    "instruction_addr" to hex(addr),
                    // Until the server symbolicates it: `libapp.so+0x13b2`, stable across runs
                    // (unlike the address, which moves with ASLR), so grouping still works.
                    "function" to image?.let { it.path.substringAfterLast('/') + "+" + hex(addr - it.start) },
                    "package" to image?.path,
                    "in_app" to (image != null && isAppImage(image.path)),
                )
            },
        )
    }

    /** ELF images that contain at least one frame, as ingest `debug_images`. */
    fun debugImages(): List<Map<String, Any?>> = images
        .filter { img -> frames.any { it in img } }
        .map { img ->
            linkedMapOf<String, Any?>(
                "type" to "elf",
                "code_id" to img.buildId,
                "code_file" to img.path,
                "image_addr" to hex(img.start),
                "image_size" to img.size,
            )
        }

    private fun hasFaultAddr() = code > 0 && signal in FAULT_SIGNALS

    private fun codeName(): String? = when {
        code == SI_USER -> "SI_USER"
        code == SI_QUEUE -> "SI_QUEUE"
        code == SI_TKILL -> "SI_TKILL"
        code <= 0 -> null
        else -> CODE_NAMES[signal]?.getOrNull(code - 1)
    }

    companion object {
        const val SUFFIX = ".crash"
        private const val HEADER = "vaka-native-crash 1"
        private const val SI_USER = 0
        private const val SI_QUEUE = -1
        private const val SI_TKILL = -6
        private val FAULT_SIGNALS = setOf(4, 7, 8, 11) // ILL, BUS, FPE, SEGV

        private val SIGNAL_NAMES = mapOf(
            4 to "SIGILL", 5 to "SIGTRAP", 6 to "SIGABRT", 7 to "SIGBUS", 8 to "SIGFPE", 11 to "SIGSEGV", 31 to "SIGSYS",
        )
        private val SIGNAL_DESCRIPTIONS = mapOf(
            4 to "Illegal instruction", 5 to "Trace/breakpoint trap", 6 to "Abort", 7 to "Bus error",
            8 to "Floating-point exception", 11 to "Segmentation violation", 31 to "Bad system call",
        )
        private val CODE_NAMES = mapOf(
            4 to listOf("ILL_ILLOPC", "ILL_ILLOPN", "ILL_ILLADR", "ILL_ILLTRP", "ILL_PRVOPC", "ILL_PRVREG", "ILL_COPROC", "ILL_BADSTK"),
            7 to listOf("BUS_ADRALN", "BUS_ADRERR", "BUS_OBJERR", "BUS_MCEERR_AR", "BUS_MCEERR_AO"),
            8 to listOf("FPE_INTDIV", "FPE_INTOVF", "FPE_FLTDIV", "FPE_FLTOVF", "FPE_FLTUND", "FPE_FLTRES", "FPE_FLTINV", "FPE_FLTSUB"),
            11 to listOf("SEGV_MAPERR", "SEGV_ACCERR", "SEGV_BNDERR", "SEGV_PKUERR", "SEGV_ACCADI", "SEGV_ADIDERR", "SEGV_ADIPERR", "SEGV_MTEAERR", "SEGV_MTESERR"),
            5 to listOf("TRAP_BRKPT", "TRAP_TRACE", "TRAP_BRANCH", "TRAP_HWBKPT"),
        )

        fun hex(v: Long): String = "0x" + java.lang.Long.toHexString(v)

        /** Code shipped with the app (extracted libs or mapped straight from the APK), not the OS. */
        fun isAppImage(path: String): Boolean =
            path.startsWith("/data/") || path.contains(".apk!/") || path.endsWith(".apk")

        /** Parses a report; null if it is not one. A report cut short (no `end`) is still used. */
        fun parse(text: String): NativeCrashReport? {
            val lines = text.lineSequence().iterator()
            if (!lines.hasNext() || lines.next().trim() != HEADER) return null
            var ts = 0L
            var signal = -1
            var code = 0
            var fault = 0L
            var tid = 0L
            var thread: String? = null
            var session: String? = null
            var release: String? = null
            val frames = ArrayList<Long>()
            val images = ArrayList<Image>()
            for (line in lines) {
                val key = line.substringBefore(' ')
                val rest = line.substringAfter(' ', "")
                when (key) {
                    "timestamp_ms" -> ts = rest.toLongOrNull() ?: 0
                    "signal" -> signal = rest.toIntOrNull() ?: -1
                    "code" -> code = rest.toIntOrNull() ?: 0
                    "fault_addr" -> fault = parseHex(rest) ?: 0
                    "tid" -> tid = rest.toLongOrNull() ?: 0
                    "thread" -> thread = rest.ifBlank { null }
                    "session" -> session = rest.takeIf { SESSION_ID.matches(it) }
                    "release" -> release = rest.ifBlank { null }
                    "frame" -> parseHex(rest)?.let(frames::add)
                    "image" -> {
                        val parts = rest.split(' ', limit = 4)
                        if (parts.size == 4) {
                            val start = parseHex(parts[0])
                            val size = parseHex(parts[1])
                            if (start != null && size != null) {
                                images.add(Image(start, size, parts[2].takeIf { it != "-" && HEX.matches(it) }, parts[3]))
                            }
                        }
                    }
                }
            }
            if (signal < 0) return null
            return NativeCrashReport(ts, signal, code, fault, tid, thread, session, release, frames, images)
        }

        private val SESSION_ID = Regex("[0-9a-fA-F-]{8,64}")
        private val HEX = Regex("[0-9a-f]+")

        private fun parseHex(s: String): Long? {
            val t = s.trim().removePrefix("0x")
            return if (t.isEmpty() || t.length > 16) null else java.lang.Long.parseUnsignedLong(t, 16)
        }

        /** Reads and deletes every report in [dir]. */
        fun takeAll(dir: File, log: Logger = Logger.NONE): List<NativeCrashReport> {
            val files = dir.listFiles { f -> f.name.endsWith(SUFFIX) } ?: return emptyList()
            return files.sortedBy { it.name }.mapNotNull { f ->
                try {
                    parse(f.readText())
                } catch (e: Exception) {
                    log.warn("unreadable native crash report ${f.name}", e)
                    null
                } finally {
                    f.delete()
                }
            }
        }
    }
}
