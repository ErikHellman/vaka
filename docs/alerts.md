# Alerts

Alert rules notify a webhook, a Slack channel, an email address, or GitHub (to wake up a coding
agent). Create them in the admin UI (**Alerts**), with `POST /api/v1/alert-rules`, or with the MCP
tool `create_alert_rule`.

## Triggers

| Trigger | Fires when |
| --- | --- |
| `new_issue` | An error is seen for the first time |
| `regression` | A resolved issue gets a new event |
| `spike` | An issue has at least `threshold` events (default 50, minimum 10) in the last hour and more than 3× its hourly average over the previous day. Checked every 5 minutes |
| `threshold` | An open issue reaches `threshold` events (default 100). Once per issue, checked every minute |
| `issue_resolved` | An issue is resolved (by a person, an agent or a `Fixes WEB-12` commit) |
| `issue_assigned` | An issue is assigned |
| `issue_commented` | Someone comments on an issue (`event.actor` says who, so an agent can ignore its own comments) |
| `release_created` | Vaka sees a release for the first time (first event or `set-commits`) |
| `perf_regression` | The newest release with spans is slower than the previous one: a transaction's p95 grew by more than `threshold` percent (default 20) or a Web Vital got worse. Once per release, checked every 10 minutes. See [performance](performance.md) |
| `new_feedback` | A user sends feedback. Sent once tagging and crash linking are done (at most 2 minutes later), so filters can use sentiment and topics. Not for `github_issue` |
| `digest` | Daily or weekly on a `schedule` in your time zone (default: daily at `threshold` o'clock UTC): per project, errors and new issues with the change from the previous period, top issues, crash-free sessions, traffic and [detected anomalies](anomalies.md), opened by a short narrative. Quiet projects are left out |

A rule without `project` applies to all projects. A spike rule notifies at most once per issue
every 6 hours.

## Filters and rate limits

`filters` narrows a rule (all given filters must match):

| Filter | Matches |
| --- | --- |
| `environment` | Glob on the event's environment (`production`, `prod*`) |
| `min_level` | `debug`, `info`, `warning`, `error` or `fatal` and above |
| `release` | Glob on the event's release (`2.*`) |
| `owner` | Issue owner or assignee email |
| `unhandled_only` | `true`: only unhandled errors |
| `tags` | `{"area": "checkout*"}`: every tag must match its glob |
| `sentiment` | `new_feedback`: `negative`, or a list such as `["negative", "mixed"]` |
| `topic` | `new_feedback`: glob on any topic (`checkout*`) |
| `linked_to_issue` | `new_feedback`: `true` only feedback about a crash, `false` only the rest |
| `has_screenshot` | `new_feedback`: `true` or `false` |
| `mention` | `github_issue` only: first line of the issue body, e.g. a mention that starts an agent |

`environment`, `unhandled_only` and `tags` apply to triggers caused by an error event
(`new_issue`, `regression`). `max_per_hour` caps deliveries per rule (default 10 for GitHub
channels, otherwise unlimited); extra deliveries are recorded as `rate_limited`.

## Create a rule

```sh
curl -X POST {{ENDPOINT}}/api/v1/alert-rules -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{
    "project": "web",
    "name": "New web errors to Slack bridge",
    "trigger": "new_issue",
    "channel": "webhook",
    "target": "https://hooks.example.com/vaka",
    "secret": "a-shared-secret"
  }'
```

For Slack, create an [incoming webhook](https://api.slack.com/messaging/webhooks) and use
`"channel": "slack"` with the webhook URL as `"target"`. Messages show the issue, culprit, release,
counts and owners, linked to the issue. Slack-compatible webhooks (Mattermost) work too.

For a daily digest to Slack at 07:00 UTC:

```sh
curl -X POST {{ENDPOINT}}/api/v1/alert-rules -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"trigger": "digest", "channel": "slack", "target": "https://hooks.slack.com/services/…", "threshold": 7}'
```

Testing a digest rule sends today's digest right away. For weekly digests or a local hour, set
`schedule` (see [anomalies.md](anomalies.md#scheduled-digests)).

For email, set `"channel": "email"` and `"target"` to one or more comma-separated addresses.
Email needs `VAKA_RESEND_API_KEY` and `VAKA_EMAIL_FROM` on the server.

### SMS

For SMS, set `"channel": "sms"` and `"target"` to one or more comma-separated phone numbers in
E.164 format (`+46701234567`, up to 10). Each alert is one short text message that fits a single
SMS segment (160 characters, or 70 with emoji or other non-GSM characters): the trigger, project,
issue id, a shortened title and the issue link. Digests become a one-line summary. SMS rules default
to `max_per_hour: 10`, and the server sends at most `VAKA_SMS_MAX_PER_DAY` SMS deliveries per 24 hours
(default 100); deliveries over the cap are marked `failed` and not retried.

```sh
curl -X POST {{ENDPOINT}}/api/v1/alert-rules -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"project": "web", "trigger": "new_issue", "channel": "sms", "target": "+46701234567",
       "filters": {"environment": "production", "min_level": "fatal"}}'
```

The SMS provider is configured once per server:

| Provider | `VAKA_SMS_PROVIDER` | Credentials |
|---|---|---|
| [46elks](https://46elks.com) (default) | `46elks` | `VAKA_46ELKS_USERNAME`, `VAKA_46ELKS_PASSWORD` (API username and password) |
| [Twilio](https://www.twilio.com) | `twilio` | `VAKA_TWILIO_ACCOUNT_SID`, `VAKA_TWILIO_AUTH_TOKEN` |
| [Sinch](https://sinch.com) | `sinch` | `VAKA_SINCH_SERVICE_PLAN_ID`, `VAKA_SINCH_API_TOKEN`, optional `VAKA_SINCH_REGION` (`us` default, `eu`, `au`, `br`, `ca`) |

`VAKA_SMS_FROM` is the sender (default `Vaka`). 46elks accepts an alphanumeric sender of up to 11
characters; Twilio needs one of your Twilio numbers, and Sinch usually a number too.

Test a rule with `POST /api/v1/alert-rules/{id}/test` and see results with
`GET /api/v1/alert-rules/{id}/deliveries` (`?payload=true` includes what was sent).

## Delivery and retries

Every delivery is recorded with its payload. A failed delivery (non-2xx, timeout) is retried after
1 minute, 5 minutes, 30 minutes, 2 hours and 6 hours, then marked `failed`. Statuses: `ok`,
`retrying`, `failed`, `rate_limited`, `skipped`. Send one again with
`POST /api/v1/alert-rules/{id}/deliveries/{delivery}/redeliver` (MCP `redeliver_alert`).

## Agent triggers (GitHub)

Two channels hand an issue to a coding agent. Both take `"target": "owner/repo"` and a GitHub
token as `"secret"` (fine-grained: Contents read/write for dispatch, Issues read/write for issues).

- `github_dispatch` sends a
  [`repository_dispatch`](https://docs.github.com/en/rest/repos/repos#create-a-repository-dispatch-event)
  event `vaka.<trigger>` (e.g. `vaka.new_issue`). `client_payload` holds `issue` (short id, title,
  culprit, releases, owners), `url`, `api_url` and `fix` (repository, ref, top in-app frames with
  source paths and links, suggested branch, PR title and `Fixes <ID>` trailer, top suspect commit).
- `github_issue` opens one GitHub issue per Vaka issue with the stack, source links, suspect
  commits, owners and breadcrumbs, and links it on the Vaka issue. Later alerts for the same issue
  (regressions, spikes) become comments. Put an agent mention in `filters.mention` to start it.

```sh
curl -X POST {{ENDPOINT}}/api/v1/alert-rules -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{
    "project": "web", "trigger": "new_issue", "channel": "github_dispatch",
    "target": "acme/shop", "secret": "github_pat_…",
    "filters": {"environment": "production", "unhandled_only": true}, "max_per_hour": 5
  }'
```

A workflow that runs a coding agent on each dispatch and opens a PR:

```yaml
# .github/workflows/vaka-fix.yml
name: Fix Vaka issue
on:
  repository_dispatch:
    types: [vaka.new_issue, vaka.regression]
permissions:
  contents: write
  pull-requests: write
jobs:
  fix:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event.client_payload.fix.ref || github.event.repository.default_branch }}
      - uses: anthropics/claude-code-action@v1
        with:
          anthropic_api_key: ${{ secrets.ANTHROPIC_API_KEY }}
          claude_args: >-
            --mcp-config '{"mcpServers":{"vaka":{"type":"http","url":"{{ENDPOINT}}/mcp","headers":{"Authorization":"Bearer ${{ secrets.VAKA_TOKEN }}"}}}}'
            --allowedTools mcp__vaka__get_fix_context,mcp__vaka__link_issue,mcp__vaka__comment_issue,Bash,Edit,Read,Write
          prompt: |
            Vaka reported ${{ github.event.client_payload.issue.short_id }}: ${{ github.event.client_payload.issue.title }}.
            Call get_fix_context for it, find the root cause, fix it on branch
            ${{ github.event.client_payload.fix.branch }} with a commit message ending in
            "${{ github.event.client_payload.fix.commit_trailer }}", open a pull request, then call
            link_issue with the PR URL and comment_issue with the root cause.
```

For any other agent runner, use a `webhook` rule: issue payloads include the full `fix_context`.

## Webhook payload

`POST` with `Content-Type: application/json`, `User-Agent: Vaka-Webhook/1`, `X-Vaka-Event`
(the trigger), `X-Vaka-Delivery` (delivery id, stable across retries) and `X-Vaka-Timestamp`
(unix seconds):

```json
{
  "trigger": "new_issue",
  "project": {"id": "…", "slug": "web", "name": "Web"},
  "issue": {"id": "…", "short_id": "WEB-12", "title": "TypeError: x is undefined", "culprit": "checkout (src/cart.ts:42)", "level": "error", "platform": "javascript", "status": "open", "times_seen": 1, "first_seen": "…", "last_seen": "…", "first_release": "1.4.0", "last_release": "1.4.0"},
  "url": "{{ENDPOINT}}/ui/projects/web/issues/…",
  "api_url": "{{ENDPOINT}}/api/v1/issues/…",
  "details": {},
  "sent_at": "2026-11-10T08:00:00Z"
}
```

Issue payloads also carry `issue.owners` (from [ownership rules](api.md#ownership)), `event`
(what caused the alert: `{environment, level, release, handled, tags}` for errors,
`{actor, ...}` for resolve/assign/comment) and `fix_context` (the same object as
`GET /api/v1/issues/{id}/fix-context`: in-app frames with source links, breadcrumbs, suspect
commits, links and suggested branch and commit trailer). Slack and email messages stay short.

A `new_feedback` webhook gets `{"trigger", "project", "feedback": {"id", "short_id", "message", "source", "sentiment", "topics", "summary", "release", "url", "issue_short_id", "attachments", ...}, "url", "api_url", "sent_at"}`. Contact details are never included.

A `release_created` webhook gets `{"trigger", "project", "release": {"version"}, "api_url", "sent_at"}`.

A digest webhook gets `{"trigger": "digest", "title", "schedule", "period": {"since", "until", "label"}, "narrative", "narrative_source", "projects": [{"project", "anomalies", "errors", "errors_prev", "new_issue_count", "new_issues", "top_issues", "sessions", "crash_free_sessions", "visitors", "visitors_prev", "pageviews", "pageviews_prev"}], "sent_at"}`.

For `spike`, `details` is `{"last_hour": 140, "hourly_avg_prev_24h": 12.5}`; for `threshold`,
`{"threshold": 100}`. Non-2xx responses and timeouts (10 s) are retried (see above).

## Verifying signatures

When the rule has a `secret`, the request carries
`X-Vaka-Signature: sha256=<hex HMAC-SHA256 of the raw body with the secret>`.

```js
import crypto from 'node:crypto';
const expected = 'sha256=' + crypto.createHmac('sha256', secret).update(rawBody).digest('hex');
const ok = crypto.timingSafeEqual(Buffer.from(expected), Buffer.from(req.headers['x-vaka-signature'] ?? ''));
```
