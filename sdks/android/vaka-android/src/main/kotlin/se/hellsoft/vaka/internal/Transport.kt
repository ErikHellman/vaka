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

internal sealed class FeedbackSendResult {
    /** 202: stored. */
    data class Accepted(val id: String?, val shortId: String?) : FeedbackSendResult()
    /** 400, 413, 429: will never be accepted as is (429 must not be retried automatically). */
    data class Rejected(val error: String) : FeedbackSendResult()
    /** 401: stop sending until the app restarts. */
    object Unauthorized : FeedbackSendResult()
    /** 5xx, network error: keep it and try again later. */
    object Retry : FeedbackSendResult()
}

internal interface Transport {
    fun send(body: String): SendResult

    /** Sends one feedback item to `/ingest/v1/feedback`, as multipart when it has attachments. */
    fun sendFeedback(json: String, attachments: List<Attachment>): FeedbackSendResult = FeedbackSendResult.Retry
}

internal class HttpTransport(
    endpoint: String,
    private val key: String,
    private val sdkHeader: String,
    private val log: Logger,
) : Transport {
    private val url = URL(endpoint.trimEnd('/') + "/ingest/v1/batch")
    private val feedbackUrl = URL(endpoint.trimEnd('/') + "/ingest/v1/feedback")

    override fun sendFeedback(json: String, attachments: List<Attachment>): FeedbackSendResult {
        val body: ByteArray
        val contentType: String
        if (attachments.isEmpty()) {
            body = json.toByteArray(Charsets.UTF_8)
            contentType = "application/json"
        } else {
            val boundary = Multipart.boundary()
            body = Multipart.build(boundary, json, attachments)
            contentType = "multipart/form-data; boundary=$boundary"
        }
        var conn: HttpURLConnection? = null
        return try {
            conn = (feedbackUrl.openConnection() as HttpURLConnection).apply {
                requestMethod = "POST"
                doOutput = true
                connectTimeout = 15_000
                readTimeout = 60_000
                setFixedLengthStreamingMode(body.size)
                setRequestProperty("Content-Type", contentType)
                setRequestProperty("X-Vaka-Key", key)
                setRequestProperty("X-Vaka-Sdk", sdkHeader)
            }
            conn.outputStream.use { it.write(body) }
            val code = conn.responseCode
            val text = try {
                (if (code < 400) conn.inputStream else conn.errorStream)?.bufferedReader()?.use { it.readText() }.orEmpty()
            } catch (e: Exception) {
                ""
            }
            log.debug("sent feedback: HTTP $code $text")
            feedbackResult(code, text)
        } catch (e: Exception) {
            log.debug("feedback send failed: $e")
            FeedbackSendResult.Retry
        } finally {
            conn?.disconnect()
        }
    }

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

internal fun feedbackResult(code: Int, text: String): FeedbackSendResult = when {
    code in 200..299 -> FeedbackSendResult.Accepted(jsonString(text, "id"), jsonString(text, "short_id"))
    code == 401 || code == 403 -> FeedbackSendResult.Unauthorized
    code == 400 || code == 413 || code == 422 || code == 429 ->
        FeedbackSendResult.Rejected(jsonString(text, "error") ?: "HTTP $code")
    else -> FeedbackSendResult.Retry
}

/** Reads a top-level string field from a small, flat JSON response. */
internal fun jsonString(json: String, field: String): String? =
    Regex("\"" + Regex.escape(field) + "\"\\s*:\\s*\"((?:[^\"\\\\]|\\\\.)*)\"").find(json)?.groupValues?.get(1)
        ?.replace("\\\"", "\"")?.replace("\\\\", "\\")
