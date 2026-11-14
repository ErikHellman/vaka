# Monitoring

Monitors tell you when something that should happen doesn't. Vaka alerts through your existing
[alert rules](alerts.md) (webhook, Slack, email, SMS, GitHub dispatch) when a monitor goes down,
and again when it recovers.

Three kinds:

- **heartbeat**: your cron jobs, queues and other scheduled work check in; a missed, failed or
  hung run takes the monitor down.
- **http**: Vaka requests a URL on an interval and checks the status code, optionally the body.
- **tls**: Vaka watches a host's certificate and warns before it expires.

## Heartbeat monitors

A heartbeat monitor expects a check-in on a schedule. If the check-in doesn't arrive within the
grace period, if the job reports an error, or if a started run doesn't finish within its maximum
runtime, the check fails.

Create one with the API (or let the first check-in create it, see below):

```sh
curl -X POST {{ENDPOINT}}/api/v1/projects/web/monitors -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{
    "slug": "nightly-backup",
    "name": "Nightly backup",
    "environment": "production",
    "config": {
      "schedule": {"cron": "0 3 * * *", "timezone": "Europe/Stockholm"},
      "grace_s": 600,
      "max_runtime_s": 3600
    }
  }'
```

| Config | Meaning |
| --- | --- |
| `schedule` | `{"every": "15m"}` (at least 1 minute), or `{"cron": "0 3 * * *", "timezone": "UTC"}`: five fields, ranges, steps, lists, names (`mon-fri`, `jan`) and `@daily`, `@hourly`, `@weekly`, `@monthly` |
| `grace_s` | How late a check-in may be (default 120 s). Also accepts `"10m"` |
| `max_runtime_s` | How long a run reported with `in_progress` may take (default 1800 s; 0 = no limit) |

`fail_threshold` and `recover_threshold` (default 1) set how many failed or successful checks in
a row change the state.

### Check in

Send the project's ingest key, the same one your SDKs use:

```sh
# Crontab: report success after the job, failure if it fails.
0 3 * * * /usr/local/bin/backup.sh && curl -fsS "{{ENDPOINT}}/ingest/v1/monitors/nightly-backup/ok?key=$VAKA_KEY" \
  || curl -fsS "{{ENDPOINT}}/ingest/v1/monitors/nightly-backup/error?key=$VAKA_KEY"
```

To measure runtime and catch jobs that hang, report the start too:

```sh
curl -X POST {{ENDPOINT}}/ingest/v1/monitors/nightly-backup/check-ins -H "X-Vaka-Key: $VAKA_KEY" \
  -H 'Content-Type: application/json' -d '{"status": "in_progress", "check_in_id": "run-42"}'
# ... the job runs ...
curl -X POST {{ENDPOINT}}/ingest/v1/monitors/nightly-backup/check-ins -H "X-Vaka-Key: $VAKA_KEY" \
  -H 'Content-Type: application/json' -d '{"status": "ok", "check_in_id": "run-42", "release": "1.4.0"}'
```

Or use a helper that does this for you: `vaka monitor run nightly-backup -- ./backup.sh` in
the [CLI](cli.md#monitor-a-cron-job), `withMonitor` in [Node](sdk-node.md#cron-jobs-heartbeat-monitors),
`@vaka.monitor("slug")` in [Python](sdk-python.md#cron-jobs-heartbeat-monitors) and
`vaka.WithMonitor` in [Go](sdk-go.md#cron-jobs-heartbeat-monitors). They also tag errors from the
run, so the incident lists them.

| Field | Meaning |
| --- | --- |
| `status` | `in_progress`, `ok` (default) or `error` |
| `check_in_id` | Pairs a start with its finish |
| `duration_ms` | Runtime, if you measured it yourself |
| `release`, `environment` | Stored with the check |
| `message` | Error text for `error` check-ins, shown in alerts |
| `monitor` | Create the monitor on its first check-in: `{"schedule": {...}, "grace_s": 600, "max_runtime_s": 3600, "name": "..."}` |

The short form `GET` or `POST /ingest/v1/monitors/{slug}/ok|error|start?key=...` takes the same
fields as query parameters. Check-ins are limited to one per second per monitor and status.

## HTTP monitors

```sh
curl -X POST {{ENDPOINT}}/api/v1/projects/web/monitors -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{
    "name": "Shop API", "kind": "http",
    "config": {"url": "https://shop.example.com/health", "every": "1m", "body_contains": "ok"},
    "secret_headers": {"Authorization": "Bearer ..."}
  }'
```

| Config | Meaning |
| --- | --- |
| `url` | http or https URL (required) |
| `every` | Check interval, `30s` to `7d` (default `1m`) |
| `method` | `GET` (default) or `HEAD` |
| `timeout_s` | Seconds to wait for a response (default 10, at most 60) |
| `expect_status` | Accepted codes: `200`, `"2xx"` or a list (default 2xx and 3xx) |
| `body_contains` | Text the response (first 1 MB) must contain |
| `headers` | Extra request headers, stored and shown as-is |
| `follow_redirects` | Follow up to 5 redirects (default `true`); each target is checked like `url` |
| `tls_warn_days` | For https: fire `tls_expiring` this many days before the certificate expires (default 14, 0 = off) |

`secret_headers` (next to `config`) are stored encrypted with `VAKA_SECRET_KEY` and only their
names are shown again. Updating them replaces the whole set; `{}` removes them.

An HTTP monitor goes down after **2** failed checks in a row and up after 2 good ones
(`fail_threshold`, `recover_threshold`). A check fails on a connection error, a timeout, an
unexpected status, a missing `body_contains` or an invalid certificate.

Every check sends a W3C `traceparent` header. If the target reports traces to Vaka, the incident
context has `check_trace_id`, and `get_trace` shows where the failed request spent its time.

Run a check right away with `POST .../monitors/{slug}/run`, the MCP tool `run_monitor_check` or
**Check now** in the admin UI, for example to confirm a fix. It counts like a scheduled check.

### Which addresses checks can reach

Checks only reach public addresses. Private, loopback, link-local and cloud metadata
addresses are refused when the monitor is saved and again when each connection is made (after
DNS), and redirects to them are refused too. To monitor internal services, the operator lists
the ranges in `VAKA_MONITOR_ALLOW_CIDRS`, for example `10.0.0.0/8,fd00::/8`.

When a check can't connect, Vaka first checks its own network by requesting the canary URLs
(`VAKA_MONITOR_CANARY_URLS`, by default Google's and Cloudflare's). If none answer, the check is
stored as `inconclusive` and doesn't count, so a network problem on Vaka's side doesn't page you
for every monitor. Set it to `off` to skip this.

## TLS monitors

```sh
curl -X POST {{ENDPOINT}}/api/v1/projects/web/monitors -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{"name": "Shop certificate", "kind": "tls", "config": {"host": "shop.example.com"}}'
```

Config: `host` (required), `port` (443), `every` (default `6h`), `tls_warn_days` (14). Vaka
connects, verifies the certificate and records when it expires. A certificate that is invalid
or expired fails the check (and takes the monitor down). Once it is within `tls_warn_days` of
expiring, Vaka fires **`tls_expiring`** once for that certificate and opens a `tls_expiring`
incident, which closes when a renewed certificate shows up. HTTP monitors on https URLs do the
same, so a separate TLS monitor is only needed for hosts you don't otherwise check.

## States, incidents and flapping

A monitor is `unknown` until its first check, then `up` or `down`. Going down opens an
**incident** that closes when the monitor recovers; `monitor_up` alerts carry the downtime.

A monitor that changes state 4 or more times within 30 minutes is **flapping**: Vaka sends one
alert saying so, then stays quiet until it has been stable for 15 minutes, and sends one more
alert with the state it settled in.

Pause a monitor during maintenance with `PATCH .../monitors/{slug}` and `{"paused_until": "2h"}`
(a duration, an RFC 3339 time or `"forever"`); `{"paused_until": null}` resumes it. Check-ins
while paused are stored but change nothing.

## Alerts

```sh
curl -X POST {{ENDPOINT}}/api/v1/alert-rules -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{
    "project": "web", "trigger": "monitor_down", "channel": "sms", "target": "+46701234567",
    "filters": {"monitors": ["nightly-backup"]}
  }'
```

Add a second rule with `monitor_up` to hear about recoveries, and one with `tls_expiring` for
certificates. Webhook payloads contain `monitor`, `incident` and `details`. `monitor_kind`
(`heartbeat`, `http`, `tls`) filters by kind.

## Why is it down?

When a monitor goes down, Vaka stores a **context** snapshot on the incident and puts it in the
alert payload:

- `recent_releases`: releases created in the 2 hours before the incident.
- `issues`: the issues with the most errors within 15 minutes of the start, flagged `new` or
  `regressed` when that happened in the same window.
- `error_rate`: errors in the 30 minutes before against the 24-hour baseline.
- `error_logs`: the most common error logs in the window.
- `run_errors`: errors tagged `monitor.check_in_id` with the failed run's id (the SDK helpers
  add this tag).
- `hints`: one-line leads, such as "release 2.4.0 was created 6 min before the incident".

The MCP tool `get_incident` returns it, recomputed while the incident is open, so an agent can
go from "the backup job is down" to `get_fix_context` on the issue that broke it.

## Admin UI

**Monitors** on a project page lists monitors with state and uptime. A monitor's page shows its
last checks, incidents with their hints, the crontab line to check in (heartbeats) or the target
and certificate expiry (HTTP and TLS), **Check now**, and pause and resume.

## API

| Endpoint | Purpose |
| --- | --- |
| `GET/POST /api/v1/projects/{p}/monitors` | List (with uptime for 24 h, 7 d, 30 d) or create |
| `GET/PATCH/DELETE /api/v1/projects/{p}/monitors/{slug}` | One monitor with recent checks and incidents |
| `GET /api/v1/projects/{p}/monitors/{slug}/checks?since=7d` | Check results |
| `POST /api/v1/projects/{p}/monitors/{slug}/run` | Run an HTTP or TLS check now |
| `GET /api/v1/projects/{p}/incidents?status=open` | Incidents |
| `GET/PATCH /api/v1/incidents/{id}` | One incident; `{"acknowledged": true}` |

## Limits

- 50 monitors per project (`VAKA_MONITOR_MAX_PER_PROJECT`).
- Check results follow the project's data retention.
- HTTP and TLS checks run from the Vaka server, one region. Up to 16 run at the same time.
- Vaka can't alert about itself. If Vaka is down, nothing is checked; watch Vaka's `/health`
  from somewhere else.
- Monitors pause while the server is in read-only maintenance mode.
