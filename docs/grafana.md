# Grafana

Three ways to chart Vaka in Grafana:

| | Vaka plugin (recommended) | Infinity datasource | ClickHouse datasource |
| --- | --- | --- | --- |
| Talks to | Vaka's REST API | Vaka's REST API | Vaka's ClickHouse directly |
| Auth and project scoping | Vaka API token, kept on the Grafana server | Vaka API token | A ClickHouse user that sees every project |
| Queries | Pick a query type and project from dropdowns | Hand-written URLs and JSON columns | Any SQL |
| Alerting, live streams, logs and traces | Yes | Alerting only | Alerting only |
| Stability | Versioned API | Versioned API | Internal tables that can change between releases |

## Vaka plugin

The `hellsoft-vaka-datasource` plugin lives in
[`integrations/grafana`](https://github.com/ErikHellman/vaka/tree/main/integrations/grafana). It needs
Grafana 12 or later and Vaka 0.5 or later (older servers work, but without the errors, performance
and session time series and live streams).

### Install

Until the plugin is in the Grafana catalog, install a release zip from
[GitHub releases](https://github.com/ErikHellman/vaka/releases) (tags `grafana/v*`):

```sh
grafana cli --pluginUrl https://github.com/ErikHellman/vaka/releases/download/grafana/v0.1.0/hellsoft-vaka-datasource-0.1.0.zip plugins install hellsoft-vaka-datasource
```

If the zip is not signed for your Grafana, allow it explicitly in `grafana.ini`
(`[plugins] allow_loading_unsigned_plugins = hellsoft-vaka-datasource`) or with
`GF_PLUGINS_ALLOW_LOADING_UNSIGNED_PLUGINS=hellsoft-vaka-datasource`. To build from source, see the
plugin's [README](https://github.com/ErikHellman/vaka/blob/main/integrations/grafana/README.md).

### Configure

1. Create a read token in Vaka: admin UI → Tokens, or `POST /api/v1/tokens` with `{"name": "grafana", "scope": "read"}`.
   Scope it to one project if Grafana should only see that one.
2. In Grafana: Connections → Data sources → Add → Vaka. Enter the Vaka URL (without `/api/v1`),
   the token and optionally a default project, then **Save & test**. The test reports the server
   version, the token and how many projects it can read.

Or provision it:

```yaml
apiVersion: 1
datasources:
  - name: Vaka
    type: hellsoft-vaka-datasource
    access: proxy
    jsonData:
      url: https://vaka.example.com
      defaultProject: web
    secureJsonData:
      token: $VAKA_GRAFANA_TOKEN
```

### Query types

Every query picks a **query type** and a **project**; the dashboard time range applies.

| Query type | What you get | Good for |
| --- | --- | --- |
| Errors over time | `errors`, `unhandled`, `fatal`, `users`, `issues` per bucket; group by issue, release, level, environment, exception type, platform, OS, browser or country | Graphs and alerts on error rates |
| Issues | Table of issues with events and users in the time range, linked to Vaka | "Top issues" tables |
| Sessions and crash-free rate | `crash_free_sessions`, `crash_free_users`, `sessions`, ... per bucket; group by release | Crash-free rate per release, alerts |
| Release health | Crash-free sessions and users, errors, ANRs per release | Release tables |
| Release adoption | Share of sessions per release over time | Rollout graphs |
| Analytics over time | Visitors, pageviews, screen views, custom events per bucket; group by any dimension; filters | Traffic graphs |
| Analytics breakdown | Top values of a dimension (pages, referrers, countries, `prop:<key>`) | Bar charts, tables |
| Analytics summary | Totals with the previous period | Stat panels |
| Realtime visitors | The last 1 to 60 minutes, plus visitors per minute | Wallboards |
| Funnel | Conversion through steps such as `pageview\|path:/pricing, signup` | Bar charts |
| Performance over time | `p50_ms`..`p99_ms`, `count`, `per_minute`, `error_rate` per bucket; group by transaction, op, release, ... | Latency graphs and alerts |
| Performance summary | Latency per transaction | Tables |
| Web Vitals | p75 and rating for LCP, INP, CLS, FCP, TTFB, overall or per page | Stat panels |
| Logs | OpenTelemetry logs as a logs frame | Logs panel, Explore |
| Trace | One trace as a span tree | Trace view, Explore |
| Monitors | Monitor state, `up` (1/0) and uptime | Status panels, alerts |
| Incidents | Monitor incidents | Tables |
| Live stream | New errors, analytics events or transactions as they arrive | Live tables and logs |

Time series use the panel's interval, rounded to 1m, 2m, 5m, 10m, 15m, 30m, 1h, ... and every
bucket is returned (zeros where nothing happened), so alerts don't see gaps as missing data. Set
**Bucket** to fix the size. With **Group by**, the first metric is shown once per group (top 10 by
default, **Top** changes it).

### Alerting

Every time series query type works in Grafana alert rules. Examples:

- **Error spike:** Errors over time, metric `errors`, then a Reduce (last) and Threshold above 50.
- **Crash-free rate:** Sessions and crash-free rate, group by release, metric
  `crash_free_sessions`, Threshold below 0.99 (one alert instance per release).
- **Slow endpoint:** Performance over time, transaction `GET /api/orders`, metric `p95_ms`,
  Threshold above 800.
- **Monitor down:** Monitors, Reduce `up` (min), Threshold below 1.

Vaka's own [alert rules](alerts.md) still matter: they know about new issues and regressions and
can wake a coding agent. Grafana alerting suits teams that route everything through Grafana.

### Variables and annotations

Variables (dashboard settings → Variables → Query, data source Vaka) list projects, releases,
event names or the top values of a dimension. Use them anywhere: `$project` as the project,
`$release` in a filter.

Annotations (dashboard settings → Annotations, data source Vaka) mark when each release was first
seen, new issues and regressions, or monitor incidents (as regions).

### Live streams

The **Live stream** query type subscribes to Vaka's [live stream](api.md) through Grafana Live:
one connection per channel on the Grafana server, shared by every viewer. Pick errors, analytics
events or transactions and optionally an environment. Use it with a table or logs panel; the panel
keeps the last 500 rows (**Keep**). Live streams don't run in alert rules.

## Infinity datasource (REST API)

1. Install the [Infinity plugin](https://grafana.com/grafana/plugins/yesoreyeram-infinity-datasource/)
   (`grafana cli plugins install yesoreyeram-infinity-datasource`, or
   `GF_INSTALL_PLUGINS=yesoreyeram-infinity-datasource` in Docker).
2. Create a read-only API token in Vaka, scoped to the project if you only chart one.
3. Add the datasource. In the UI: Authentication → Bearer token, paste the token, and add your
   Vaka URL under Allowed hosts. Or provision it with
   [`deploy/grafana/datasource.yaml`](https://github.com/ErikHellman/vaka/blob/main/deploy/grafana/datasource.yaml).
4. Import [`deploy/grafana/vaka-overview.json`](https://github.com/ErikHellman/vaka/blob/main/deploy/grafana/vaka-overview.json)
   (Dashboards → New → Import) and set the `Vaka URL` and `Project slug` variables at the top.

The example dashboard refreshes every 10 seconds and shows live visitors, pageviews, active
sessions and errors, visitors per minute, traffic over the selected time range, top pages and
the top issues.

### Writing your own panels

Each panel is an Infinity query of type JSON, source URL, parser Backend:

| Panel | URL | Rows / root | Columns |
| --- | --- | --- | --- |
| Traffic over time | `${vaka_url}/api/v1/projects/${project}/analytics/timeseries?since=${__from}&until=${__to}` | `series` | `bucket` (timestamp), `visitors`, `pageviews`, `events` |
| Breakdown | `…/analytics/breakdown?dimension=country&since=${__from}&until=${__to}` | `rows` | `value`, `visitors`, `pageviews` |
| Right now | `…/analytics/realtime?minutes=5` | (none) | `visitors`, `pageviews`, `active_sessions`, `errors` |
| Issues | `…/issues?sort=events&since=${__from}` | `issues` | `short_id`, `title`, `events_in_window`, `users_in_window` |
| Endpoint latency | `…/performance/endpoints?since=${__from}` | see [performance.md](performance.md) | |

`since` and `until` accept epoch milliseconds, so Grafana's `${__from}` and `${__to}` work as
they are. Analytics endpoints take the same `filter=dimension:value` parameters as in
[api.md](api.md). Set the dashboard refresh to 5 to 10 seconds for a live view: Vaka stores new
data about once a second.

## ClickHouse datasource (SQL)

For ad-hoc SQL you can point Grafana's
[ClickHouse plugin](https://grafana.com/grafana/plugins/grafana-clickhouse-datasource/) at
Vaka's ClickHouse. Know what you give up:

- The ClickHouse user sees every project. There is no token or project scoping.
- Tables (`events`, `errors`, `sessions`, `spans`, `logs`) are internal and can change in any
  release. Pin dashboards to a Vaka version and recheck after upgrades.
- Heavy queries compete with ingestion on the same server.

Create a read-only user instead of reusing Vaka's (`docker compose exec clickhouse clickhouse-client --user vaka --password …`):

```sql
CREATE USER grafana IDENTIFIED WITH sha256_password BY 'a-long-random-password'
  SETTINGS readonly = 1, max_execution_time = 30, max_memory_usage = 2000000000;
GRANT SELECT ON vaka.* TO grafana;
```

The compose file does not publish ClickHouse's port. Run Grafana on the same Docker network
(`vaka`) and use `clickhouse:8123`, or publish the port only on a private interface.

Example: pageviews per minute for one project.

```sql
SELECT toStartOfMinute(timestamp) AS time, count() AS pageviews
FROM vaka.events
WHERE project_id = '<project id>'
  AND type = 'pageview' AND $__timeFilter(timestamp)
GROUP BY time ORDER BY time
```

Project ids come from `GET /api/v1/projects` (`id`); ClickHouse stores ids, not slugs.
