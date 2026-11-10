package se.hellsoft.vaka.internal

import java.util.IdentityHashMap

/** Turns a Throwable into the ingest `exceptions` array (outermost first, crash site first). */
internal class ErrorBuilder(inAppPackages: List<String>) {
    private val inApp = inAppPackages.filter { it.isNotBlank() }.map { it.trimEnd('.') + "." }

    fun exceptions(t: Throwable): List<Map<String, Any?>> {
        val seen = IdentityHashMap<Throwable, Boolean>()
        val out = ArrayList<Map<String, Any?>>()
        var cur: Throwable? = t
        while (cur != null && seen.put(cur, true) == null && out.size < MAX_CAUSES) {
            out.add(exception(cur))
            cur = cur.cause
        }
        return out
    }

    private fun exception(t: Throwable): Map<String, Any?> {
        val cls = t.javaClass.name
        return linkedMapOf(
            "type" to cls,
            "value" to (t.message ?: ""),
            "module" to cls.substringBeforeLast('.', ""),
            "stacktrace" to t.stackTrace.take(MAX_FRAMES).map(::frame),
        )
    }

    fun frame(e: StackTraceElement): Map<String, Any?> = linkedMapOf<String, Any?>(
        "function" to e.methodName,
        "module" to e.className,
        "filename" to e.fileName,
        "lineno" to e.lineNumber.takeIf { it > 0 },
        "in_app" to isInApp(e.className),
    ).also { if (e.isNativeMethod) it["function"] = e.methodName + " (native)" }

    fun isInApp(className: String): Boolean {
        if (inApp.any { className.startsWith(it) }) return true
        // Framework classes are never obfuscated; anything else (including R8-renamed classes
        // like `a.b`) is treated as app code.
        return SYSTEM_PREFIXES.none { className.startsWith(it) }
    }

    companion object {
        const val MAX_CAUSES = 10
        const val MAX_FRAMES = 200
        val SYSTEM_PREFIXES = listOf(
            "java.", "javax.", "jdk.", "sun.", "libcore.", "dalvik.", "android.", "androidx.",
            "com.android.", "com.google.android.", "kotlin.", "kotlinx.", "org.json.", "org.apache.",
            "okhttp3.", "okio.", "retrofit2.", "io.reactivex.", "se.hellsoft.vaka.",
        )
    }
}
