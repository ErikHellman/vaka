package se.hellsoft.vaka.internal

import java.io.ByteArrayOutputStream
import java.io.File
import java.util.UUID

/** An image sent with feedback. */
internal class Attachment(val filename: String, val contentType: String, val bytes: ByteArray)

internal object Images {
    const val MAX_ATTACHMENTS = 3
    const val MAX_BYTES = 5 * 1024 * 1024

    /** PNG, JPEG or WebP from the magic bytes, else null. */
    fun contentType(b: ByteArray): String? = when {
        b.size >= 8 && b[0] == 0x89.toByte() && b[1] == 'P'.code.toByte() && b[2] == 'N'.code.toByte() && b[3] == 'G'.code.toByte() -> "image/png"
        b.size >= 3 && b[0] == 0xFF.toByte() && b[1] == 0xD8.toByte() && b[2] == 0xFF.toByte() -> "image/jpeg"
        b.size >= 12 && String(b, 0, 4, Charsets.US_ASCII) == "RIFF" && String(b, 8, 4, Charsets.US_ASCII) == "WEBP" -> "image/webp"
        else -> null
    }

    /** Keeps the first 3 valid images of at most 5 MB each. */
    fun attachments(images: List<ByteArray>, log: Logger): List<Attachment> {
        val out = ArrayList<Attachment>()
        for (b in images) {
            if (out.size >= MAX_ATTACHMENTS) {
                log.warn("feedback: at most $MAX_ATTACHMENTS attachments, dropping the rest")
                break
            }
            val type = contentType(b)
            when {
                type == null -> log.warn("feedback: attachment is not a PNG, JPEG or WebP image, dropping it")
                b.size > MAX_BYTES -> log.warn("feedback: attachment larger than 5 MB, dropping it")
                else -> out.add(Attachment("screenshot-${out.size + 1}.${type.substringAfter('/').replace("jpeg", "jpg")}", type, b))
            }
        }
        return out
    }
}

internal object Multipart {
    fun boundary(): String = "vaka-" + UUID.randomUUID().toString().replace("-", "")

    /** `feedback` JSON part followed by one `screenshot` file part per attachment. */
    fun build(boundary: String, json: String, attachments: List<Attachment>): ByteArray {
        val out = ByteArrayOutputStream()
        fun w(s: String) = out.write(s.toByteArray(Charsets.UTF_8))
        w("--$boundary\r\nContent-Disposition: form-data; name=\"feedback\"\r\nContent-Type: application/json\r\n\r\n")
        w(json)
        w("\r\n")
        for (a in attachments) {
            w("--$boundary\r\nContent-Disposition: form-data; name=\"screenshot\"; filename=\"${a.filename}\"\r\nContent-Type: ${a.contentType}\r\n\r\n")
            out.write(a.bytes)
            w("\r\n")
        }
        w("--$boundary--\r\n")
        return out.toByteArray()
    }
}

/**
 * Feedback waiting to be sent, one directory per item (`item.json` plus its images), so it
 * survives being offline and app restarts. Directories are renamed into place when complete.
 */
internal class FeedbackStore(private val dir: File, private val maxItems: Int, private val log: Logger) {
    class Pending(val dir: File, val json: String, val attachments: List<Attachment>)

    init {
        dir.listFiles { f -> f.name.endsWith(".tmp") }?.forEach { it.deleteRecursively() }
    }

    @Synchronized
    fun add(id: String, json: String, attachments: List<Attachment>): Pending? = try {
        dir.mkdirs()
        val name = String.format(java.util.Locale.ROOT, "%013d-%s", System.currentTimeMillis(), id.filter { it.isLetterOrDigit() || it == '-' })
        val tmp = File(dir, "$name.tmp").apply { deleteRecursively(); mkdirs() }
        File(tmp, "item.json").writeText(json)
        attachments.forEachIndexed { i, a -> File(tmp, "$i-${a.filename}").writeBytes(a.bytes) }
        val final = File(dir, name)
        if (!tmp.renameTo(final)) {
            tmp.deleteRecursively()
            null
        } else {
            trim()
            Pending(final, json, attachments)
        }
    } catch (e: Exception) {
        log.warn("could not store feedback", e)
        null
    }

    @Synchronized
    fun all(): List<Pending> = (dir.listFiles { f -> f.isDirectory && !f.name.endsWith(".tmp") } ?: emptyArray())
        .sortedBy { it.name }
        .mapNotNull { d -> read(d) }

    @Synchronized
    fun remove(p: Pending) {
        p.dir.deleteRecursively()
    }

    private fun read(d: File): Pending? = try {
        val json = File(d, "item.json").readText()
        val files = (d.listFiles { f -> f.name != "item.json" } ?: emptyArray()).sortedBy { it.name }
        val atts = files.mapNotNull { f ->
            val bytes = f.readBytes()
            Images.contentType(bytes)?.let { Attachment(f.name.substringAfter('-'), it, bytes) }
        }
        Pending(d, json, atts)
    } catch (e: Exception) {
        log.warn("dropping unreadable feedback ${d.name}", e)
        d.deleteRecursively()
        null
    }

    private fun trim() {
        val items = (dir.listFiles { f -> f.isDirectory && !f.name.endsWith(".tmp") } ?: return).sortedBy { it.name }
        for (i in 0 until (items.size - maxItems).coerceAtLeast(0)) items[i].deleteRecursively()
    }
}
