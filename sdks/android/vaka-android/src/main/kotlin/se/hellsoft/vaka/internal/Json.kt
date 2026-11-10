package se.hellsoft.vaka.internal

/** Minimal JSON writer; the SDK only ever produces JSON, so no parser is needed. */
internal object Json {
    fun write(value: Any?): String = StringBuilder().also { append(it, value) }.toString()

    fun append(sb: StringBuilder, value: Any?) {
        when (value) {
            null -> sb.append("null")
            is String -> string(sb, value)
            is Boolean -> sb.append(value)
            is Double -> if (value.isNaN() || value.isInfinite()) sb.append("null") else sb.append(value)
            is Float -> append(sb, value.toDouble())
            is Number -> sb.append(value)
            is Map<*, *> -> {
                sb.append('{')
                var first = true
                for ((k, v) in value) {
                    if (!first) sb.append(',')
                    first = false
                    string(sb, k.toString())
                    sb.append(':')
                    append(sb, v)
                }
                sb.append('}')
            }
            is Iterable<*> -> {
                sb.append('[')
                var first = true
                for (v in value) {
                    if (!first) sb.append(',')
                    first = false
                    append(sb, v)
                }
                sb.append(']')
            }
            is Array<*> -> append(sb, value.asList())
            is RawJson -> sb.append(value.json)
            else -> string(sb, value.toString())
        }
    }

    private fun string(sb: StringBuilder, s: String) {
        sb.append('"')
        for (c in s) {
            when (c) {
                '"' -> sb.append("\\\"")
                '\\' -> sb.append("\\\\")
                '\n' -> sb.append("\\n")
                '\r' -> sb.append("\\r")
                '\t' -> sb.append("\\t")
                '\b' -> sb.append("\\b")
                '\u000C' -> sb.append("\\f")
                else -> if (c < ' ' || c == ' ' || c == ' ') {
                    sb.append("\\u").append(String.format("%04x", c.code))
                } else {
                    sb.append(c)
                }
            }
        }
        sb.append('"')
    }
}

/** Already-serialized JSON, embedded verbatim. */
internal class RawJson(val json: String)
