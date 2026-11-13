# Performance tracing

Vaka stores spans: timed operations grouped into traces. A span without a parent is a
transaction (a page load, a screen, an HTTP request handled by your server, a background job).
Agents use the REST API or MCP tools to find slow endpoints, slow app starts, poor Web Vitals and
releases that made things slower.

## Sending spans

Send `span` items in `/ingest/v1/batch` (or an array to `POST /ingest/v1/spans`):

```json
{"type": "span", "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736", "span_id": "00f067aa0ba902b7",
 "name": "GET /api/carts/123", "op": "http.server", "start_timestamp": "2026-11-13T10:00:00.000Z",
 "timestamp": "2026-11-13T10:00:00.250Z", "status": "ok", "release": "1.4.0",
 "http": {"method": "GET", "status_code": 200}}
```

| Field | Meaning |
| --- | --- |
| `trace_id` | Required, up to 64 characters (W3C: 32 hex) |
| `span_id`, `parent_span_id` | Spans without `parent_span_id` are transactions |
| `is_transaction` | `true` for a span whose parent is in another service (default: true for `kind: "server"`) |
| `name` | What ran. HTTP and page names are grouped: `/users/123` becomes `/users/:id` |
| `op` | `http.server`, `http.client`, `db`, `pageload`, `navigation`, `app.start.cold`, `app.start.warm`, `web.vitals`, `function`, ... |
| `start_timestamp`, `timestamp` | RFC 3339, unix seconds or unix millis. Or give `duration_ms` |
| `status` | `ok`, `error` or `cancelled` (an HTTP status of 500 or more also counts as an error) |
| `transaction` | The transaction a child span belongs to (optional; defaults to the root's name for roots) |
| `release`, `environment`, `platform`, `service`, `session_id`, `user_id` | As for other items |
| `http` | `{method, status_code}` |
| `attributes` | Flat key/values, at most 64 |
| `measurements` | Numbers, at most 32. Web Vitals use `lcp`, `inp`, `cls`, `fcp`, `ttfb` (ms, CLS unitless) |

Errors can carry `trace_id` (or `contexts.trace.trace_id`); `get_trace` lists them. OpenTelemetry
traces are accepted too (see the OpenTelemetry section once enabled).

App starts: send a root span with op `app.start.cold` (process start to first frame) or
`app.start.warm`. Web Vitals: one span per page view with op `web.vitals`, the page path as
`transaction` and the vitals as `measurements`. The JavaScript SDK does this with
`tracesSampleRate`.

## Queries

| REST (`/api/v1/projects/{p}/...`) | MCP | What it answers |
| --- | --- | --- |
| `performance/summary` | `performance_summary` | p50/p75/p95/p99, throughput and error rate per transaction (`op`, `sort`) |
| `performance/endpoints` | `slow_endpoints` | Slowest `http.server*` endpoints by p95, with total time and 5xx counts (`min_count`) |
| `performance/app-start` | `app_startup` | Cold and warm start p50/p95 by release |
| `performance/web-vitals` | `web_vitals` | p75 per vital with rating (good / needs-improvement / poor) and shares, overall and by page |
| `performance/traces` | `list_slow_traces` | Slowest individual traces (`min_ms`) |
| `traces/{trace_id}` | `get_trace` | The span tree with offsets and self time, plus errors in the trace |
| `performance/breakdown` | `span_breakdown` | Where a transaction's time goes (child spans by op and name); needs `transaction` |
| `performance/compare` | `compare_release_performance` | p95 and Web Vitals regressions from `base` to `target` (`min_count`, default 20; `threshold`, default 0.2) |

Common filters: `since` (default 24h), `until`, `environment`, `release`, `transaction`, `limit`.
`op` filters accept a trailing `*` for a prefix (`http.*`).

Web Vitals ratings follow web.dev: LCP good up to 2500 ms and poor above 4000; INP 200/500 ms;
CLS 0.1/0.25; FCP 1800/3000 ms; TTFB 800/1800 ms.

## Regression alerts

An alert rule with `"trigger": "perf_regression"` compares the newest release with spans against
the previous one every 10 minutes and notifies once per release when a transaction's p95 grew by
more than `threshold` percent (default 20, with at least 20 samples in both releases and more than
10 ms) or a Web Vital got worse and is no longer rated good. Testing the rule sends the current
comparison right away. See [alerts](alerts.md).
