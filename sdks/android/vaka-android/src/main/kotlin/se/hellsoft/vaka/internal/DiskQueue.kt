package se.hellsoft.vaka.internal

import java.io.File

/**
 * One file per item, named so that lexical order is insertion order. Each write goes to a temp
 * file first and is renamed, so a crash mid-write never leaves a half item behind.
 */
internal class DiskQueue(private val dir: File, private val maxItems: Int, private val log: Logger) {
    private var seq = 0

    init {
        dir.mkdirs()
        dir.listFiles { f -> f.name.endsWith(".tmp") }?.forEach { it.delete() }
    }

    @Synchronized
    fun add(json: String) {
        try {
            val name = String.format(java.util.Locale.ROOT, "%013d-%06d", System.currentTimeMillis(), seq++ % 1_000_000)
            val tmp = File(dir, "$name.tmp")
            tmp.writeText(json)
            if (!tmp.renameTo(File(dir, "$name.json"))) tmp.delete()
            trim()
        } catch (e: Exception) {
            log.warn("could not queue item", e)
        }
    }

    @Synchronized
    fun size(): Int = files().size

    /** The oldest [n] items, as (file, json) pairs. Unreadable files are deleted. */
    @Synchronized
    fun peek(n: Int): List<Pair<File, String>> = files().take(n).mapNotNull { f ->
        try {
            f to f.readText()
        } catch (e: Exception) {
            f.delete()
            null
        }
    }

    @Synchronized
    fun remove(items: List<File>) {
        items.forEach { it.delete() }
    }

    private fun files(): List<File> =
        (dir.listFiles { f -> f.name.endsWith(".json") } ?: emptyArray()).sortedBy { it.name }

    private fun trim() {
        val all = files()
        if (all.size > maxItems) {
            all.take(all.size - maxItems).forEach { it.delete() }
            log.debug("queue full, dropped ${all.size - maxItems} oldest items")
        }
    }
}
