//! OpenAPI 3.1 description of the integration API (hand-maintained, kept small).

use serde_json::{json, Value};

fn op(summary: &str, params: &[(&str, &str, bool)], body: Option<&str>) -> Value {
    let parameters: Vec<Value> = params
        .iter()
        .map(|(name, loc, required)| json!({"name": name, "in": loc, "required": required, "schema": {"type": "string"}}))
        .collect();
    let mut v = json!({"summary": summary, "parameters": parameters, "responses": {"200": {"description": "JSON response"}, "4XX": {"description": "{\"error\": \"...\"}"}}});
    if let Some(b) = body {
        v["requestBody"] = json!({"content": {"application/json": {"schema": {"type": "object", "description": b}}}});
    }
    v
}

pub fn spec() -> Value {
    let p = ("p", "path", true);
    let id = ("id", "path", true);
    let since = ("since", "query", false);
    json!({
        "openapi": "3.1.0",
        "info": {"title": "Vaka integration API", "version": env!("CARGO_PKG_VERSION"), "description": "Query crashes, release health and analytics; triage issues. Authenticate with `Authorization: Bearer <api token>`. `p` is a project slug or id; issues accept ids or short ids like WEB-12. Times accept relative values (30m, 24h, 7d) or RFC 3339."},
        "servers": [{"url": "/"}],
        "components": {"securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}}},
        "security": [{"bearer": []}],
        "paths": {
            "/api/v1/me": {"get": op("Who am I (token or user)", &[], None)},
            "/api/v1/projects": {"get": op("List projects", &[], None), "post": op("Create project", &[], Some("{name, platform, slug?}"))},
            "/api/v1/projects/{p}": {"get": op("Get project", &[p], None), "patch": op("Update project settings (admin)", &[p], Some("{name?, retention_days?, scrub_enabled?, scrub_fields?}"))},
            "/api/v1/projects/{p}/keys": {"post": op("Create ingest key (admin)", &[p], None)},
            "/api/v1/projects/{p}/issues": {"get": op("List issues", &[p, ("status", "query", false), ("query", "query", false), ("release", "query", false), ("environment", "query", false), since, ("sort", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/events": {"get": op("Search error events", &[p, ("query", "query", false), ("level", "query", false), ("release", "query", false), ("environment", "query", false), ("user_id", "query", false), ("exception_type", "query", false), since, ("until", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/events/{event_id}": {"get": op("Get one error event", &[p, ("event_id", "path", true)], None)},
            "/api/v1/projects/{p}/releases": {"get": op("List releases", &[p], None)},
            "/api/v1/projects/{p}/releases/health": {"get": op("Release health (crash-free sessions/users)", &[p, ("release", "query", false), since], None)},
            "/api/v1/projects/{p}/releases/compare": {"get": op("Compare two releases", &[p, ("base", "query", true), ("target", "query", true)], None)},
            "/api/v1/projects/{p}/releases/{version}/commits": {"get": op("Commits recorded for a release", &[p, ("version", "path", true)], None), "put": op("Replace a release's commits (for suspect commits)", &[p, ("version", "path", true)], Some("{repository?: url, commits: [{sha, message?, author_name?, author_email?, timestamp?, files?: [path]}]}"))},
            "/api/v1/projects/{p}/ownership": {"get": op("Code ownership rules", &[p], None), "put": op("Replace code ownership rules", &[p], Some("{rules: string}"))},
            "/api/v1/projects/{p}/ownership/apply": {"post": op("Re-apply ownership rules to open issues (all=true: also issues that already have owners)", &[p, ("all", "query", false)], None)},
            "/api/v1/projects/{p}/code-mappings": {"get": op("Code mappings (stack path prefix -> repository path)", &[p], None), "put": op("Replace code mappings", &[p], Some("{mappings: [{stack_root, source_root, repository?, default_branch?}]}"))},
            "/api/v1/issues/{id}/fix-context": {"get": op("Everything needed to fix an issue: in-app frames with source links, breadcrumbs, suspect commits, owners, links, suggested branch and commit trailer", &[id], None)},
            "/api/v1/issues/{id}/precursors": {"get": op("What users most often did in the minutes before this issue, with lift over all sessions, plus common breadcrumbs", &[id, ("minutes", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/sessions/{session_id}/timeline": {"get": op("A session's pageviews, screen views, events, breadcrumbs and errors in order", &[p, ("session_id", "path", true), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/performance/summary": {"get": op("Per-transaction p50/p75/p95/p99, throughput and error rate", &[p, ("since", "query", false), ("until", "query", false), ("environment", "query", false), ("release", "query", false), ("op", "query", false), ("sort", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/performance/endpoints": {"get": op("Slowest HTTP server endpoints by p95", &[p, ("since", "query", false), ("until", "query", false), ("environment", "query", false), ("release", "query", false), ("min_count", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/performance/app-start": {"get": op("Cold and warm app start durations by release", &[p, ("since", "query", false), ("until", "query", false), ("environment", "query", false), ("release", "query", false)], None)},
            "/api/v1/projects/{p}/performance/web-vitals": {"get": op("Web Vitals (LCP, INP, CLS, FCP, TTFB) p75 with good/poor shares, overall and by page", &[p, ("since", "query", false), ("until", "query", false), ("environment", "query", false), ("release", "query", false), ("transaction", "query", false), ("by_page", "query", false)], None)},
            "/api/v1/projects/{p}/performance/traces": {"get": op("Slowest traces (root spans)", &[p, ("since", "query", false), ("until", "query", false), ("environment", "query", false), ("release", "query", false), ("op", "query", false), ("transaction", "query", false), ("min_ms", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/performance/breakdown": {"get": op("Where a transaction's time goes, by child span op and name", &[p, ("since", "query", false), ("until", "query", false), ("environment", "query", false), ("release", "query", false), ("transaction", "query", true), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/performance/compare": {"get": op("p95 and Web Vitals regressions between two releases", &[p, ("base", "query", true), ("target", "query", true), ("min_count", "query", false), ("threshold", "query", false)], None)},
            "/api/v1/projects/{p}/traces/{trace_id}": {"get": op("One trace as a span tree, with errors and logs that carry its trace id", &[p, ("trace_id", "path", true)], None)},
            "/api/v1/projects/{p}/flags": {"get": op("Feature flags", &[p], None), "post": op("Create a feature flag", &[p], Some("{key, description?, enabled?, rollout?: 0-100, variants?: [{key, weight}], rules?: [{user_ids?, properties?, variant?, rollout?}], environments?: {env: bool}}"))},
            "/api/v1/projects/{p}/flags/{key}": {"get": op("One flag with exposure in the last 7 days", &[p, ("key", "path", true)], None), "patch": op("Update a flag (only given fields)", &[p, ("key", "path", true)], Some("{description?, enabled?, rollout?, variants?, rules?, environments?}")), "delete": op("Delete a flag", &[p, ("key", "path", true)], None)},
            "/api/v1/projects/{p}/flags/{key}/health": {"get": op("Crash-free sessions, errors and over-represented issues per variant", &[p, ("key", "path", true), ("since", "query", false), ("release", "query", false)], None)},
            "/api/v1/projects/{p}/flags/{key}/results": {"get": op("Conversion per variant for a goal, with uplift and significance", &[p, ("key", "path", true), ("goal", "query", true), ("control", "query", false), ("since", "query", false), ("identity", "query", false)], None)},
            "/api/v1/projects/{p}/experiments": {"get": op("Saved experiments", &[p], None), "post": op("Create an experiment", &[p], Some("{name, flag, goal, control?, description?}"))},
            "/api/v1/projects/{p}/experiments/{name}/results": {"get": op("Experiment results per variant", &[p, ("name", "path", true), ("control", "query", false), ("since", "query", false), ("identity", "query", false)], None)},
            "/api/v1/projects/{p}/logs": {"get": op("Search OTLP logs, newest first, with counts per severity", &[p, ("q", "query", false), ("severity", "query", false), ("trace_id", "query", false), ("service", "query", false), ("release", "query", false), ("environment", "query", false), ("since", "query", false), ("until", "query", false), ("limit", "query", false)], None)},
            "/ingest/otlp/v1/traces": {"post": op("OTLP/HTTP traces (protobuf or JSON; ingest key in X-Vaka-Key or Authorization: Bearer)", &[], None)},
            "/ingest/otlp/v1/logs": {"post": op("OTLP/HTTP logs (protobuf or JSON)", &[], None)},
            "/api/v1/projects/{p}/anomalies": {"get": op("Anomalies in the last window vs the same window 1 to 4 weeks earlier, with sentences and next steps", &[p, ("window", "query", false)], None)},
            "/api/v1/projects/{p}/digest/preview": {"get": op("The digest a digest rule would send now (anomalies, narrative, text), without sending", &[p, ("frequency", "query", false), ("narrative", "query", false)], None)},
            "/ingest/v1/flags": {"post": op("Evaluate all flags for an identity (ingest key)", &[], Some("{distinct_id, user_id?, properties?, environment?}"))},
            "/api/v1/projects/{p}/cohorts": {"get": op("Saved cohorts with definitions and sizes", &[p, ("sizes", "query", false)], None), "post": op("Create a cohort", &[p], Some("{name, description?, identity?: auto|user|visitor, within?: 30d, criteria?: [{event|step, filters?, min_count?, within?, negate?}], properties?: {dimension: value}}"))},
            "/api/v1/projects/{p}/cohorts/{cohort}": {"get": op("Cohort definition, size and sample members", &[p, ("cohort", "path", true), ("limit", "query", false)], None), "delete": op("Delete a cohort", &[p, ("cohort", "path", true)], None)},
            "/api/v1/projects/{p}/analytics/stickiness": {"get": op("DAU/WAU/MAU, DAU/MAU and days active per week", &[p, ("since", "query", false), ("until", "query", false), ("event", "query", false), ("identity", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/lifecycle": {"get": op("New, returning, resurrecting and dormant people per period", &[p, ("since", "query", false), ("until", "query", false), ("period", "query", false), ("event", "query", false), ("identity", "query", false)], None)},
            "/api/v1/issues/{id}/links": {"get": op("Links (pull requests, tickets, commits) on an issue", &[id], None), "post": op("Link a pull request, ticket or commit to an issue", &[id], Some("{url, kind?: pull_request|issue|commit|other, title?}"))},
            "/api/v1/issues/{id}/links/{link}": {"delete": op("Remove a link", &[id, ("link", "path", true)], None)},
            "/api/v1/projects/{p}/analytics/summary": {"get": op("Analytics totals with previous period. Repeat `filter=dimension:value`.", &[p, since, ("until", "query", false), ("type", "query", false), ("filter", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/timeseries": {"get": op("Analytics time series", &[p, since, ("until", "query", false), ("interval", "query", false), ("type", "query", false), ("filter", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/breakdown": {"get": op("Top values of a dimension", &[p, ("dimension", "query", true), since, ("until", "query", false), ("limit", "query", false), ("type", "query", false), ("filter", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/funnel": {"get": op("Funnel conversion through ordered steps (repeat `step=event|dimension:value`)", &[p, ("step", "query", true), ("window", "query", false), ("breakdown", "query", false), ("identity", "query", false), since, ("until", "query", false), ("filter", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/retention": {"get": op("Cohort retention by day or week", &[p, ("start", "query", false), ("return", "query", false), ("period", "query", false), ("periods", "query", false), ("identity", "query", false), since, ("until", "query", false), ("filter", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/paths": {"get": op("Common paths after or before a page, screen or event", &[p, ("from", "query", false), ("direction", "query", false), ("depth", "query", false), ("identity", "query", false), since, ("until", "query", false), ("filter", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/events": {"get": op("Event catalog: names, counts and property keys", &[p, since, ("until", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/revenue": {"get": op("Revenue per currency from a numeric event property", &[p, ("event", "query", false), ("value_prop", "query", false), ("currency_prop", "query", false), ("breakdown", "query", false), since, ("until", "query", false), ("filter", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/analytics/realtime": {"get": op("Traffic in the last few minutes", &[p, ("minutes", "query", false)], None)},
            "/api/v1/projects/{p}/goals": {"get": op("Goals with conversions and revenue", &[p, since, ("until", "query", false), ("filter", "query", false)], None), "post": op("Create a goal", &[p], Some("{name, step: 'event|dimension:value', value_prop?}"))},
            "/api/v1/projects/{p}/goals/{goal}": {"delete": op("Delete a goal (id or name)", &[p, ("goal", "path", true)], None)},
            "/api/v1/projects/{p}/releases/adoption": {"get": op("Share of sessions and users per release over time", &[p, since, ("interval", "query", false), ("environment", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/users": {"get": op("Identified users with activity and error counts", &[p, since, ("query", "query", false), ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/users/{user_id}/timeline": {"get": op("Everything one identified user did: events, errors, sessions", &[p, ("user_id", "path", true), since, ("limit", "query", false)], None)},
            "/api/v1/projects/{p}/explain-spike": {"get": op("Explain an error spike", &[p, ("window", "query", false), ("baseline", "query", false)], None)},
            "/api/v1/projects/{p}/artifacts": {"get": op("List debug files", &[p, ("release", "query", false)], None), "post": {"summary": "Upload a debug file (multipart: kind, release, name, file)", "requestBody": {"content": {"multipart/form-data": {"schema": {"type": "object", "properties": {"kind": {"type": "string", "enum": ["sourcemap", "proguard", "native"]}, "release": {"type": "string"}, "name": {"type": "string"}, "file": {"type": "string", "format": "binary"}}}}}}, "responses": {"201": {"description": "Uploaded"}}}},
            "/api/v1/projects/{p}/artifacts/{id}": {"delete": op("Delete a debug file", &[p, id], None)},
            "/api/v1/projects/{p}/users/{user_id}": {"delete": op("GDPR: delete all data for a user id", &[p, ("user_id", "path", true)], None)},
            "/api/v1/projects/{p}/install/{platform}": {"get": op("SDK install instructions (Markdown) with key filled in", &[p, ("platform", "path", true)], None)},
            "/api/v1/issues/{id}": {"get": op("Get issue with latest event and stats", &[id], None), "patch": op("Update issue", &[id], Some("{status?: open|resolved|ignored, resolved_in_release?, ignore_for?, assignee?}"))},
            "/api/v1/issues/{id}/events": {"get": op("List events of an issue", &[id, ("limit", "query", false)], None)},
            "/api/v1/issues/{id}/comments": {"post": op("Comment on an issue", &[id], Some("{body}"))},
            "/api/v1/alert-rules": {"get": op("List alert rules", &[], None), "post": op("Create alert rule", &[], Some("{project?, trigger: new_issue|regression|spike|threshold|issue_resolved|issue_assigned|issue_commented|release_created|perf_regression|digest, channel: webhook|slack|email|github_dispatch|github_issue, target, secret?, threshold?, filters?, max_per_hour?, schedule?: {frequency: daily|weekly, hour, weekday?, timezone?}, name?}"))},
            "/api/v1/alert-rules/{id}": {"patch": op("Update alert rule", &[id], Some("{name?, target?, secret?, threshold?, enabled?, filters?, max_per_hour?, schedule?}")), "delete": op("Delete alert rule", &[id], None)},
            "/api/v1/alert-rules/{id}/test": {"post": op("Send a test notification", &[id], None)},
            "/api/v1/alert-rules/{id}/deliveries": {"get": op("Recent deliveries (status, attempts, next retry)", &[id, ("payload", "query", false), ("limit", "query", false)], None)},
            "/api/v1/alert-rules/{id}/deliveries/{delivery}/redeliver": {"post": op("Send a stored delivery again", &[id, ("delivery", "path", true)], None)},
            "/api/v1/tokens": {"get": op("List API tokens (admin)", &[], None), "post": op("Create API token (admin)", &[], Some("{name, project?, scope: read|write, expires_in_days?}"))},
            "/api/v1/tokens/{id}": {"delete": op("Revoke API token (admin)", &[id], None)},
            "/api/v1/users": {"get": op("List users (admin)", &[], None)},
            "/api/v1/users/invite": {"post": op("Invite a user (admin)", &[], Some("{email, role: viewer|member|admin|owner}"))},
            "/api/v1/users/{id}": {"patch": op("Update user role or disable (admin)", &[id], Some("{role?, disabled?}")), "delete": op("Delete user (admin)", &[id], None)},
            "/ingest/v1/batch": {"post": {"summary": "Ingest a batch of errors, sessions and events (ingest key in X-Vaka-Key)", "security": [], "responses": {"202": {"description": "{accepted, rejected[]}"}}}},
        }
    })
}
