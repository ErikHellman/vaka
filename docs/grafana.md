# Grafana

Vaka has no Grafana plugin yet. Two setups work today.

| | Infinity datasource (recommended) | ClickHouse datasource |
| --- | --- | --- |
| Talks to | Vaka's REST API | Vaka's ClickHouse directly |
| Auth and project scoping | Vaka API token | A ClickHouse user that sees every project |
| Queries | The API's metrics and breakdowns | Any SQL |
| Stability | Versioned API | Internal tables that can change between releases |

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
