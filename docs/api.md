# REST API

Base URL: `{{ENDPOINT}}/api/v1`. The OpenAPI document is at `{{ENDPOINT}}/api/v1/openapi.json`.
The [MCP server](mcp.md) uses the same query layer, so both return the same data.

## Authentication

```
Authorization: Bearer vt_...
```

API tokens are created by admins in the admin UI (**Tokens**) or with `POST /api/v1/tokens`.
Each token is either scoped to one project or to all projects, and is either `read` or `write`.
Signed-in admin UI sessions also work (cookie), which is handy for quick browser checks.

Errors are `{"error": "explanation"}` with a 4xx/5xx status. `401` means a missing or bad token,
`403` a token without access to that project or without write scope.

## Conventions

- `{p}` is a project slug or id. `{id}` for issues is the id or short id (`WEB-12`).
- Time parameters (`since`, `until`, `window`, `baseline`) take a relative duration (`30m`,
  `24h`, `7d`), an RFC 3339 timestamp, or unix milliseconds.
- Timestamps in responses are RFC 3339 UTC.
- Stack traces are crash site first: `frames[0]` is where the error was raised.

## Projects and keys

| Method & path | Description |
| --- | --- |
| `GET /me` | The current user or token and its access |
| `GET /projects` | Projects you can access |
| `POST /projects` | Create: `{"name", "platform", "slug"?}`. Platforms: javascript, android, ios, rust, node, jvm, go, dotnet, python, other. Returns `ingest_key` and `install_instructions_url` |
| `GET /projects/{p}` | One project |
| `PATCH /projects/{p}` | Admin: `{"name"?, "retention_days"?, "scrub_enabled"?, "scrub_fields"?}` (`scrub_fields`: comma-separated extra keys) |
| `POST /projects/{p}/keys` | Create another ingest key |
| `DELETE /projects/{p}/keys/{key}` | Revoke an ingest key |
| `GET /projects/{p}/install/{platform}` | Install guide as Markdown with key and endpoint filled in (`javascript`, `android`, `ios`, `rust`, `http`) |

## Issues and events

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/issues` | `status` (open\|resolved\|ignored\|all, default open), `query` (title/culprit text), `release`, `environment`, `since` (count window, default 24h), `sort` (last_seen\|first_seen\|times_seen\|events\|users), `limit` |
| `GET /issues/{id}` | Issue with latest event (symbolicated frames, source context, breadcrumbs, tags) and breakdowns by release, OS, device, browser |
| `PATCH /issues/{id}` | `{"status"?: "open"\|"resolved"\|"ignored", "resolved_in_release"?, "ignore_for"?: "7d", "assignee"?: "email" or ""}` |
| `GET /issues/{id}/events` | Recent events (`limit`) |
| `POST /issues/{id}/comments` | `{"body": "markdown"}` |
| `GET /projects/{p}/events` | Search error events: `query`, `level`, `release`, `environment`, `user_id`, `exception_type`, `since`, `until`, `limit` |
| `GET /projects/{p}/events/{event_id}` | One event in full |

A resolved issue that receives a new event is reopened and marked as a regression (and fires
`regression` alerts). With `resolved_in_release`, only events from that release or newer count.

## Releases

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/releases` | Known releases |
| `GET /projects/{p}/releases/health` | Crash-free sessions and users, sessions, errors and new issues per release; `release`, `since` |
| `GET /projects/{p}/releases/compare?base=1.2.0&target=1.3.0` | Health side by side, new issues, gone issues, issues that increased |
| `GET /projects/{p}/explain-spike` | Why errors went up: `window` (default `1h`) vs `baseline` (default the 24h before), ranked by issue, release, OS, device, country, exception type |

## Analytics

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/analytics/summary` | Visitors, pageviews, screen views, custom events, identified users, with the previous period |
| `GET /projects/{p}/analytics/timeseries` | Same metrics per `interval` (hour\|day\|week\|month) |
| `GET /projects/{p}/analytics/breakdown` | Top values of `dimension`, ranked by visitors; `limit` |

Common parameters: `since`, `until`, `type` (pageview\|screen_view\|custom), and repeated
`filter=dimension:value` (prefix the value with `~` for contains or `!` for not equal).

Dimensions: `path`, `url`, `hostname`, `referrer` (referrer host or UTM source), `referrer_url`,
`utm_source`, `utm_medium`, `utm_campaign`, `utm_term`, `utm_content`, `country`, `device`,
`device_model`, `browser`, `browser_version`, `os`, `os_version`, `app_version`, `release`,
`environment`, `platform`, `event` (event name), `type`, `screen`, and `prop:<name>` for custom
event properties.

```sh
curl -H "Authorization: Bearer $VAKA_TOKEN" \
  "{{ENDPOINT}}/api/v1/projects/web/analytics/breakdown?dimension=prop:plan&filter=event:signup&since=30d"
```

## Product analytics

Funnels, retention and paths work on the same events as the analytics endpoints and take the same
`since`, `until` and `filter=dimension:value` parameters (filters apply to every event considered).

A **step** is an event name, optionally with filters: `signup`, `pageview|path:/pricing`,
`screen_view|screen:Checkout`, `purchase|prop:plan:pro`. Find event names and their property keys
with the event catalog first.

**Identity** decides who counts as one person: `visitor` (cookieless visitor id: stable per install
on mobile, new every day for anonymous web visitors), `user` (only events after `identify()`), or
`auto` (user id when present, else visitor id).

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/analytics/events` | Event catalog: each event name with type, count, visitors, last seen and property keys |
| `GET /projects/{p}/analytics/funnel` | Repeat `step=` 2 to 10 times, in order. `window` (max time from first to last step, default `1d`), `identity` (default visitor), `breakdown` (a dimension; each person's first non-empty value), `limit` |
| `GET /projects/{p}/analytics/retention` | `start` (step that puts a person in a cohort the first time they do it; default any event), `return` (step that counts as coming back; default any event), `period` (day\|week, default week), `periods`, `identity` (default auto). `since` = earliest cohort |
| `GET /projects/{p}/analytics/paths` | `from` (a label such as `pageview /pricing`, `screen Checkout` or `signup`; omit to start at each visit's first event), `direction` (after\|before), `depth` (1 to 6, default 3), `identity`, `limit` |
| `GET /projects/{p}/users` | Identified users: event and error counts, first/last seen, last release, platform. `since`, `query` (id substring), `limit` |
| `GET /projects/{p}/users/{user_id}/timeline` | Everything one identified user did, newest first: events, errors/crashes with issue short ids, sessions. `since` (default 7d), `limit` |

```sh
# Signup funnel from the pricing page, split by traffic source
curl -G -H "Authorization: Bearer $VAKA_TOKEN" "{{ENDPOINT}}/api/v1/projects/web/analytics/funnel" \
  --data-urlencode 'step=pageview|path:/pricing' --data-urlencode 'step=signup' \
  --data-urlencode 'step=purchase' -d window=7d -d breakdown=utm_source -d since=30d
```

Funnel steps report `count`, `conversion_from_start_pct`, `conversion_from_previous_pct` and
`dropped_from_previous`. Retention returns one row per cohort with `size` and `retained[n]`
(`people`, `pct`), where `retained[0]` is the cohort period and `null` means the period has not
happened yet. Paths stay within one visit (the SDK session, or a calendar day when there is none) and
collapse consecutive repeats; `(end)` marks where the visit ended.

Anonymous web visitors get a new visitor id every day, so web retention beyond the first day only
says something about identified users (`identity=user`).

## Debug files

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/artifacts` | Uploaded files (`release` filter) |
| `POST /projects/{p}/artifacts` | Multipart, fields in this order: `kind` (sourcemap\|proguard\|native; dSYMs, ELF and PDB are `native`), `release` (required except for native, which is matched by debug id), `name` (for source maps: URL path of the minified file, e.g. `/assets/app.js`), then `file`. Max 1 GB |
| `DELETE /projects/{p}/artifacts/{id}` | Delete |

The [CLI](cli.md) wraps these; prefer `vaka upload ...` in build scripts.

## Privacy

| Method & path | Description |
| --- | --- |
| `DELETE /projects/{p}/users/{user_id}` | Delete all errors, events and sessions for a user id (write scope). Returns counts. Cannot be undone |

## Alerts

| Method & path | Description |
| --- | --- |
| `GET /alert-rules` | Rules, plus the valid triggers and channels |
| `POST /alert-rules` | See [alerts](alerts.md) |
| `PATCH /alert-rules/{id}` / `DELETE /alert-rules/{id}` | Update or delete |
| `POST /alert-rules/{id}/test` | Send a test notification |
| `GET /alert-rules/{id}/deliveries` | Recent deliveries and their status |

## Tokens and users (admins)

| Method & path | Description |
| --- | --- |
| `GET /tokens`, `POST /tokens` | Create: `{"name", "project"?, "scope": "read"\|"write", "expires_in_days"?}`. The token is returned once |
| `DELETE /tokens/{id}` | Revoke |
| `GET /users` | Users |
| `POST /users/invite` | `{"email", "role": "viewer"\|"member"\|"admin"\|"owner"}`; returns an invite link (also emailed if Resend is set up) |
| `PATCH /users/{id}` / `DELETE /users/{id}` | `{"role"?, "disabled"?}` |

Roles: **viewer** reads everything, **member** can also triage issues and upload debug files,
**admin** manages projects, tokens, users and alerts, **owner** is an admin who can manage other owners.
