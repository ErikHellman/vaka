# Alerts

Alert rules notify a webhook or email address. Create them in the admin UI (**Alerts**), with
`POST /api/v1/alert-rules`, or with the MCP tool `create_alert_rule`.

## Triggers

| Trigger | Fires when |
| --- | --- |
| `new_issue` | An error is seen for the first time |
| `regression` | A resolved issue gets a new event |
| `spike` | An issue has at least `threshold` events (default 50, minimum 10) in the last hour and more than 3× its hourly average over the previous day. Checked every 5 minutes |

A rule without `project` applies to all projects. A spike rule notifies at most once per issue
every 6 hours.

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

For email, set `"channel": "email"` and `"target"` to one or more comma-separated addresses.
Email needs `VAKA_RESEND_API_KEY` and `VAKA_EMAIL_FROM` on the server.

Test a rule with `POST /api/v1/alert-rules/{id}/test` and see results with
`GET /api/v1/alert-rules/{id}/deliveries`.

## Webhook payload

`POST` with `Content-Type: application/json` and `User-Agent: Vaka-Webhook/1`:

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

For `spike`, `details` is `{"last_hour": 140, "hourly_avg_prev_24h": 12.5}`. Non-2xx responses and
timeouts (10 s) are recorded as failed deliveries.

## Verifying signatures

When the rule has a `secret`, the request carries
`X-Vaka-Signature: sha256=<hex HMAC-SHA256 of the raw body with the secret>`.

```js
import crypto from 'node:crypto';
const expected = 'sha256=' + crypto.createHmac('sha256', secret).update(rawBody).digest('hex');
const ok = crypto.timingSafeEqual(Buffer.from(expected), Buffer.from(req.headers['x-vaka-signature'] ?? ''));
```
