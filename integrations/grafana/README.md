# Vaka data source for Grafana

Chart and alert on [Vaka](https://vaka.hellsoft.se) data in Grafana: errors and issues, crash-free
sessions per release, web and mobile analytics, performance (latency percentiles, Web Vitals),
OpenTelemetry logs and traces, uptime monitors, and live streams of new errors and events.

## Requirements

- Grafana 12 or later.
- A Vaka server, 0.5 or later for every query type (older servers lack the errors, performance and
  session time series and the live stream).
- A Vaka API token with **read** scope, for all projects or one.

## Getting started

1. Install the plugin (Grafana catalog, or a release zip from
   [GitHub releases](https://github.com/ErikHellman/vaka/releases), tags `grafana/v*`).
2. Connections → Data sources → Add data source → **Vaka**.
3. Enter your Vaka URL (for example `https://vaka.example.com`) and the API token, optionally a
   default project, and click **Save & test**.
4. In a panel, choose a **query type** (for example *Errors over time*) and a **project**.

Full documentation, alert examples and provisioning: <https://vaka.hellsoft.se/grafana.html>.

## Features

- **Query types:** errors over time, issues, sessions and crash-free rate, release health,
  release adoption, analytics over time, analytics breakdown and summary, realtime visitors,
  funnels, performance over time and per transaction, Web Vitals, logs, traces, monitors and
  incidents.
- **Alerting** on every time series query type, with zero-filled buckets and one series per group
  (per issue, per release, per transaction, ...).
- **Live streams** through Grafana Live: new errors, analytics events or transactions as they
  arrive, one shared connection per channel.
- **Template variables:** projects, releases, event names and the top values of any dimension.
- **Annotations:** releases, new issues and regressions, monitor incidents.
- **Logs and traces** in Explore, the logs panel and the trace view.
- The API token is stored encrypted and used only by the plugin backend; the browser never sees it.

## Development

The plugin was scaffolded with `@grafana/create-plugin`; don't edit `.config/`.

```sh
npm install
npm run dev            # frontend, watch mode
mage -v build:linux    # backend (needs Go from go.mod and mage)
npm run mock-vaka      # a mock Vaka on :18080 (token vt_e2e) for local Grafana
docker compose up      # Grafana on :3000 with the plugin and a provisioned data source
```

Tests:

```sh
npm run typecheck && npm run lint && npm run test:ci
go test ./pkg/...
npm run e2e            # Playwright, against the docker compose Grafana and npm run mock-vaka

# Every query type against a real Vaka seeded by scripts/smoke.sh:
VAKA_TEST_URL=http://127.0.0.1:8080 VAKA_TEST_TOKEN=vt_... VAKA_TEST_PROJECT=smoke-123 go test ./pkg/plugin -run TestLiveServer -v
```

CI is `.github/workflows/grafana.yml` at the repository root; releases are built, signed and
published by `.github/workflows/release-grafana.yml` (see `docs/releasing/grafana.md`).

## License

MIT or Apache-2.0, at your option.
