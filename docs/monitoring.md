# Monitoring

Monitors tell you when something that should happen doesn't. Vaka alerts through your existing
[alert rules](alerts.md) (webhook, Slack, email, SMS, GitHub dispatch) when a monitor goes down,
and again when it recovers.

Today Vaka has **heartbeat monitors** for cron jobs, queues and other scheduled work. HTTP
uptime checks and TLS certificate expiry are coming next.

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

Add a second rule with `monitor_up` to hear about recoveries. Webhook payloads contain `monitor`,
`incident` and `details`.

## API

| Endpoint | Purpose |
| --- | --- |
| `GET/POST /api/v1/projects/{p}/monitors` | List (with uptime for 24 h, 7 d, 30 d) or create |
| `GET/PATCH/DELETE /api/v1/projects/{p}/monitors/{slug}` | One monitor with recent checks and incidents |
| `GET /api/v1/projects/{p}/monitors/{slug}/checks?since=7d` | Check results |
| `GET /api/v1/projects/{p}/incidents?status=open` | Incidents |
| `GET/PATCH /api/v1/incidents/{id}` | One incident; `{"acknowledged": true}` |

## Limits

- 50 monitors per project (`VAKA_MONITOR_MAX_PER_PROJECT`).
- Check results follow the project's data retention.
- Vaka can't alert about itself. If Vaka is down, nothing is checked; watch Vaka's `/health`
  from somewhere else.
- Monitors pause while the server is in read-only maintenance mode.
