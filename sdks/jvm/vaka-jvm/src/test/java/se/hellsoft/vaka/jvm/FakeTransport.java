package se.hellsoft.vaka.jvm;

import java.time.Duration;
import java.util.ArrayList;
import java.util.ArrayDeque;
import java.util.Deque;
import java.util.List;
import java.util.Map;
import se.hellsoft.vaka.jvm.internal.Json;
import se.hellsoft.vaka.jvm.internal.Transport;

/** Records requests; answers 202 unless responses are queued. */
final class FakeTransport implements Transport {
    static final class Request {
        final String path;
        final String json;
        final String contentType;
        final byte[] raw;
        final Map<String, String> headers;

        Request(String path, String json) { this(path, json, "application/json", null, Map.of()); }

        Request(String path, String json, String contentType, byte[] raw, Map<String, String> headers) {
            this.path = path;
            this.json = json;
            this.contentType = contentType;
            this.raw = raw;
            this.headers = headers;
        }

        @SuppressWarnings("unchecked")
        Map<String, Object> body() { return (Map<String, Object>) Json.parse(json); }
    }

    final List<Request> requests = new ArrayList<>();
    private final Deque<Response> responses = new ArrayDeque<>();
    String flagsResponse = "{\"flags\":{}}";

    synchronized void respond(int status) { responses.add(new Response(status, "{}", -1)); }

    @Override
    public synchronized Response post(String path, String json, Duration timeout) {
        requests.add(new Request(path, json));
        if (path.equals("/ingest/v1/flags")) return new Response(200, flagsResponse, -1);
        Response r = responses.poll();
        return r != null ? r : new Response(202, "{\"accepted\":1,\"rejected\":[]}", -1);
    }

    @Override
    public synchronized Response post(String path, String contentType, byte[] body, Map<String, String> headers, Duration timeout) {
        String text = new String(body, java.nio.charset.StandardCharsets.UTF_8);
        requests.add(new Request(path, contentType.equals("application/json") ? text : null, contentType, body, Map.copyOf(headers)));
        Response r = responses.poll();
        return r != null ? r : new Response(202, "{\"id\":\"fb-1\",\"short_id\":\"API-F1\"}", -1);
    }

    synchronized void respond(int status, String body) { responses.add(new Response(status, body, -1)); }

    synchronized List<Request> feedback() {
        List<Request> out = new ArrayList<>();
        for (Request r : requests) if (r.path.equals("/ingest/v1/feedback")) out.add(r);
        return out;
    }

    /** All items sent to the batch endpoint with a 2xx answer or not, in order. */
    @SuppressWarnings("unchecked")
    synchronized List<Map<String, Object>> items() {
        List<Map<String, Object>> out = new ArrayList<>();
        for (Request r : requests) {
            if (!r.path.equals("/ingest/v1/batch")) continue;
            out.addAll((List<Map<String, Object>>) r.body().get("items"));
        }
        return out;
    }

    synchronized List<Map<String, Object>> items(String type) {
        List<Map<String, Object>> out = new ArrayList<>();
        for (Map<String, Object> i : items()) if (type.equals(i.get("type"))) out.add(i);
        return out;
    }

    synchronized int batchRequests() {
        int n = 0;
        for (Request r : requests) if (r.path.equals("/ingest/v1/batch")) n++;
        return n;
    }
}
