# Vaka

Vaka is a self-hosted crash reporting and analytics service built for coding agents.

- **Crash reporting**: errors and crashes grouped into issues, symbolicated (JS source maps,
  Android R8, iOS dSYM, native/Rust debug info), with breadcrumbs, regressions and
  crash-free users/sessions per release. Android ANRs and native (NDK) crashes, iOS app hangs.
- **Fix context**: stack frames linked to repository source at the release's commit, suspect
  commits, owners from ownership rules, and `Fixes WEB-12` trailers that resolve issues in the
  release that ships the fix ([CLI](cli.md#record-release-commits)).
- **Performance**: traces and spans, latency percentiles, slow endpoints, app start times,
  Web Vitals and release regressions ([performance](performance.md)), plus OTLP traces and logs
  from any backend ([OpenTelemetry](opentelemetry.md)).
- **Web and mobile analytics**: cookieless, no-PII visitor counts, pageviews, screen views,
  referrers, UTM tags, devices and countries.
- **Product analytics**: custom events with properties, funnels, retention, paths, cohorts,
  stickiness, lifecycle, goals and revenue, session timelines with crash precursors, and
  per-user timelines for users the app chooses to identify.
- **Feature flags and experiments**: rollouts, targeting, flag health and experiment results
  ([flags](flags.md)).
- **Alerts and agent triggers**: webhooks, Slack, email, GitHub `repository_dispatch` or GitHub
  issues ([alerts](alerts.md)); anomaly detection and scheduled digests ([anomalies](anomalies.md)).

There is no dashboard. A small admin UI (`/ui`) handles setup: users, projects, keys and tokens.
Everything else goes through the [REST API](api.md) or the [MCP server](mcp.md), so you can ask
an agent "what broke in yesterday's release and why?" and let it fix the bug.

## Start here

- [Quickstart](quickstart.md): run Vaka and send a first event in five minutes.
- [Self-hosting](self-hosting.md): Docker Compose, configuration, storage, backups and upgrades.
  [Deploy on Fly.io](deploy-fly.md) with one command.
- Install an SDK: [JavaScript](sdk-javascript.md), [Android](sdk-android.md), [iOS](sdk-ios.md),
  [Rust](sdk-rust.md), [JVM](sdk-jvm.md), [Node.js](sdk-node.md), [.NET](sdk-dotnet.md), [OpenTelemetry](opentelemetry.md) for any backend, or [any language over HTTP](sdk-http.md).
- Connect an agent: [MCP](mcp.md). Machine-readable index: [/llms.txt]({{ENDPOINT}}/llms.txt).

## Reference

- [REST API](api.md) · [MCP tools](mcp.md) · [Ingest protocol](ingest.md)
- [CLI](cli.md) · [Alerts](alerts.md) · [Privacy and data retention](privacy.md)
- [Performance](performance.md) · [Feature flags](flags.md) · [Anomalies and digests](anomalies.md) · [OpenTelemetry](opentelemetry.md)

Vaka is open source under MIT or Apache-2.0: https://github.com/ErikHellman/vaka
