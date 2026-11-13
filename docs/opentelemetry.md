# OpenTelemetry

Vaka accepts OTLP/HTTP traces and logs from any OpenTelemetry SDK or the OpenTelemetry Collector,
so backend services in Java, Go, Python, .NET, Node, Ruby or Rust need no Vaka SDK.

| What you send | Where it goes |
| --- | --- |
| Spans | Performance: transactions, slow endpoints, traces and release comparisons ([performance.md](performance.md)) |
| Span `exception` events | Errors, grouped into issues, with the trace id and a parsed stack trace |
| Log records | The logs store (search with REST or the `search_logs` MCP tool), shown on their trace |
| ERROR and FATAL log records | Errors too (the log body is the message, or `exception.*` attributes when present) |
| Metrics | Accepted and dropped, so exporters with all signals on don't retry |

## Endpoint and key

Use the project's ingest key as a header (`X-Vaka-Key: vk_…` or `Authorization: Bearer vk_…`).

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT={{ENDPOINT}}/ingest/otlp
export OTEL_EXPORTER_OTLP_HEADERS="x-vaka-key=$VAKA_INGEST_KEY"
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf   # or http/json
export OTEL_SERVICE_NAME=checkout
export OTEL_RESOURCE_ATTRIBUTES="service.version=2.4.0,deployment.environment.name=production"
export OTEL_METRICS_EXPORTER=none
```

SDKs append `/v1/traces` and `/v1/logs`. The full URLs are `{{ENDPOINT}}/ingest/otlp/v1/traces` and
`{{ENDPOINT}}/ingest/otlp/v1/logs`. Both protobuf (`application/x-protobuf`) and JSON
(`application/json`) work, gzip too. gRPC is not supported; use the HTTP exporter.

## OpenTelemetry Collector

```yaml
exporters:
  otlphttp/vaka:
    endpoint: {{ENDPOINT}}/ingest/otlp
    headers:
      x-vaka-key: ${env:VAKA_INGEST_KEY}

service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [batch]
      exporters: [otlphttp/vaka]
    logs:
      receivers: [otlp]
      processors: [batch]
      exporters: [otlphttp/vaka]
```

## Mapping

| OpenTelemetry | Vaka |
| --- | --- |
| `service.version` | release (so release health, comparisons and regressions work per version) |
| `deployment.environment.name` (or `deployment.environment`) | environment |
| `service.name` | service (span and log field, `service` tag on errors) |
| `telemetry.sdk.language` | platform |
| SERVER and CONSUMER spans | transactions, even with a remote parent |
| `http.request.method` + `http.route` | transaction name, such as `GET /carts/{id}` |
| `http.response.status_code` | HTTP status (5xx counts as an error) |
| `db.system`, `messaging.system`, `rpc.system` | span op `db`, `queue.publish` / `queue.process`, `rpc.client` / `rpc.server` |
| status `ERROR` | span status `error` |
| `enduser.id`, `session.id` | user id and session id |
| `exception.type`, `.message`, `.stacktrace` | exception type, value and frames (Java/Kotlin, JavaScript, Python, .NET, Go and Ruby stack formats; vendor frames are marked not in-app) |
| `exception.escaped: true` | unhandled |

Use the same trace context in the browser or app: the Vaka JavaScript SDK sends a W3C
`traceparent` header, so backend spans join the frontend trace.

## Logs

```sh
curl "{{ENDPOINT}}/api/v1/projects/checkout/logs?since=1h&severity=warn&q=timeout" \
  -H "Authorization: Bearer $VAKA_TOKEN"
```

Filters: `q` (text in the body, case-insensitive), `severity` (minimum: trace, debug, info, warn,
error, fatal), `trace_id`, `service`, `release`, `environment`, `since`, `until`, `limit` (default
100). The response has `logs` (newest first) and `counts` per severity. `GET
/projects/{p}/traces/{trace_id}` includes the trace's logs. MCP: `search_logs`.

Logs follow the project's retention and are erased with the rest of a user's data when they carry
`enduser.id`.
