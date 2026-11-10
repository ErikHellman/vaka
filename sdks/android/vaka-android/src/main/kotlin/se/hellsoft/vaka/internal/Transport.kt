package se.hellsoft.vaka.internal

import java.io.ByteArrayOutputStream
import java.net.HttpURLConnection
import java.net.URL
import java.util.zip.GZIPOutputStream

internal sealed class SendResult {
    /** 202, or 400 (the batch can never succeed): remove the items. */
    object Done : SendResult()
    /** 413: split the batch and retry. */
    object TooLarge : SendResult()
    /** 401: stop sending until the app restarts. */
    object Unauthorized : SendResult()
    /** 429, 5xx, network error: keep the items and back off. */
    data class Retry(val retryAfterMillis: Long?) : SendResult()
}

internal interface Transport {
    fun send(body: String): SendResult
}

internal class HttpTransport(
    endpoint: String,
    private val key: String,
    private val sdkHeader: String,
    private val log: Logger,
) : Transport {
    private val url = URL(endpoint.trimEnd('/') + "/ingest/v1/batch")

    override fun send(body: String): SendResult {
        val gz = ByteArrayOutputStream().also { out -> GZIPOutputStream(out).use { it.write(body.toByteArray(Charsets.UTF_8)) } }.toByteArray()
        var conn: HttpURLConnection? = null
        return try {
            conn = (url.openConnection() as HttpURLConnection).apply {
                requestMethod = "POST"
                doOutput = true
                connectTimeout = 15_000
                readTimeout = 30_000
                setFixedLengthStreamingMode(gz.size)
                setRequestProperty("Content-Type", "application/json")
                setRequestProperty("Content-Encoding", "gzip")
                setRequestProperty("X-Vaka-Key", key)
                setRequestProperty("X-Vaka-Sdk", sdkHeader)
            }
            conn.outputStream.use { it.write(gz) }
            val code = conn.responseCode
            val text = try {
                (if (code < 400) conn.inputStream else conn.errorStream)?.bufferedReader()?.use { it.readText() }.orEmpty()
            } catch (e: Exception) {
                ""
            }
            when {
                code in 200..299 -> {
                    if (text.contains("\"rejected\":[{")) log.warn("server rejected some items: ${text.take(500)}")
                    log.debug("sent batch: HTTP $code $text")
                    SendResult.Done
                }
                code == 400 -> { log.warn("server rejected batch: ${text.take(500)}"); SendResult.Done }
                code == 401 || code == 403 -> { log.warn("ingest key rejected (HTTP $code), sending paused until restart: $text"); SendResult.Unauthorized }
                code == 413 -> SendResult.TooLarge
                else -> SendResult.Retry(conn.getHeaderField("Retry-After")?.trim()?.toLongOrNull()?.times(1000))
            }
        } catch (e: Exception) {
            log.debug("send failed: $e")
            SendResult.Retry(null)
        } finally {
            conn?.disconnect()
        }
    }
}
