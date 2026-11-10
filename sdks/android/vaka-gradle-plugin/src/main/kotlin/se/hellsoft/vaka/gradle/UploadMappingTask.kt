package se.hellsoft.vaka.gradle

import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.InputFile
import org.gradle.api.tasks.Optional
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.work.DisableCachingByDefault
import java.io.File

/** Uploads one mapping file with `POST /api/v1/projects/{p}/artifacts` (kind=proguard). */
@DisableCachingByDefault(because = "Uploads to a remote server")
abstract class UploadMappingTask : DefaultTask() {
    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val mappingFile: RegularFileProperty

    @get:Input @get:Optional
    abstract val serverUrl: Property<String>

    @get:Input @get:Optional
    abstract val projectId: Property<String>

    @get:Input @get:Optional
    abstract val token: Property<String>

    @get:Input @get:Optional
    abstract val release: Property<String>

    @get:Input
    abstract val failOnError: Property<Boolean>

    init {
        // The server is the source of truth; always run so a re-run re-uploads.
        outputs.upToDateWhen { false }
    }

    @TaskAction
    fun upload() {
        val file = mappingFile.get().asFile
        val missing = listOfNotNull(
            "url (VAKA_URL)".takeIf { serverUrl.orNull.isNullOrBlank() },
            "project (VAKA_PROJECT)".takeIf { projectId.orNull.isNullOrBlank() },
            "token (VAKA_TOKEN)".takeIf { token.orNull.isNullOrBlank() },
            "release (versionName)".takeIf { release.orNull.isNullOrBlank() },
        )
        if (missing.isNotEmpty()) {
            fail("Vaka: skipping mapping upload, missing ${missing.joinToString()}. Crashes will stay obfuscated.")
            return
        }
        if (!file.isFile || file.length() == 0L) {
            fail("Vaka: mapping file ${file.path} is missing or empty.")
            return
        }
        try {
            val result = MappingUploader.upload(serverUrl.get(), projectId.get(), token.get(), release.get(), file)
            logger.lifecycle("Vaka: uploaded ${file.name} (${file.length() / 1024} KiB) for release ${release.get()}: $result")
        } catch (e: Exception) {
            fail("Vaka: mapping upload failed: ${e.message}")
        }
    }

    private fun fail(message: String) {
        if (failOnError.get()) throw GradleException(message)
        logger.warn(message)
    }
}

/** Plain HttpURLConnection multipart upload, kept separate so it can be unit tested. */
object MappingUploader {
    fun upload(serverUrl: String, project: String, token: String, release: String, file: File): String {
        val boundary = "vaka-" + System.nanoTime().toString(16)
        val url = java.net.URL(serverUrl.trimEnd('/') + "/api/v1/projects/" + java.net.URLEncoder.encode(project, "UTF-8") + "/artifacts")
        val conn = url.openConnection() as java.net.HttpURLConnection
        conn.requestMethod = "POST"
        conn.doOutput = true
        conn.connectTimeout = 30_000
        conn.readTimeout = 300_000
        conn.setChunkedStreamingMode(64 * 1024)
        conn.setRequestProperty("Authorization", "Bearer $token")
        conn.setRequestProperty("Content-Type", "multipart/form-data; boundary=$boundary")
        conn.outputStream.buffered().use { out ->
            fun field(name: String, value: String) {
                out.write("--$boundary\r\nContent-Disposition: form-data; name=\"$name\"\r\n\r\n$value\r\n".toByteArray())
            }
            // The server needs kind and release before the file part.
            field("kind", "proguard")
            field("release", release)
            field("name", file.name)
            out.write("--$boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"${file.name}\"\r\nContent-Type: text/plain\r\n\r\n".toByteArray())
            file.inputStream().use { it.copyTo(out) }
            out.write("\r\n--$boundary--\r\n".toByteArray())
        }
        val code = conn.responseCode
        val body = (if (code < 400) conn.inputStream else conn.errorStream)?.bufferedReader()?.use { it.readText() }.orEmpty()
        conn.disconnect()
        if (code !in 200..299) throw java.io.IOException("HTTP $code from $url: ${body.take(500)}")
        return body
    }
}
