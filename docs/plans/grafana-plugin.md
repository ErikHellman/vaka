# Native Grafana datasource plugin

Status: proposal, for review (2026-11-16). Owner: Erik + Claude Code. Nothing is built yet.

Vaka deliberately has almost no UI of its own. Grafana is the most common place people
already build dashboards and alerts, so a first-class datasource lets Vaka data sit next to
Prometheus, Loki and Tempo without anyone writing JSON paths. The Infinity-based guide (being
written in a separate thread) is the zero-install option; this plugin is the polished one.

## Goals

- Pick a project and a query type from dropdowns and get a correct graph, table, logs or trace
  view with no JSON parsing, transformations or hand-written URLs.
- Grafana alerting works on Vaka queries (errors per minute, crash-free rate, p95 latency).
- Live panels: realtime visitors and new errors update without refresh, via the SSE stream.
- Release and incident markers as annotations; projects, releases and environments as
  dashboard variables.
- Listed in the public Grafana plugin catalog, installable with `grafana cli plugins install`.

Non-goals for v1: writing to Vaka from Grafana (resolving issues, creating monitors), a Grafana
app plugin with its own pages, and bundled dashboards beyond a few examples.

## Plugin type

A **backend datasource plugin**: TypeScript/React frontend (query editor, config page) plus a
Go backend built on `grafana-plugin-sdk-go`. Scaffold with `npx @grafana/create-plugin`
(datasource, with backend).

Why backend rather than frontend-only:

- **Alerting and recorded queries** only run server side, so they need a backend.
- **Secrets stay on the Grafana server**: the API token lives in `secureJsonData` and is added
  by the backend; browsers never see it.
- **Streaming** through Grafana Live needs a backend `StreamHandler`.
- Response shaping (zero-filling buckets, building data frames) happens once in Go and is
  shared by panels, alerts and public dashboards.

Plugin id: `hellsoft-vaka-datasource` (Grafana requires `<org>-<name>-<type>`, and the org
slug must match the grafana.com account that signs it).

## Configuration page

| Field | Storage | Notes |
| --- | --- | --- |
| Vaka URL | `jsonData.url` | e.g. `https://vaka.example.com`; `/api/v1` is appended |
| API token | `secureJsonData.token` | A `read` token, all projects or one project |
| Default project | `jsonData.defaultProject` | Optional; pre-selected in new queries |
| TLS options | standard | Skip-verify and custom CA for self-hosted instances |

**Save & test** calls `CheckHealth`, which hits `GET /api/v1/me` and reports the token's name,
scope and how many projects it can read (or the exact 401/403 reason).

## Auth

- Bearer token only (`Authorization: Bearer vt_...`), injected by the backend on every request.
- Recommend a dedicated read token per Grafana instance (docs show `vaka tokens create
  --scope read --name grafana`). Write scope is never needed and the config page warns if
  `/me` reports one.
- Forwarding the signed-in Grafana user's identity is out of scope; Vaka has no OAuth/OIDC
  today.

## Query types

The query editor has a **Query type** selector, then fields specific to that type. Every type
takes the project and uses the dashboard time range (`since`/`until` from Grafana's
`TimeRange`, so no relative-duration strings).

| Query type | Vaka endpoint | Frame | Panels |
| --- | --- | --- | --- |
| Analytics time series | `analytics/timeseries` | time series, one field per metric | graph, stat |
| Analytics breakdown | `analytics/breakdown` | table (dimension, visitors, pageviews, ...) | bar chart, table, pie |
| Analytics summary | `analytics/summary` | single row, with previous period | stat |
| Realtime | `analytics/realtime` (+ stream) | stat frame + visitors per minute | stat, graph |
| Errors over time | new `errors/timeseries` (see prerequisites) | time series, split by issue/release/level | graph, alerting |
| Issues | `issues` | table with short id, title, events, users, status, link | table |
| Release health | `releases/health` | table, or time series per release | table, bar gauge |
| Release adoption | `releases/adoption` | time series, one field per release | stacked graph |
| Performance summary | `performance/summary` | table per transaction (p50..p99, error rate) | table |
| Performance over time | new `performance/timeseries` | time series p50/p95/p99/count | graph, alerting |
| Web Vitals | `performance/web-vitals` | table or stat | stat, table |
| Logs | `logs` | logs frame (`timestamp`, `body`, `severity`, labels) | logs panel, Explore |
| Trace | `traces/{trace_id}` | trace frame (Grafana trace data model) | trace view, Explore |
| Funnel | `analytics/funnel` | table (step, count, conversion) | bar chart |
| Monitors / uptime | `monitors` | table with state and uptime | table, stat |

Editor helpers come from backend **resource calls** (`CallResource`) so the token stays server
side: `/projects`, `/projects/{p}/releases`, `/projects/{p}/analytics/events` (event names and
property keys for filters and funnel steps), and the static dimension list. Filters are a
key/operator/value row editor that maps to `filter=dimension:value` (`~` contains, `!` not).

Template variables are interpolated in every text field (`$project`, `$release`, `$env`).

### Variables

A variable query editor offers: projects, releases, environments, event names and the values
of any dimension (via `analytics/breakdown`, top 100).

### Annotations

- Releases: first-session time of each release from `releases/adoption` (`first_session`).
- Incidents: open/close times from `incidents`.
- New issues and regressions: from `issues` with `first_seen` in range.

### Explore and links

- Data links on issue rows open the Vaka issue (`/issues/{id}` in the admin UI if present,
  otherwise the API URL) and, when `links` exist, the PR.
- Logs with a `trace_id` get a derived field that opens the trace query in Explore.

## Streaming (Grafana Live)

The SSE stream `GET /api/v1/projects/{p}/stream` (separate thread) is the source.

- The backend implements `SubscribeStream`, `RunStream` and `PublishStream` (publish denied).
  Channel path: `ds/<uid>/stream/<project>/<kind>`, kind = `errors` | `events` | `realtime`.
- `RunStream` opens one SSE connection per channel, shared by every viewer on that Grafana
  instance, reconnects with `Last-Event-ID` and backoff, and pushes frames with
  `sender.SendFrame` (append mode, capped length).
- The **Realtime** and **Errors over time** query types get a "Live" toggle. With it on, the
  query returns the historical frame plus a `Channel` in the frame meta, so Grafana joins
  history and live updates in one panel.
- If the server has no stream endpoint (older Vaka), the toggle is hidden and panels fall back
  to normal refresh.

## Alerting

- `"alerting": true` and `"backend": true` in `plugin.json`; every time-series query type is
  alertable because `QueryData` returns numeric wide frames.
- Typical rules shipped as examples in the docs: errors per 5 min above N, crash-free sessions
  for the latest release below 99%, p95 of a transaction above X ms, uptime below 100%.
- This complements, not replaces, Vaka's own alert rules: Vaka rules know about issues
  (new, regression) and can wake agents; Grafana rules suit teams that route everything
  through Grafana on-call.

## Server-side prerequisites

Small Vaka API additions that make the plugin (and the Infinity guide) much better. Each ships
with REST, MCP, OpenAPI and smoke coverage like every other feature.

1. **`interval=minute`** (and `5m`, `15m`) for `analytics/timeseries`, capped by bucket count,
   so Grafana's `$__interval` can be honoured. Today only hour/day/week/month exist.
2. **Zero-filled buckets** (`fill=true`) so graphs and alerts don't see gaps as missing data.
3. **`GET /projects/{p}/errors/timeseries`**: error and crash counts per bucket, optional
   `group_by` (`issue`, `release`, `level`, `environment`) and the usual filters. There is no
   error time series today.
4. **`GET /projects/{p}/performance/timeseries`**: count, p50, p95, p99 per bucket for a
   `transaction`/`op`.
5. **Releases time series** for crash-free rate per day (`releases/health?interval=day`).
6. The SSE stream endpoint (already in progress in another thread).

The plugin can ship M1 without 3 to 5 (those query types are simply hidden when the server
returns 404 on a capability probe), but alerting is far less useful without 3.

## Repository layout

A new top-level directory in this repo: `integrations/grafana/` (room for other integrations
later), with its own `package.json`, `go.mod` (module
`github.com/ErikHellman/vaka/integrations/grafana`) and `Magefile.go`, as generated by
`create-plugin`. Keeping it in the monorepo means the plugin, the API change and the smoke test
land in one PR. The Grafana catalog accepts a subdirectory of a public repo as the source link.

```
integrations/grafana/
  src/           # React: ConfigEditor, QueryEditor, VariableQueryEditor, datasource.ts
  pkg/           # Go: plugin.Datasource, query handlers per type, streams, resources
  provisioning/  # dev datasource + example dashboards
  tests/         # @grafana/plugin-e2e Playwright tests
  docker-compose.yaml  # Grafana + Vaka + ClickHouse for local dev and CI
```

## Testing

- Go unit tests per query type against recorded Vaka JSON (golden frames).
- `@grafana/plugin-e2e` Playwright tests: configure the datasource, run each query type, check
  the panel renders; run against Grafana's minimum supported version and latest.
- CI job (new `grafana.yml`, path-filtered to `integrations/grafana/**`): lint, typecheck, Go
  tests, build, `plugincheck2` (the Grafana plugin validator), e2e against the docker-compose
  stack seeded by the existing smoke script.

## Packaging, signing and publishing

1. **Versioning**: own semver, tags `grafana/vX.Y.Z` (same pattern as `sdks/go/vX.Y.Z`, so the
   server's `v*` release workflow is not triggered). Grafana dependency: `>=11.0.0`.
2. **Build**: `npm run build` + `mage -v` for linux/amd64, linux/arm64, darwin/amd64,
   darwin/arm64, windows/amd64; zip as `hellsoft-vaka-datasource-X.Y.Z.zip` with an MD5/SHA1.
3. **Signing**: create the `hellsoft` org on grafana.com, an access policy token with
   `plugins:write`, stored as `GRAFANA_ACCESS_POLICY_TOKEN` in repo secrets. CI runs
   `npx @grafana/sign-plugin@latest`. Before catalog approval, builds can be signed as
   *private* for specific root URLs (Erik's own Grafana) so they load without
   `allow_loading_unsigned_plugins`.
4. **Release workflow** `release-grafana.yml` (workflow_dispatch, input version): checks the
   version in `package.json`, builds, signs, validates, creates a GitHub release with the zip.
5. **Catalog submission** (manual, once): submit the release zip URL and source link on
   grafana.com > My Plugins. Grafana reviews it (expect days to a few weeks, sometimes a round
   of feedback); once approved it gets a **community** signature and later releases are
   submitted the same way. Needs: README with screenshots, `CHANGELOG.md`, logo (from
   `brand/`), license (MIT/Apache-2.0), and a test-able demo setup for the reviewers (the
   docker-compose file covers this).
6. **Docs**: `docs/grafana.md` (install, token, query types, alert and live examples), linked
   from `docs/index.md` and `llms.txt`, next to the Infinity guide.

## Milestones and effort

Effort assumes Claude Code doing the work with Erik reviewing; estimates are working days.

| Milestone | Scope | Effort |
| --- | --- | --- |
| **M0: server prerequisites** | Minute intervals, zero fill, errors and performance time series, capability probe | 1 to 2 d |
| **M1: core datasource** | Scaffold, config page, health check, auth, resource calls, query types: analytics time series/breakdown/summary, errors over time, issues, release health; variables; Go tests; CI | 3 to 4 d |
| **M2: alerting, annotations, more types** | Alerting enabled and verified, release/incident/issue annotations, performance types, Web Vitals, adoption, funnel, monitors, data links, example dashboards | 2 to 3 d |
| **M3: live and observability** | Grafana Live streaming from SSE, Logs and Trace frames with Explore links, e2e tests on min/latest Grafana | 2 to 3 d |
| **M4: release** | Docs, screenshots, private signing for Erik's Grafana, release workflow, catalog submission, review fixes | 1 to 2 d + Grafana review wait |

Total: roughly 9 to 14 working days of build, plus the catalog review. M1 alone is already
useful (installable as an unsigned or privately signed plugin on Erik's Grafana).

## Open questions for Erik

1. **Org slug**: `hellsoft` for the grafana.com org and plugin id (`hellsoft-vaka-datasource`)?
2. **Location**: `integrations/grafana/` in this repo, or a separate `vaka-grafana` repo? The
   monorepo is recommended.
3. **Minimum Grafana version**: 11.0 (recommended, keeps the frontend on current
   `@grafana/ui`), or support 10.4 too?
4. **Timing**: after v1 (end of November), or start M0 now since it also improves the
   Infinity guide?
