package se.hellsoft.vaka.internal

internal interface Logger {
    fun debug(msg: String)
    fun warn(msg: String, t: Throwable? = null)

    companion object {
        val NONE: Logger = object : Logger {
            override fun debug(msg: String) {}
            override fun warn(msg: String, t: Throwable?) {}
        }
    }
}

/** Key-value persistence (SharedPreferences on Android, a map in tests). */
internal interface KeyValueStore {
    fun getString(key: String): String?
    fun getLong(key: String): Long
    /** [commit] = write synchronously (used from the crash handler). */
    fun put(values: Map<String, Any?>, commit: Boolean = false)
}

internal class MemoryStore : KeyValueStore {
    private val map = HashMap<String, Any?>()
    @Synchronized override fun getString(key: String) = map[key] as? String
    @Synchronized override fun getLong(key: String) = (map[key] as? Long) ?: 0L
    @Synchronized override fun put(values: Map<String, Any?>, commit: Boolean) {
        for ((k, v) in values) if (v == null) map.remove(k) else map[k] = v
    }
}

internal interface Clock {
    fun now(): Long

    companion object {
        val SYSTEM: Clock = object : Clock {
            override fun now() = System.currentTimeMillis()
        }
    }
}
