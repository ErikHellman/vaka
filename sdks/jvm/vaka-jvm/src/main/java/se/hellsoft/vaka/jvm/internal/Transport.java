package se.hellsoft.vaka.jvm.internal;

import java.time.Duration;

/** Sends JSON to the Vaka ingest API. */
public interface Transport {
    /** {@code status} is 0 when the request failed before a response arrived. */
    final class Response {
        public final int status;
        public final String body;
        /** From {@code Retry-After}, or -1. */
        public final long retryAfterMillis;

        public Response(int status, String body, long retryAfterMillis) {
            this.status = status;
            this.body = body == null ? "" : body;
            this.retryAfterMillis = retryAfterMillis;
        }
    }

    /** POST {@code json} to {@code path} (such as {@code /ingest/v1/batch}). Never throws. */
    Response post(String path, String json, Duration timeout);

    /** POST an uncompressed body with extra headers (used for feedback uploads). Never throws. */
    Response post(String path, String contentType, byte[] body, java.util.Map<String, String> headers, Duration timeout);
}
