package se.hellsoft.vaka.jvm.internal;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.zip.GZIPOutputStream;

/** {@link Transport} on the JDK HttpClient, gzip-compressing request bodies. */
public final class HttpTransport implements Transport {
    private final String endpoint;
    private final String key;
    private final String sdkHeader;
    private final Logger log;
    private final HttpClient http;

    public HttpTransport(String endpoint, String key, String sdkHeader, Logger log) {
        this.endpoint = endpoint.replaceAll("/+$", "");
        this.key = key;
        this.sdkHeader = sdkHeader;
        this.log = log;
        this.http = HttpClient.newBuilder()
            .connectTimeout(Duration.ofSeconds(10))
            .followRedirects(HttpClient.Redirect.NORMAL)
            .build();
    }

    @Override
    public Response post(String path, String json, Duration timeout) {
        try {
            HttpRequest req = HttpRequest.newBuilder(URI.create(endpoint + path))
                .timeout(timeout)
                .header("Content-Type", "application/json")
                .header("Content-Encoding", "gzip")
                .header("X-Vaka-Key", key)
                .header("X-Vaka-Sdk", sdkHeader)
                .POST(HttpRequest.BodyPublishers.ofByteArray(gzip(json)))
                .build();
            HttpResponse<String> res = http.send(req, HttpResponse.BodyHandlers.ofString(StandardCharsets.UTF_8));
            long retryAfter = res.headers().firstValue("Retry-After").map(HttpTransport::seconds).orElse(-1L);
            return new Response(res.statusCode(), res.body(), retryAfter);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return new Response(0, e.toString(), -1);
        } catch (IOException | RuntimeException e) {
            log.debug("request to " + path + " failed: " + e);
            return new Response(0, e.toString(), -1);
        }
    }

    @Override
    public Response post(String path, String contentType, byte[] body, java.util.Map<String, String> headers, Duration timeout) {
        try {
            HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(endpoint + path))
                .timeout(timeout)
                .header("Content-Type", contentType)
                .header("X-Vaka-Key", key)
                .header("X-Vaka-Sdk", sdkHeader);
            for (java.util.Map.Entry<String, String> h : headers.entrySet()) b.header(h.getKey(), h.getValue());
            HttpResponse<String> res = http.send(b.POST(HttpRequest.BodyPublishers.ofByteArray(body)).build(),
                HttpResponse.BodyHandlers.ofString(StandardCharsets.UTF_8));
            return new Response(res.statusCode(), res.body(), -1);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return new Response(0, e.toString(), -1);
        } catch (IOException | RuntimeException e) {
            log.debug("request to " + path + " failed: " + e);
            return new Response(0, e.toString(), -1);
        }
    }

    private static long seconds(String v) {
        try {
            return Long.parseLong(v.trim()) * 1000;
        } catch (NumberFormatException e) {
            return -1;
        }
    }

    static byte[] gzip(String s) throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        try (GZIPOutputStream gz = new GZIPOutputStream(out)) {
            gz.write(s.getBytes(StandardCharsets.UTF_8));
        }
        return out.toByteArray();
    }
}
