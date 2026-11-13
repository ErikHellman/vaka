# Agent-era roadmap

Status: in progress (started 2026-11-13). Owner: Erik + Claude Code.

Vaka already has issues with symbolication, ownership rules, suspect commits from
`vaka releases set-commits`, alert rules (webhook, Slack, email) for new issues,
regressions, spikes and daily digests, and product analytics (funnels, retention,
paths, goals, revenue). This roadmap closes the gaps that matter when the main
consumer of the data is a coding agent: everything an agent needs to go from
"something broke" to "here is a PR" must be reachable through the REST API and MCP,
and Vaka must be able to wake an agent up.

Every feature below ships with REST endpoints, MCP tools, OpenAPI entries, docs and
smoke-test coverage. Work lands as stacked PRs, one per feature, each merged as soon
as CI is green.

Order: **2 → 5 → 1 → 3 → 4 → 6 → 7 → 8**.

---

## 2. Source and commit linking (fix context for agents)

Goal: for any issue an agent gets the exact files and lines in the repository, the
commit that probably caused it, who owns the code, and whether someone is already
working on it.

What exists: ownership rules (`ownership.rs`), owners stored on new issues,
`release_commits` with suspect-commit scoring for the issue's first release.

New:

- **Code mappings** (migration 005, table `code_mappings`): per project, a list of
  `{stack_root, source_root, repository, default_branch}`. A frame path starting with
  `stack_root` is rewritten to `source_root` + rest. Lets `webpack:///./src/x.ts`,
  `/home/ci/build/src/x.rs` or `com/example/Cart.kt` map to `app/src/main/java/...`.
- **Release head commit**: `releases.head_commit` (set from `set-commits`, explicit
  `head_commit` or the newest commit). Source links pin to it; fall back to the
  mapping's default branch.
- **Source links on frames**: every in-app frame in `get_issue` / `get_event` gets
  `source: {repository, path, line, url}` with GitHub, GitLab and Bitbucket URL
  formats.
- **Owners at read time**: issues created before rules existed get owners computed
  on read (`owners_source: "rules"`). `POST /projects/{p}/ownership/apply` re-applies
  rules to open issues.
- **Suspect commits across releases**: if the first release has no commits, fall back
  to the newest earlier release that has commits; the author is matched to a Vaka user.
- **Issue links**: table `issue_links` (`issue_id, kind, url, title, created_by`).
  Kinds: `pull_request`, `issue`, `commit`, `other`. Agents record "PR opened" so
  other agents don't duplicate work. REST `GET/POST /issues/{id}/links`,
  `DELETE /issues/{id}/links/{link}`; MCP `link_issue`.
- **Fix-by-commit**: when commits are set for a release, messages containing
  `Fixes WEB-12` / `Closes` / `Resolves` (case-insensitive, short ids of this project)
  resolve that issue in that release, add a comment and a `commit` link.
- **`get_fix_context` MCP tool / `GET /issues/{id}/fix-context`**: one compact bundle
  for an agent: issue summary, the crash's in-app frames with source links and
  context lines, last breadcrumbs, tags, suspect commits, owners, existing links,
  repository + ref, suggested branch name (`vaka/web-12-typeerror-x-is-undefined`)
  and the commit trailer that auto-resolves the issue (`Fixes WEB-12`).
- CLI: `vaka code-mappings set|get`.

## 5. Webhooks and agent triggers

Goal: Vaka pushes a complete, signed, retried event to whatever should act on it,
including a coding agent in CI.

What exists: `alert_rules` with webhook/slack/email channels, HMAC signature,
fire-and-forget delivery log.

New:

- **Event types**: `new_issue`, `regression`, `spike`, `digest` plus
  `issue_resolved`, `issue_assigned`, `issue_commented`, `threshold` (issue reaches N
  events) and `release_created`.
- **Rule filters** (migration 006, JSON `filters` column): `environment`,
  `min_level`, `release` glob, `owner`, `tag:<k>=<v>`, `handled` (only unhandled).
- **Rich payloads**: issue webhooks embed the fix context from feature 2 so a
  receiver can act without calling back. Headers: `X-Vaka-Event`, `X-Vaka-Delivery`,
  `X-Vaka-Timestamp`, `X-Vaka-Signature: sha256=<hmac(body)>` (unchanged).
- **Reliable delivery**: deliveries are rows with `attempts`/`next_attempt_at`;
  a worker retries failed deliveries with exponential backoff (1m, 5m, 30m, 2h, 6h).
  REST `POST /alert-rules/{id}/deliveries/{delivery}/redeliver`.
- **Agent channels**:
  - `github_dispatch`: sends `repository_dispatch` (`event_type: vaka.<trigger>`,
    `client_payload` = slim fix context) to `owner/repo` using a token stored as the
    rule secret. Pairs with a ready-made GitHub Actions workflow
    (`docs/agents/vaka-fix.yml`) that runs a coding agent with the Vaka MCP server
    and opens a PR containing `Fixes WEB-12`.
  - `github_issue`: opens a GitHub issue with the fix context (optionally with an
    agent mention line) and records it as an issue link.
- **Rate limits**: `max_per_hour` per rule so agent triggers can't flood a repo.
- MCP tools: `list_alert_rules`, `update_alert_rule`, `delete_alert_rule`,
  `test_alert_rule`, `list_alert_deliveries`.

## 1. Session timelines before crashes

Goal: "what did the user do in the minutes before this crash" as one ordered list.

- Breadcrumb normalisation: `{timestamp, category, type, level, message, data}`;
  well-known categories `navigation`, `http`, `ui.click`, `console`, `lifecycle`.
- **Session timeline**: merge of analytics events (pageviews, screen views, custom
  events), errors and their breadcrumbs for a `session_id`, sorted, de-duplicated.
  REST `GET /projects/{p}/sessions/{session_id}/timeline`, MCP
  `get_session_timeline`.
- `get_issue` and fix context include `timeline_before_crash` (last 30 entries
  within 10 minutes before the latest event).
- **Crash precursors**: `GET /issues/{id}/precursors` / MCP `issue_precursors`:
  the most common events/pages/breadcrumbs in the minutes before this issue across
  sessions, compared with the project baseline (lift).
- SDKs: JS records navigation, click, console, fetch (with status and duration);
  Android/iOS breadcrumbs follow in their SDK threads.

## 3. Performance tracing

Goal: agents can find slow endpoints, slow app starts, poor Web Vitals and
performance regressions between releases.

- ClickHouse table `spans` (trace_id, span_id, parent_span_id, name, op, kind,
  status, start, duration_ms, transaction, release, environment, platform,
  session_id, user_id, attributes, measurements). Errors get a `trace_id` column.
- Ingest item `span` in `/ingest/v1/batch` (plus OTLP in feature 8).
- Queries: `performance_summary` (per transaction p50/p75/p95/p99, throughput,
  error rate), `slow_endpoints`, `app_startup` (cold/warm by release),
  `web_vitals` (LCP, INP, CLS, FCP, TTFB p75 + good/needs-improvement/poor share,
  by path), `get_trace` (span tree), `span_breakdown` (where time goes within a
  transaction), `compare_release_performance` (p95 regressions with minimum counts).
- Alert trigger `perf_regression`: hourly check of the newest release against the
  previous one.
- SDKs: JS pageload span, fetch spans, Web Vitals (`tracesSampleRate`); Rust
  `vaka_sdk::start_span` / `continue_span` (W3C traceparent); Android and iOS app-start spans
  in their SDK threads.

## 4. Funnels, retention and cohorts via MCP

Funnels, retention and paths exist. New:

- **Cohorts** (migration, table `cohorts`): saved definitions — performed event
  (optionally at least N times within a window), did-not-perform, property
  filters, identified users only. `cohort_size`, sample members.
- `cohort:<name>` usable as a filter in analytics, funnels and retention.
- `analytics_stickiness` (DAU/WAU/MAU, days active per week) and
  `analytics_lifecycle` (new, returning, resurrecting, dormant per period).
- Funnel `breakdown=cohort` and conversion time distribution.
- MCP: `create_cohort`, `list_cohorts`, `get_cohort`, `delete_cohort`,
  `analytics_stickiness`, `analytics_lifecycle`.

## 6. Feature flags and experiments

- Flags in the metadata DB: key, description, variants with weights, rollout
  percentage, targeting rules (user ids, property matches), per-environment enable.
  Deterministic bucketing: `sha256(flag_key + ":" + distinct_id)`.
- Evaluation endpoint for SDKs: `POST /ingest/v1/flags` (ingest key) returns all
  evaluated flags for an identity.
- Exposure: SDKs attach active flags to errors, sessions and events (`flags` map
  column) so every metric can be split by flag.
- **Release health by flag**: crash-free sessions, error rate and issues
  over-represented per variant (`flag_health`).
- **Experiments**: flag + goal/metric; results per variant with conversion,
  uplift, confidence interval and significance (two-proportion z-test).
- MCP: `list_flags`, `get_flag`, `create_flag`, `update_flag`, `delete_flag`,
  `flag_health`, `experiment_results`. An agent can roll a bad variant back.
- SDK: JS `getFlag` / `isEnabled`; Android/iOS in their threads.

## 7. Scheduled natural-language anomaly digests

- **Anomaly detection** over errors (per issue and total), crash-free rate per
  release, traffic, goal conversions, p95 latency of top transactions, Web Vitals and
  flag variants: current window vs. same window on previous weeks, robust z-score
  with minimum volumes.
- `GET /projects/{p}/anomalies`, MCP `detect_anomalies`.
- Each anomaly carries a plain-English sentence ("Checkout conversion fell from
  4.1% to 2.3% since release 2.4.0 (−44%)") with evidence and a suggested next tool.
- Digest rules get schedules: `daily` or `weekly` with hour, weekday and IANA time
  zone, plus an optional `focus` (free text). If `VAKA_ANTHROPIC_API_KEY` is set the
  narrative is written by Claude from the anomaly list and focus; otherwise
  template sentences are used. MCP `preview_digest`.

## 8. OpenTelemetry ingestion

- OTLP/HTTP receivers: `POST /v1/traces` and `POST /v1/logs` (protobuf and JSON),
  authenticated by ingest key in `x-vaka-key` or `Authorization: Bearer`.
- Resource mapping: `service.version` → release, `deployment.environment` →
  environment, `service.name` kept as an attribute.
- Spans → `spans` table (feature 3). Span events named `exception` and ERROR logs
  with `exception.*` attributes → error events (grouped into issues).
- ClickHouse table `logs`; `search_logs` (text, severity, trace id, attributes) and
  logs inside `get_trace`.
- Docs: OpenTelemetry Collector and SDK exporter configuration.

---

## Delivery

| PR | Content |
| --- | --- |
| 1 | This plan |
| 2 | Source/commit linking, fix context |
| 3 | Webhooks and agent triggers |
| 4 | Session timelines |
| 5 | Performance tracing (server + JS/Rust SDK) |
| 6 | Cohorts, stickiness, lifecycle |
| 7 | Feature flags and experiments (server + JS SDK) |
| 8 | Anomaly digests |
| 9 | OTLP ingestion |

Android and iOS SDK parts (breadcrumbs, app-start spans, flags) run in separate
threads; iOS verification needs Erik's Mac.

## Progress

- [x] 2 Source/commit linking
- [x] 5 Webhooks and agent triggers
- [x] 1 Session timelines
- [x] 3 Performance tracing
- [x] 4 Cohorts
- [ ] 6 Feature flags and experiments
- [ ] 7 Anomaly digests
- [ ] 8 OTLP ingestion
