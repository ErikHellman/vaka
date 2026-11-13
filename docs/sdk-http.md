# Any language over HTTP

Vaka has SDKs for JavaScript, Android, iOS and Rust. For anything else (Go, Node.js, Python,
.NET, JVM servers...), the [ingest protocol](ingest.md) is small enough for a coding agent to
write a client in one sitting. Backends that already use OpenTelemetry can instead send OTLP
traces and logs directly, and span exceptions and ERROR logs become issues (see
[OpenTelemetry](opentelemetry.md)); use the ingest protocol for sessions (crash-free rates),
breadcrumbs and analytics events.

Agent-followable guide: [`/docs/install/http`]({{ENDPOINT}}/docs/install/http).

A minimal client needs to:

1. Hook the runtime's uncaught error / panic handler and convert the exception and its causes
   into `exceptions`, with frames ordered **crash site first** and `in_app` set for your code.
2. Keep a ring buffer of the last 100 breadcrumbs (log lines, HTTP requests, navigation).
3. Queue items and `POST /ingest/v1/batch` every few seconds with the `X-Vaka-Key` header,
   retrying 429/5xx with backoff, and flush synchronously on fatal errors and shutdown.
4. Optionally send a `session` at start and end of each process or user session for
   crash-free rates, and `event` items for analytics.

If you write a reusable client, consider contributing it under `sdks/`.
