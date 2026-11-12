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
            "/api/v1/alert-rules": {"get": op("List alert rules", &[], None), "post": op("Create alert rule", &[], Some("{project?, trigger: new_issue|regression|spike, channel: webhook|email, target, secret?, threshold?, name?}"))},
            "/api/v1/alert-rules/{id}": {"patch": op("Update alert rule", &[id], Some("{name?, target?, secret?, threshold?, enabled?}")), "delete": op("Delete alert rule", &[id], None)},
            "/api/v1/alert-rules/{id}/test": {"post": op("Send a test notification", &[id], None)},
            "/api/v1/alert-rules/{id}/deliveries": {"get": op("Recent deliveries", &[id], None)},
            "/api/v1/tokens": {"get": op("List API tokens (admin)", &[], None), "post": op("Create API token (admin)", &[], Some("{name, project?, scope: read|write, expires_in_days?}"))},
            "/api/v1/tokens/{id}": {"delete": op("Revoke API token (admin)", &[id], None)},
            "/api/v1/users": {"get": op("List users (admin)", &[], None)},
            "/api/v1/users/invite": {"post": op("Invite a user (admin)", &[], Some("{email, role: viewer|member|admin|owner}"))},
            "/api/v1/users/{id}": {"patch": op("Update user role or disable (admin)", &[id], Some("{role?, disabled?}")), "delete": op("Delete user (admin)", &[id], None)},
            "/ingest/v1/batch": {"post": {"summary": "Ingest a batch of errors, sessions and events (ingest key in X-Vaka-Key)", "security": [], "responses": {"202": {"description": "{accepted, rejected[]}"}}}},
        }
    })
}
