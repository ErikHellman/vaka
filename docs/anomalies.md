# Anomalies and digests

Vaka looks for what is unusual so you don't have to scan dashboards. Ask an agent "is anything
wrong?" (MCP `detect_anomalies`), call the REST endpoint, or get a scheduled digest in Slack, email
or a webhook that opens with a short written summary.

## How detection works

The last window (default 24 hours) is compared with the same window one, two, three and four weeks
earlier. Comparing like with like (Tuesday morning with Tuesday mornings) cancels out weekly
seasonality. Only weeks the project was already sending data in count; with less than a week of
history the window right before is used instead.

For each metric Vaka takes the median of those baseline windows and a robust z-score:

```
z = (current - median) / max(1.4826 × MAD, noise floor)
```

The noise floor stops tiny numbers from looking dramatic: the square root of the count for counts
(Poisson noise) or 10% of the baseline, the binomial standard error for rates, and 10% (at least
5 ms) for latency. An anomaly is reported when |z| ≥ 3 and the change matters: at least 25% for
counts and latency, plus a minimum absolute change per metric.

| Metric | Reported when | Next tool |
| --- | --- | --- |
| `errors` | All errors go up (at least 10 more) | `explain_spike` |
| `issue_errors` | One issue jumps or is new (at least 5 events), up to 10 issues | `get_issue` |
| `crash_free_sessions` | Crash-free sessions drop (at least 0.5 points, 20+ sessions) | `release_health` |
| `release_crash_free` | A release (that is not nearly all traffic) is clearly worse than the usual rate | `release_health` |
| `pageviews` / `visitors` | Traffic goes up or down (at least 20) | `analytics_breakdown` by referrer |
| `event_count` | One of the 30 most common custom events goes up or down, up to 5 | `analytics_timeseries` |
| `transaction_p95` | p95 of one of the 20 busiest transactions rises (at least 50 ms, 10+ spans) | `list_slow_traces` |
| `web_vital_p75` | LCP, INP or CLS p75 gets worse | `web_vitals` |

Each anomaly carries a sentence, numbers and evidence:

```json
{
  "metric": "issue_errors",
  "subject": {"issue": "WEB-41", "title": "TypeError: cart is undefined", "url": "…"},
  "direction": "up", "severity": "high", "z": 27.3,
  "current": 40, "baseline": 0, "change_pct": null,
  "sentence": "WEB-41 `TypeError: cart is undefined` is new: 40 events in the last 24 hours.",
  "evidence": {"current": 40, "baseline": [{"bucket": "1w ago", "since": "…", "value": 0}, …]},
  "next_tool": {"tool": "get_issue", "args": {"issue": "WEB-41"}}
}
```

When a release shipped during the window, error and crash sentences say so ("…, since release
2.4.0 shipped 5h ago") and the response includes `recent_release`.

## REST

```sh
curl {{ENDPOINT}}/api/v1/projects/web/anomalies?window=24h -H "Authorization: Bearer $VAKA_TOKEN"
```

`window` is 1h to 7d. `GET /api/v1/projects/{p}/digest/preview?frequency=daily|weekly&narrative=true`
returns the digest a digest rule would send now, including `text`, without sending it.

## MCP

- `detect_anomalies` `{project, window?}`
- `preview_digest` `{project, frequency?, narrative?}`
- `create_alert_rule` / `update_alert_rule` take `schedule` for digests.

## Scheduled digests

A digest rule (`"trigger": "digest"`) sends, per project, errors and new issues with the change
from the previous period, top issues, crash-free sessions, traffic, and the detected anomalies,
opened by a short narrative. Quiet projects are left out.

Set `schedule` to choose when, in your own time zone:

```sh
curl -X POST {{ENDPOINT}}/api/v1/alert-rules -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"project": "web", "trigger": "digest", "channel": "slack",
       "target": "https://hooks.slack.com/services/…",
       "schedule": {"frequency": "weekly", "weekday": "monday", "hour": 9, "timezone": "Europe/Stockholm"}}'
```

| Field | Default | |
| --- | --- | --- |
| `frequency` | `daily` | `daily` covers the last 24 hours, `weekly` the last 7 days |
| `hour` | `8` | Local hour, 0 to 23 |
| `weekday` | `monday` | Weekly only |
| `timezone` | `UTC` | IANA name, such as `America/New_York`; daylight saving time is handled |

Without `schedule`, a digest goes out daily at `threshold` o'clock UTC (the old behavior). A new
rule waits for its first scheduled time; `POST /alert-rules/{id}/test` sends one right away.

### The narrative

With `VAKA_ANTHROPIC_API_KEY` set on the server, Claude writes the opening paragraph from the
digest data: it leads with the most severe change, connects related findings (a new release and a
crash-rate drop) and sticks to the numbers in the digest. Only the digest summary is sent (totals,
issue titles and anomaly sentences, no stack traces or user data). `VAKA_ANTHROPIC_MODEL` picks the
model (default `claude-opus-5-5`). Without a key, or if the call fails, the narrative is built from
the anomaly sentences instead. The payload says which in `narrative_source` (`claude` or `template`).
