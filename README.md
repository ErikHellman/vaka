# Vaka

Crash reporting, performance and product analytics for the agentic era. Vaka collects crashes,
errors, traces, release health and privacy-friendly web and mobile analytics, and exposes all of
it through a REST API and an MCP server, so coding agents can query data, triage issues and open
fix PRs. There is no dashboard: the web UI only covers setup and administration.

- **Crash and error reporting**: grouping into issues, breadcrumbs, regressions and server-side
  symbolication (JS source maps, Android R8/ProGuard, iOS dSYM, Rust/ELF debug info). Android ANRs
  and native (NDK) crashes, iOS app hangs.
- **Fix context for agents**: stack frames linked to repository source at the release's commit,
  suspect commits, CODEOWNERS-like owners, and `Fixes WEB-12` trailers that resolve an issue in
  the release that ships the fix. `get_fix_context` returns all of it in one call.
- **Release health**: crash-free users and sessions, ANR and hang rates, adoption per release.
- **Performance tracing**: spans and traces, latency percentiles, slow endpoints, app start times,
  Web Vitals (LCP, INP, CLS, FCP, TTFB) and p95 regressions between releases.
- **OpenTelemetry ingestion**: OTLP/HTTP traces and logs (protobuf or JSON) from any backend.
- **Web and mobile analytics**: cookieless, no personal data by default (Plausible-style).
- **Product analytics**: funnels, retention, paths, cohorts, stickiness, lifecycle, goals, revenue,
  session timelines, crash precursors and per-user timelines for users the app identifies.
- **Feature flags and experiments**: rollouts and targeting, flag health per variant, experiment
  results with uplift, confidence interval and p-value.
- **Alerts, webhooks and agent triggers**: webhook, Slack, email, GitHub `repository_dispatch` or a
  GitHub issue on new issues, regressions, spikes, thresholds and more, with a delivery log.
- **Anomaly detection and digests**: daily or weekly digests in any time zone, with an optional
  narrative written by any OpenAI-compatible LLM.
- **Agent-first**: MCP server at `/mcp` (about 75 tools), REST API at `/api/v1`, `llms.txt` and
  install guides written for coding agents.
- **Self-hosted**: one Rust binary plus ClickHouse; metadata in SQLite or Postgres; debug files on
  disk or in S3-compatible storage. Docker Compose (with optional
  Caddy HTTPS and Postgres) and a Fly.io deploy script included, all driven by env files.

## Quick start

```sh
cp .env.example .env    # set VAKA_ADMIN_EMAIL, VAKA_ADMIN_PASSWORD, VAKA_PUBLIC_URL
docker compose up -d
open http://localhost:8080/ui
```

Then create a project and an API token in the UI, and point your agent at the MCP server:

```sh
claude mcp add --transport http vaka http://localhost:8080/mcp --header "Authorization: Bearer vt_..."
```

Or, for any client that takes a JSON config:

```json
{ "mcpServers": { "vaka": { "type": "http", "url": "http://localhost:8080/mcp",
  "headers": { "Authorization": "Bearer vt_..." } } } }
```

Documentation lives in [`docs/`](docs/index.md), is published at https://vaka.hellsoft.se, and
is served by every instance at `/docs/` and `/llms.txt`. Deploying for real (HTTPS, Postgres, S3,
email login): see [self-hosting](docs/self-hosting.md), or [deploy on Fly.io](docs/deploy-fly.md)
with `deploy/fly/fly.sh launch`.

## SDKs

| Platform | Location | Docs |
| --- | --- | --- |
| JavaScript (browser) | [`sdks/js`](sdks/js) | [SDK](docs/sdk-javascript.md) · [install](docs/install/javascript.md) |
| Source map upload (Vite, Rollup, webpack) | [`sdks/js-bundler`](sdks/js-bundler) | [README](sdks/js-bundler/README.md) |
| Android (Kotlin) | [`sdks/android`](sdks/android) | [SDK](docs/sdk-android.md) · [install](docs/install/android.md) |
| iOS, macOS, tvOS (Swift) | [`sdks/ios`](sdks/ios) | [SDK](docs/sdk-ios.md) · [install](docs/install/ios.md) |
| Rust | [`crates/vaka-sdk-rust`](crates/vaka-sdk-rust) | [SDK](docs/sdk-rust.md) · [install](docs/install/rust.md) |
| Go | [`sdks/go`](sdks/go) | [SDK](docs/sdk-go.md) · [install](docs/install/go.md) · [release](docs/releasing/go.md) |
| Any OpenTelemetry backend | OTLP/HTTP | [OpenTelemetry](docs/opentelemetry.md) |
| Anything else | [HTTP ingest API](docs/ingest.md) | [guide](docs/sdk-http.md) |

The [`vaka` CLI](docs/cli.md) uploads debug files, records release commits, manages code mappings
and ownership rules, and queries issues and release health.

## Development

```sh
docker run -d -p 8123:8123 clickhouse/clickhouse-server:24.8
VAKA_ADMIN_EMAIL=me@example.com VAKA_ADMIN_PASSWORD=password123 cargo run -p vaka-server
scripts/smoke.sh   # end-to-end test against the running server
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
