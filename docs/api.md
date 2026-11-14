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
| `GET /issues/{id}` | Issue with latest event (symbolicated frames, source context, breadcrumbs, tags), breakdowns by release, OS, device, browser, `owners`, `suspect_commits`, `links` and `timeline_before_crash` (the last 30 session entries in the 10 minutes before the latest event) |
| `PATCH /issues/{id}` | `{"status"?: "open"\|"resolved"\|"ignored", "resolved_in_release"?, "ignore_for"?: "7d", "assignee"?: "email" or ""}` |
| `GET /issues/{id}/fix-context` | One compact bundle for fixing the issue: in-app frames with `source` links, last breadcrumbs, tags, suspect commits, owners, links, comments and `suggested` branch name, PR title and `Fixes <ID>` commit trailer |
| `GET /issues/{id}/precursors` | What users most often did in the `minutes` (default 5) before this issue: pages, screens and events with `share_of_crash_sessions`, `share_of_all_sessions` and `lift`, plus the most common breadcrumbs |
| `GET /projects/{p}/sessions/{session_id}/timeline` | Everything in one session, oldest first: `session_start`, `pageview`, `screen_view`, `event`, `breadcrumb`, `error`, `crash`, `session_end` (`limit`, default 500) |
| `GET /issues/{id}/links` | Pull requests, tickets and commits linked to the issue |
| `POST /issues/{id}/links` | `{"url", "kind"?: "pull_request"\|"issue"\|"commit"\|"other", "title"?}` (kind is guessed from the URL) |
| `DELETE /issues/{id}/links/{link}` | Remove a link |
| `GET /issues/{id}/events` | Recent events (`limit`) |
| `POST /issues/{id}/comments` | `{"body": "markdown"}` |
| `GET /projects/{p}/events` | Search error events: `query`, `level`, `release`, `environment`, `user_id`, `exception_type`, `since`, `until`, `limit` |
| `GET /projects/{p}/events/{event_id}` | One event in full |

A resolved issue that receives a new event is reopened and marked as a regression (and fires
`regression` alerts). With `resolved_in_release`, events from releases first seen before that
release don't count (old clients still crashing are expected).

## Performance

See [performance.md](performance.md) for spans and the query parameters.

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/performance/summary` | Per transaction: `count`, `per_minute`, `p50_ms`..`p99_ms`, `error_rate` (`op`, `sort`: p95\|p50\|p99\|count\|errors\|total) |
| `GET /projects/{p}/performance/endpoints` | Slowest `http.server*` endpoints by p95 (`min_count`, default 5) |
| `GET /projects/{p}/performance/app-start` | Cold/warm app start by release |
| `GET /projects/{p}/performance/web-vitals` | Web Vitals p75, rating and good/poor shares (`transaction` = page, `by_page`) |
| `GET /projects/{p}/performance/traces` | Slowest traces (`min_ms`, `op`, `transaction`) |
| `GET /projects/{p}/performance/breakdown` | Child spans of `transaction` by op and name, with share of time |
| `GET /projects/{p}/performance/compare` | `base`, `target`, `min_count`, `threshold`: regressions, improvements and Web Vitals changes |
| `GET /projects/{p}/traces/{trace_id}` | Span tree with `offset_ms`, `duration_ms`, `self_time_ms`, and errors in the trace |

## Releases

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/releases` | Known releases |
| `GET /projects/{p}/releases/health` | Crash-free sessions and users, sessions, errors, new issues and ANR/hang rates (`anrs`, `anr_sessions`, `anr_users`, `anr_rate_sessions`, `anr_rate_users`) per release; `release`, `since` |
| `GET /projects/{p}/releases/adoption` | Share of sessions and users per release over time: `since` (default 14d), `interval` (hour\|day), `environment`, `limit` (releases shown separately, default 5; the rest are `other`). Each release also gets `first_session`, `current_share_sessions` and `peak_share_sessions` |
| `GET /projects/{p}/releases/compare?base=1.2.0&target=1.3.0` | Health side by side, new issues, gone issues, issues that increased |
| `GET /projects/{p}/releases/{version}/commits` | Commits recorded for a release, with changed files |
| `PUT /projects/{p}/releases/{version}/commits` | Replace them: `{"repository"?: "https://github.com/o/r", "head_commit"?: "sha", "commits": [{"sha", "message"?, "author_name"?, "author_email"?, "timestamp"?, "files"?: ["src/a.ts"]}]}` (at most 1000, newest first). `head_commit` defaults to the first commit. Commit messages with `Fixes WEB-12`, `Closes WEB-3, WEB-4` or `Resolves WEB-5` resolve those issues in this release (returned as `resolved_issues`). `vaka releases set-commits` does this from `git log` |
| `GET /projects/{p}/explain-spike` | Why errors went up: `window` (default `1h`) vs `baseline` (default the 24h before), ranked by issue, release, OS, device, country, exception type |

### Suspect commits

When a release has commits recorded, `GET /issues/{id}` returns up to three `suspect_commits`:
commits in the issue's first release that changed files appearing in its in-app stack frames,
ranked toward the crash site. Each has `sha`, `message` (first line), `author`, `author_email`,
`committed_at`, `url` (GitHub/GitLab), `score`, `reasons` (which file and frame matched) and
`author_user` when the author's email belongs to a Vaka user. If the first release has no commits,
the newest earlier release with commits is used.

### Source links and code mappings

In-app frames in `GET /issues/{id}` and `GET /projects/{p}/events/{event_id}` get a `source`
object: `{repository, path, line, ref, url, mapped}`. `ref` is the release's head commit (from
set-commits), else the mapping's default branch. URLs use GitHub, GitLab or Bitbucket formats.

Code mappings turn stack paths into repository paths. Without mappings the release repository
and the (symbolicated) file name are used as a best guess (`mapped: false`).

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/code-mappings` | The mappings |
| `PUT /projects/{p}/code-mappings` | `{"mappings": [{"stack_root": "build/src/", "source_root": "packages/web/src/", "repository": "https://github.com/acme/shop", "default_branch": "main"}]}` |

The longest matching `stack_root` wins; it matches at the start of the cleaned frame path or at
any directory boundary, and `""` matches every path. `repository` defaults to the release's.

## Ownership

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/ownership` | The rules text, parsed rules and any problems |
| `PUT /projects/{p}/ownership` | `{"rules": "..."}` replaces them; invalid lines reject the whole update |
| `POST /projects/{p}/ownership/apply` | Re-apply the rules to open issues without owners (`?all=true`: every open issue) |

One rule per line, `<pattern> <owner>...`; the last matching line wins, `#` starts a comment.

```text
src/**                    #web-team               # path glob (default kind)
path:src/payments/**      alice@example.com
module:com.example.cart.* bob@example.com         # JVM/Swift module or class
type:TimeoutError         #platform               # exception type
tag:area=checkout         carol@example.com       # event tag
```

Path and module rules are checked against in-app frames from the crash site outwards; the first
frame that matches decides. Type and tag rules apply when no frame matched. A path pattern
starting with `/` is anchored at the repository root; otherwise it can match at any directory
(so `src/cart/**` matches `/home/ci/build/src/cart/a.ts`). New issues get `owners` set and are
assigned to the first owner email that belongs to a Vaka user. Existing issues without owners
get them computed when read; `POST /projects/{p}/ownership/apply` stores them.

## Analytics

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/analytics/summary` | Visitors, pageviews, screen views, custom events, identified users, with the previous period |
| `GET /projects/{p}/analytics/timeseries` | Same metrics per `interval` (hour\|day\|week\|month) |
| `GET /projects/{p}/analytics/breakdown` | Top values of `dimension`, ranked by visitors; `limit` |
| `GET /projects/{p}/analytics/realtime` | The last `minutes` (default 5, max 60): visitors, pageviews, active sessions, errors, top pages, referrers, countries and events, and visitors per minute for the last 30 minutes |

Common parameters: `since`, `until`, `type` (pageview\|screen_view\|custom), and repeated
`filter=dimension:value` (prefix the value with `~` for contains or `!` for not equal).

Dimensions: `path`, `url`, `hostname`, `referrer` (referrer host or UTM source), `referrer_url`,
`utm_source`, `utm_medium`, `utm_campaign`, `utm_term`, `utm_content`, `country`, `device`,
`device_model`, `browser`, `browser_version`, `os`, `os_version`, `app_version`, `release`,
`environment`, `platform`, `event` (event name), `type`, `screen`, and `prop:<name>` for custom
event properties. `filter=cohort:<name>` keeps only people in a saved cohort (`cohort:!<name>`
excludes them).

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
| `GET /projects/{p}/analytics/stickiness` | `active_in_window` (MAU for the default 30d), `wau`, `avg_dau`, `dau_over_active` (DAU/MAU) and `days_active_per_week`. `event` (step, default any), `identity` (default auto) |
| `GET /projects/{p}/analytics/lifecycle` | Per `period` (day\|week, default week): `new`, `returning`, `resurrecting`, `dormant` (negative), `active`, `quick_ratio`. `event`, `identity` |
| `GET /projects/{p}/cohorts` | Saved cohorts with definitions and current `size` (`sizes=false` skips counting) |
| `POST /projects/{p}/cohorts` | Create a cohort (see below) |
| `GET /projects/{p}/cohorts/{name}` | Definition, size and the most recently active `sample_members` (`limit`) |
| `DELETE /projects/{p}/cohorts/{name}` | Delete a cohort |
| `GET /projects/{p}/users` | Identified users: event and error counts, first/last seen, last release, platform. `since`, `query` (id substring), `limit` |
| `GET /projects/{p}/users/{user_id}/timeline` | Everything one identified user did, newest first: events, errors/crashes with issue short ids, sessions. `since` (default 7d), `limit` |

| `GET /projects/{p}/goals` | Saved goals with `conversions`, `converters`, `conversion_rate` (converters / visitors) and `revenue` per currency in the window |
| `POST /projects/{p}/goals` | `{"name": "Signup", "step": "signup", "value_prop"?: "revenue"}` |
| `DELETE /projects/{p}/goals/{id or name}` | Delete a goal |
| `GET /projects/{p}/analytics/revenue` | Revenue per currency: `event` (step, default `purchase`), `value_prop` (default `revenue`), `currency_prop` (default `currency`), optional `breakdown` dimension, `limit`. Rows have `revenue`, `conversions`, `customers`, `average` |

Funnel responses include `conversion_time` (median, p90 and a distribution of the time from the
first to the last step for people who finished). `breakdown=cohort:<name>` splits a funnel into
people in and not in a cohort.

### Logs

`GET /projects/{p}/logs?q=&severity=&trace_id=&service=&since=` searches logs sent over OTLP. Send
them, and traces, to `/ingest/otlp/v1/logs` and `/ingest/otlp/v1/traces`. See [opentelemetry.md](opentelemetry.md).

### Anomalies and digests

`GET /projects/{p}/anomalies?window=24h` compares the last window with the same window 1 to 4 weeks
earlier and returns anomalies with sentences, evidence and a suggested next tool.
`GET /projects/{p}/digest/preview?frequency=daily|weekly` builds a digest without sending it.
Digest alert rules take `schedule: {frequency, hour, weekday, timezone}`. See [anomalies.md](anomalies.md).

### Feature flags and experiments

See [flags.md](flags.md): `GET/POST /projects/{p}/flags`, `GET/PATCH/DELETE /projects/{p}/flags/{key}`,
`GET /projects/{p}/flags/{key}/health`, `GET /projects/{p}/flags/{key}/results?goal=`,
`GET/POST /projects/{p}/experiments`, `GET /projects/{p}/experiments/{name}/results`. SDKs evaluate
flags with `POST /ingest/v1/flags`.

### Cohorts

A cohort is a saved group of people, evaluated when it is used. All `criteria` must hold over a
look-back window from now; `properties` must match at least one of the person's events:

```sh
curl -X POST {{ENDPOINT}}/api/v1/projects/web/cohorts -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{
    "name": "power_buyers", "identity": "user", "within": "30d",
    "criteria": [{"event": "purchase", "min_count": 3}, {"step": "support_ticket", "negate": true, "within": "14d"}],
    "properties": {"country": "SE"}
  }'
```

Criterion fields: `event` or `step` (step syntax), `filters`, `min_count` (default 1), `within`
(default the cohort's, 30d), `negate` (did not do it). `identity`: `auto` (default), `user` or
`visitor`.

Revenue is the sum of a numeric event property, per currency as sent (no conversion). Track
purchases as a custom event such as `purchase` with props `{"revenue": 19.5, "currency": "USD"}`.

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

## User feedback

See [feedback](feedback.md). Contact emails are returned to write tokens only.

| Method & path | Description |
| --- | --- |
| `GET /projects/{p}/feedback` | List, newest first. Filters: `status` (`open` = new + triaged, default; `all`; or a list of new, triaged, resolved, ignored), `sentiment`, `topic`, `release`, `environment`, `platform`, `source` (widget, crash_prompt, api), `flag` (`key` or `key=variant`), `issue`, `user`, `session`, `has_screenshot`, `query` (text), `since` (default `30d`), `limit`, `offset` |
| `GET /projects/{p}/feedback/summary` | Counts by `by` = sentiment (default), topic, release, flag, day, status, platform or source, with sentiment split, negative rate and quotes; `flag`, `since`, `include_closed`, `limit` |
| `GET /feedback/{id}` | One item by id or short id (`WEB-F12`), with attachments and linked issue |
| `PATCH /feedback/{id}` | `{status?, assignee? (email or ""), sentiment?, topics?, issue? (id, short id or "")}` (write scope) |
| `GET /feedback/{id}/attachments/{n}` | Screenshot bytes (`n` from 0) |

## Privacy

| Method & path | Description |
| --- | --- |
| `DELETE /projects/{p}/users/{user_id}` | Delete all errors, events, sessions and feedback for a user id (write scope). Returns counts. Cannot be undone |

## Alerts

| Method & path | Description |
| --- | --- |
| `GET /alert-rules` | Rules, plus the valid triggers, channels and filters |
| `POST /alert-rules` | `{project?, name?, trigger, channel, target, secret?, threshold?, filters?, max_per_hour?}`; see [alerts](alerts.md) |
| `PATCH /alert-rules/{id}` / `DELETE /alert-rules/{id}` | Update (`name`, `target`, `secret`, `threshold`, `enabled`, `filters`, `max_per_hour`) or delete |
| `POST /alert-rules/{id}/test` | Send a test notification |
| `GET /alert-rules/{id}/deliveries` | Recent deliveries: status, attempts, next retry; `payload=true`, `limit` |
| `POST /alert-rules/{id}/deliveries/{delivery}/redeliver` | Send a stored payload again |

## Tokens and users (admins)

| Method & path | Description |
| --- | --- |
| `GET /tokens`, `POST /tokens` | Create: `{"name", "project"?, "scope": "read"\|"write", "expires_in_days"?}`. The token is returned once |
| `DELETE /tokens/{id}` | Revoke |
| `GET /users` | Users |
| `POST /users/invite` | `{"email", "role": "viewer"\|"member"\|"admin"\|"owner"}`; returns an invite link (also emailed if Resend is set up) |
| `PATCH /users/{id}` / `DELETE /users/{id}` | `{"role"?, "disabled"?}` |
| `GET /admin/read-only`, `PUT /admin/read-only` | Read-only maintenance mode for a metadata database move: `{"enabled": true\|false, "drain_seconds"?}`. Admins and write tokens for all projects. While on, metadata writes return 503 ([self-hosting](self-hosting.md#moving-from-sqlite-to-postgres)) |

Roles: **viewer** reads everything, **member** can also triage issues and upload debug files,
**admin** manages projects, tokens, users and alerts, **owner** is an admin who can manage other owners.
