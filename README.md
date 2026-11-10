# Vaka

Crash reporting and analytics for the agentic era. Vaka collects crashes, errors,
release health and privacy-friendly web and mobile analytics, and exposes all of it
through a REST integration API and an MCP server so coding agents can query data,
triage issues and fix bugs. The web UI only covers setup and administration.

- **Crash and error reporting** with grouping into issues, breadcrumbs, regressions and
  server-side symbolication (JS source maps, Android R8/ProGuard, iOS dSYM, Rust/ELF debug info).
- **Release health**: crash-free users and sessions per release.
- **Web and mobile analytics**: cookieless, no personal data by default (Plausible-style).
- **Agent-first**: MCP server at `/mcp`, REST API at `/api/v1`, `llms.txt` and install guides
  written for coding agents.
- **Self-hosted**: one Rust binary plus ClickHouse; metadata in SQLite or Postgres.

## Quick start

```sh
cp .env.example .env    # set VAKA_ADMIN_EMAIL, VAKA_ADMIN_PASSWORD, VAKA_PUBLIC_URL
docker compose up -d
open http://localhost:8080
```

Then create a project and an API token in the UI, and point your agent at the MCP server:

```json
{ "mcpServers": { "vaka": { "type": "http", "url": "http://localhost:8080/mcp",
  "headers": { "Authorization": "Bearer <api token>" } } } }
```

Documentation lives in [`docs/`](docs/index.md) and is served by every instance at `/docs/`
and `/llms.txt`.

## SDKs

| Platform | Location |
| --- | --- |
| JavaScript (browser) | [`sdks/js`](sdks/js) |
| Android (Kotlin) | [`sdks/android`](sdks/android) |
| iOS (Swift) | [`sdks/ios`](sdks/ios) |
| Rust | [`crates/vaka-sdk-rust`](crates/vaka-sdk-rust) |
| Anything else | [HTTP ingest API](docs/sdk-http.md) |

## Development

```sh
docker run -d -p 8123:8123 clickhouse/clickhouse-server:24.8
VAKA_ADMIN_EMAIL=me@example.com VAKA_ADMIN_PASSWORD=password123 cargo run -p vaka-server
scripts/smoke.sh   # end-to-end test against the running server
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
