# MCP server

Vaka is an MCP server, so coding agents can query crashes and analytics, triage issues and set
up new projects without a dashboard.

- Endpoint: `{{ENDPOINT}}/mcp` (Streamable HTTP, stateless JSON responses)
- Auth: `Authorization: Bearer vt_...` (an [API token](api.md#authentication))
- Protocol versions: 2025-06-18, 2025-03-26, 2024-11-05

Use a **read** token for agents that only investigate, and a **write** token for agents that
also resolve issues, comment, create alert rules or delete user data. A token scoped to all
projects with write access can create projects.

## Connect

Claude Code:

```sh
claude mcp add --transport http vaka {{ENDPOINT}}/mcp --header "Authorization: Bearer vt_..."
```

Any client that takes a JSON config:

```json
{
  "mcpServers": {
    "vaka": {
      "type": "http",
      "url": "{{ENDPOINT}}/mcp",
      "headers": {"Authorization": "Bearer vt_..."}
    }
  }
}
```

## Tools

Read (everything a read token can call):

| Tool | What it does |
| --- | --- |
| `list_projects` | Projects the token can access, with platform, retention and ingest keys |
| `get_install_instructions` | Install guide for a platform with the project's endpoint and key filled in |
| `list_issues` | Grouped errors with event and user counts in a window (`status`, `query`, `release`, `environment`, `since`, `sort`, `limit`) |
| `get_issue` | Latest event with symbolicated stack trace (`frames[0]` is the crash site), source context, breadcrumbs, tags, breakdowns, `owners` and `suspect_commits` |
| `get_fix_context` | Start here to fix a crash: in-app frames with repository source links, breadcrumbs, suspect commits, owners, linked PRs, suggested branch and `Fixes <ID>` trailer |
| `get_session_timeline` | One session in order: pageviews, screens, events, breadcrumbs, errors, crashes, feedback |
| `issue_precursors` | What users did in the minutes before an issue across sessions, with lift versus all sessions |
| `performance_summary` | Latency percentiles, throughput and error rate per transaction |
| `slow_endpoints` | Slowest HTTP server endpoints by p95 |
| `app_startup` | Cold and warm app start times by release |
| `web_vitals` | LCP, INP, CLS, FCP, TTFB p75 with ratings, overall and by page |
| `list_slow_traces` | Slowest individual traces |
| `get_trace` | One trace as a span tree with self time, errors and logs |
| `search_logs` | Search OpenTelemetry logs by text, severity, trace, service and release |
| `span_breakdown` | Where a transaction's time goes, by child span |
| `compare_release_performance` | p95 and Web Vitals regressions between two releases |
| `get_code_mappings` | How stack paths map to repository paths |
| `list_issue_events` | Recent individual events for an issue |
| `get_event` | One event in full |
| `search_events` | Search events by text, level, release, environment, user id or exception type |
| `release_health` | Crash-free sessions and users, sessions, errors, new issues and ANR/hang rates per release |
| `compare_releases` | Two releases side by side: new, gone and increased issues |
| `explain_spike` | Why errors went up: recent window vs baseline, ranked by issue, release, OS, device, country, exception type |
| `analytics_summary` | Visitors, pageviews, screen views, custom events, with the previous period |
| `analytics_timeseries` | Metrics per hour, day, week or month |
| `analytics_breakdown` | Top values of a dimension (pages, referrers, countries, browsers, events, `prop:<name>`) |
| `top_pages` | Most visited pages |
| `list_event_names` | Event catalog: names, counts and property keys (use before funnels) |
| `analytics_funnel` | Conversion through 2 to 10 ordered steps within a window, with optional breakdown |
| `analytics_retention` | Day or week cohorts by first start event, and who came back |
| `analytics_paths` | Most common sequences after or before a page, screen or event |
| `analytics_revenue` | Revenue per currency from a purchase event, optionally by referrer, campaign, country... |
| `list_goals` | Saved goals with conversions, conversion rate and revenue |
| `realtime_visitors` | Traffic in the last few minutes: visitors, top pages, referrers, errors |
| `release_adoption` | Share of sessions and users per release over time |
| `list_identified_users` | Users the app identified, with activity and error counts |
| `detect_anomalies` | What is unusual: errors, issues, crash rate, traffic, events, latency and Web Vitals vs the same window in earlier weeks, with a next tool for each |
| `preview_digest` | The digest a digest rule would send now, with narrative and text |
| `list_flags` / `get_flag` | Feature flags; one flag with exposure per variant |
| `flag_health` | Crash-free sessions, errors and over-represented issues per flag variant |
| `list_experiments` / `experiment_results` | Experiments; conversion per variant with uplift, CI and p-value |
| `list_cohorts` / `get_cohort` | Saved cohorts with sizes; one cohort's definition and sample members |
| `analytics_stickiness` | DAU/WAU/MAU, DAU/MAU and days active per week |
| `analytics_lifecycle` | New, returning, resurrecting and dormant people per day or week |
| `get_user_timeline` | One user's events, crashes and sessions, newest first |
| `get_release_commits` | Commits recorded for a release, with changed files |
| `get_ownership_rules` | The project's code ownership rules, parsed |
| `list_feedback` | User feedback with sentiment, topics and linked issue; filters for status, sentiment, topic, release, flag, issue, user, screenshots and text ([feedback](feedback.md)) |
| `get_feedback` | One feedback item with its screenshots as images |
| `feedback_summary` | Feedback counts by sentiment, topic, release, flag variant, day, status, platform or source, with quotes |

Write:

| Tool | What it does |
| --- | --- |
| `create_project` | Create a project and its ingest key (all-project write token) |
| `create_flag` / `update_flag` / `delete_flag` | Create, roll out, kill or retarget a feature flag |
| `create_experiment` | Save a flag + goal experiment |
| `create_cohort` / `delete_cohort` | Save or remove a behavioral cohort (usable as `{"cohort": name}` in any analytics filter) |
| `resolve_issue` | Resolve, optionally `resolved_in_release`; a new event reopens it as a regression |
| `ignore_issue` | Ignore forever or for a duration (`ignore_for`: `24h`, `7d`) |
| `reopen_issue` | Set back to open |
| `assign_issue` | Assign to a user by email |
| `create_goal` | Save a named conversion goal (a step, optionally with a revenue property) |
| `delete_goal` | Delete a goal |
| `set_ownership_rules` | Replace the project's ownership rules (CODEOWNERS-like; see the REST docs) |
| `link_issue` | Record a PR, ticket or commit URL on an issue (do this right after opening a fix PR) |
| `set_code_mappings` | Replace the stack path → repository path mappings |
| `apply_ownership_rules` | Re-apply ownership rules to open issues |
| `comment_issue` | Add a Markdown comment, e.g. root cause and a link to the fix |
| `create_alert_rule` | Webhook, Slack, email, GitHub repository_dispatch or GitHub issue on new issues, regressions, spikes, thresholds, resolves, assignments, comments, new releases, or a daily or weekly digest (`schedule` with a time zone); with filters and rate limits |
| `list_alert_rules` | Rules with triggers, channels and filters |
| `update_alert_rule` / `delete_alert_rule` | Change or delete a rule |
| `test_alert_rule` | Send a test notification |
| `list_alert_deliveries` | Delivery log with status, attempts and optional payloads |
| `redeliver_alert` | Send a stored delivery again |
| `update_feedback` | Set a feedback item's status, assignee, sentiment, topics or linked issue |
| `delete_user_data` | Delete all data for a user id in a project, including feedback (cannot be undone) |

## Resources

- `vaka://issues/{issue}`: an issue with its latest event (id or short id like `WEB-12`)
- `vaka://projects/{project}/releases/{release}`: health for one release

## Example prompts

- "What are the top new crashes in the iOS app since yesterday's release? Fix the worst one."
- "Compare release 2.3.0 with 2.4.0 for the web project. Did crash-free sessions drop?"
- "Errors spiked in the last hour. Explain why."
- "Is anything unusual in the web app today? Dig into the worst finding."
- "Send me a weekly digest every Monday at 9 in Stockholm time."
- "Which referrers brought the most visitors to /pricing this month?"
- "Where do people drop off between the pricing page and purchase? Does it differ by country?"
- "Do users who sign up on the pro plan come back more in the following weeks?"
- "User user-123 reported a crash. What did they do just before it?"
- "How much revenue did each campaign bring in last month?"
- "How far has 2.4.0 rolled out? Is anyone still on 2.2?"
- "Who probably broke checkout in 2.4.0? Look at the suspect commits on the top new issue."
- "Set up Vaka for this repository" (uses `create_project` and `get_install_instructions`).

A typical bug-fix loop: `list_issues` → `get_issue` (read the stack and breadcrumbs, open the
files at the in-app frames) → fix → `resolve_issue` with `resolved_in_release` set to the next
version → `comment_issue` with the commit or PR link.
