//! MCP server (Streamable HTTP transport, stateless JSON responses) at `/mcp`.
//!
//! Authenticate with `Authorization: Bearer <api token>`. Every tool is a thin
//! wrapper over the shared query layer, so MCP and REST return the same data.

use crate::auth::{ApiError, Principal};
use crate::query::{self, AnalyticsParams, EventSearchParams, IssueListParams, IssueUpdate};
use crate::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Map, Value};

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/mcp",
        post(handle).get(|| async { (StatusCode::METHOD_NOT_ALLOWED, "Vaka's MCP endpoint is stateless: POST JSON-RPC messages here.") }),
    )
}

const INSTRUCTIONS: &str = "Vaka is a crash-reporting and analytics service. Typical workflow: \
list_projects -> list_issues (sort=events, since=24h) -> get_issue (full symbolicated stack trace, breadcrumbs, \
affected releases/devices) -> fix the code -> resolve_issue with resolved_in_release. For 'why did X spike' use explain_spike; \
for release quality use release_health and compare_releases; for traffic use analytics_summary, analytics_timeseries and analytics_breakdown. \
To instrument a new app: create_project then get_install_instructions. Issues can be referenced by short id like WEB-12.";

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

async fn handle(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string());
    let principal = match token {
        Some(t) => crate::auth::principal_from_token(&state, &t).await.ok().flatten(),
        None => None,
    };
    let Some(principal) = principal else {
        return (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Bearer realm=\"vaka\"")],
            Json(json!({"error": "send `Authorization: Bearer <vaka api token>`; create tokens in the Vaka admin UI"})),
        )
            .into_response();
    };
    match body {
        Value::Array(msgs) => {
            let mut out = Vec::new();
            for m in msgs {
                if let Some(r) = dispatch(&state, &principal, m).await {
                    out.push(r);
                }
            }
            if out.is_empty() { StatusCode::ACCEPTED.into_response() } else { Json(Value::Array(out)).into_response() }
        }
        m => match dispatch(&state, &principal, m).await {
            Some(r) => Json(r).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        },
    }
}

async fn dispatch(state: &AppState, p: &Principal, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    // Notifications (no id) get no response.
    let id = id?;
    Some(match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL_VERSIONS[0]);
            let version = if PROTOCOL_VERSIONS.contains(&requested) { requested } else { PROTOCOL_VERSIONS[0] };
            rpc_result(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}, "resources": {"listChanged": false}},
                    "serverInfo": {"name": "vaka", "title": "Vaka", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => rpc_result(&id, json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(state, p, &name, args).await {
                Ok(v) => rpc_result(
                    &id,
                    json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}], "structuredContent": v, "isError": false}),
                ),
                Err(ToolError::Unknown) => rpc_error(&id, -32602, &format!("unknown tool `{name}`")),
                Err(ToolError::Failed(msg)) => rpc_result(&id, json!({"content": [{"type": "text", "text": msg}], "isError": true})),
            }
        }
        "resources/list" => rpc_result(&id, json!({"resources": []})),
        "resources/templates/list" => rpc_result(
            &id,
            json!({"resourceTemplates": [
                {"uriTemplate": "vaka://issues/{issue}", "name": "Issue", "description": "An issue with its latest event and stats (id or short id like WEB-12)", "mimeType": "application/json"},
                {"uriTemplate": "vaka://projects/{project}/releases/{release}", "name": "Release health", "description": "Crash-free rates and errors for one release", "mimeType": "application/json"}
            ]}),
        ),
        "resources/read" => {
            let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            match read_resource(state, p, uri).await {
                Ok(v) => rpc_result(&id, json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}]})),
                Err(e) => rpc_error(&id, -32002, &e),
            }
        }
        "prompts/list" => rpc_result(&id, json!({"prompts": []})),
        _ => rpc_error(&id, -32601, &format!("method `{method}` not supported")),
    })
}

async fn read_resource(state: &AppState, p: &Principal, uri: &str) -> Result<Value, String> {
    let rest = uri.strip_prefix("vaka://").ok_or("unsupported URI scheme")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let r = match parts.as_slice() {
        ["issues", id] => query::get_issue(state, p, id).await,
        ["projects", proj, "releases", rel] => query::release_health(state, p, proj, Some(rel), Some("90d")).await,
        _ => return Err(format!("unknown resource {uri}")),
    };
    r.map_err(|e| e.1)
}

enum ToolError {
    Unknown,
    Failed(String),
}

impl From<ApiError> for ToolError {
    fn from(e: ApiError) -> Self {
        ToolError::Failed(e.1)
    }
}

fn s(args: &Value, k: &str) -> Option<String> {
    args.get(k).and_then(|v| match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn req(args: &Value, k: &str) -> Result<String, ToolError> {
    s(args, k).ok_or_else(|| ToolError::Failed(format!("missing required argument `{k}`")))
}

fn i(args: &Value, k: &str) -> Option<i64> {
    args.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

fn analytics_args(args: &Value) -> Result<AnalyticsParams, ToolError> {
    let filters = args
        .get("filters")
        .and_then(|f| f.as_object())
        .map(|o| o.iter().map(|(k, v)| (k.clone(), crate::models::value_to_string(v))).collect())
        .unwrap_or_default();
    Ok(AnalyticsParams { project: req(args, "project")?, since: s(args, "since"), until: s(args, "until"), filters, r#type: s(args, "type") })
}

/// A funnel/retention step: `"signup"`, `"pageview|path:/pricing"`, or `{"event": ..., "filters": {...}}`.
fn step_value(v: &Value) -> Result<crate::product::Step, ToolError> {
    match v {
        Value::String(s) => Ok(crate::product::Step::parse(s)?),
        Value::Object(o) => {
            let event = o.get("event").and_then(|e| e.as_str()).unwrap_or("").to_string();
            if event.is_empty() {
                return Err(ToolError::Failed("each step needs an `event`".into()));
            }
            let filters = o
                .get("filters")
                .and_then(|f| f.as_object())
                .map(|f| f.iter().map(|(k, v)| (k.clone(), crate::models::value_to_string(v))).collect())
                .unwrap_or_default();
            Ok(crate::product::Step { event, filters })
        }
        _ => Err(ToolError::Failed("a step is a string like \"signup\" or an object {\"event\", \"filters\"}".into())),
    }
}

fn opt_step_arg(a: &Value, k: &str) -> Result<Option<crate::product::Step>, ToolError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(v) => step_value(v).map(Some),
    }
}

async fn call_tool(state: &AppState, p: &Principal, name: &str, a: Value) -> Result<Value, ToolError> {
    Ok(match name {
        "list_projects" => query::list_projects(state, p).await?,
        "create_project" => {
            crate::api::create_project_inner(state, p, crate::api::CreateProject { name: req(&a, "name")?, platform: req(&a, "platform")?, slug: s(&a, "slug") }).await?
        }
        "get_install_instructions" => {
            let proj = query::project_for(state, p, &req(&a, "project")?).await?;
            let platform = s(&a, "platform").unwrap_or_else(|| proj.platform.clone());
            let key = crate::projects::ingest_keys(state, &proj.id).await.map_err(ApiError::from)?.into_iter().next().unwrap_or_default();
            let md = crate::docs::install_markdown(&platform, &state.config.public_url, &key, &proj.slug)
                .ok_or_else(|| ToolError::Failed(format!("no instructions for `{platform}`; available: {}", crate::docs::INSTALL_PLATFORMS.join(", "))))?;
            json!({"project": proj.slug, "platform": platform, "endpoint": state.config.public_url, "ingest_key": key, "instructions_markdown": md})
        }
        "list_issues" => {
            query::list_issues(
                state,
                p,
                IssueListParams {
                    project: req(&a, "project")?,
                    status: s(&a, "status"),
                    query: s(&a, "query"),
                    release: s(&a, "release"),
                    environment: s(&a, "environment"),
                    since: s(&a, "since"),
                    sort: s(&a, "sort"),
                    limit: i(&a, "limit"),
                },
            )
            .await?
        }
        "get_issue" => query::get_issue(state, p, &req(&a, "issue")?).await?,
        "list_issue_events" => query::list_issue_events(state, p, &req(&a, "issue")?, i(&a, "limit")).await?,
        "get_event" => query::get_event(state, p, &req(&a, "project")?, &req(&a, "event_id")?).await?,
        "search_events" => {
            query::search_events(
                state,
                p,
                EventSearchParams {
                    project: req(&a, "project")?,
                    query: s(&a, "query"),
                    level: s(&a, "level"),
                    release: s(&a, "release"),
                    environment: s(&a, "environment"),
                    user_id: s(&a, "user_id"),
                    exception_type: s(&a, "exception_type"),
                    since: s(&a, "since"),
                    until: s(&a, "until"),
                    limit: i(&a, "limit"),
                },
            )
            .await?
        }
        "release_health" => query::release_health(state, p, &req(&a, "project")?, s(&a, "release").as_deref(), s(&a, "since").as_deref()).await?,
        "compare_releases" => query::compare_releases(state, p, &req(&a, "project")?, &req(&a, "base")?, &req(&a, "target")?).await?,
        "analytics_summary" => query::analytics_summary(state, p, analytics_args(&a)?).await?,
        "analytics_timeseries" => query::analytics_timeseries(state, p, analytics_args(&a)?, s(&a, "interval").as_deref()).await?,
        "analytics_breakdown" => query::analytics_breakdown(state, p, analytics_args(&a)?, &req(&a, "dimension")?, i(&a, "limit")).await?,
        "top_pages" => query::analytics_breakdown(state, p, analytics_args(&a)?, "path", i(&a, "limit")).await?,
        "analytics_funnel" => {
            let steps = a
                .get("steps")
                .and_then(|v| v.as_array())
                .ok_or_else(|| ToolError::Failed("`steps` must be an array of 2 to 10 steps".into()))?
                .iter()
                .map(step_value)
                .collect::<Result<Vec<_>, _>>()?;
            crate::product::funnel(
                state,
                p,
                crate::product::FunnelParams { base: analytics_args(&a)?, steps, window: s(&a, "window"), identity: s(&a, "identity"), breakdown: s(&a, "breakdown"), limit: i(&a, "limit") },
            )
            .await?
        }
        "analytics_retention" => {
            crate::product::retention(
                state,
                p,
                crate::product::RetentionParams {
                    base: analytics_args(&a)?,
                    start: opt_step_arg(&a, "start")?,
                    return_step: opt_step_arg(&a, "return")?,
                    period: s(&a, "period"),
                    periods: i(&a, "periods"),
                    identity: s(&a, "identity"),
                },
            )
            .await?
        }
        "analytics_paths" => {
            crate::product::paths(
                state,
                p,
                crate::product::PathParams { base: analytics_args(&a)?, from: s(&a, "from"), direction: s(&a, "direction"), depth: i(&a, "depth"), identity: s(&a, "identity"), limit: i(&a, "limit") },
            )
            .await?
        }
        "list_event_names" => crate::product::event_catalog(state, p, analytics_args(&a)?, i(&a, "limit")).await?,
        "list_identified_users" => {
            crate::product::list_users(state, p, crate::product::UserListParams { project: req(&a, "project")?, since: s(&a, "since"), query: s(&a, "query"), limit: i(&a, "limit") }).await?
        }
        "get_user_timeline" => crate::product::user_timeline(state, p, &req(&a, "project")?, &req(&a, "user_id")?, s(&a, "since").as_deref(), i(&a, "limit")).await?,
        "explain_spike" => query::explain_spike(state, p, &req(&a, "project")?, s(&a, "window").as_deref(), s(&a, "baseline").as_deref()).await?,
        "resolve_issue" => {
            query::update_issue(state, p, &req(&a, "issue")?, IssueUpdate { status: Some("resolved".into()), resolved_in_release: s(&a, "resolved_in_release"), ..Default::default() }).await?
        }
        "ignore_issue" => {
            query::update_issue(state, p, &req(&a, "issue")?, IssueUpdate { status: Some("ignored".into()), ignore_for: s(&a, "ignore_for"), ..Default::default() }).await?
        }
        "reopen_issue" => query::update_issue(state, p, &req(&a, "issue")?, IssueUpdate { status: Some("open".into()), ..Default::default() }).await?,
        "assign_issue" => {
            query::update_issue(state, p, &req(&a, "issue")?, IssueUpdate { assignee: Some(s(&a, "assignee").unwrap_or_default()), ..Default::default() }).await?
        }
        "comment_issue" => query::comment_issue(state, p, &req(&a, "issue")?, &req(&a, "body")?).await?,
        "get_ownership_rules" => crate::ownership::get_ownership(state, p, &req(&a, "project")?).await?,
        "set_ownership_rules" => crate::ownership::set_ownership(state, p, &req(&a, "project")?, &s(&a, "rules").unwrap_or_default()).await?,
        "get_release_commits" => crate::ownership::get_release_commits(state, p, &req(&a, "project")?, &req(&a, "release")?).await?,
        "create_alert_rule" => {
            crate::api::create_rule_inner(
                state,
                p,
                crate::api::RuleBody {
                    project: s(&a, "project"),
                    name: s(&a, "name"),
                    trigger: s(&a, "trigger"),
                    channel: s(&a, "channel"),
                    target: s(&a, "target"),
                    secret: s(&a, "secret"),
                    threshold: i(&a, "threshold"),
                    enabled: Some(true),
                },
            )
            .await?
        }
        "delete_user_data" => {
            let proj = query::project_for(state, p, &req(&a, "project")?).await?;
            if !p.can_write(&proj.id) {
                return Err(ApiError::forbidden().into());
            }
            json!(crate::maintenance::erase_user(state, &proj.id, &req(&a, "user_id")?, &p.label()).await.map_err(ApiError::from)?)
        }
        _ => return Err(ToolError::Unknown),
    })
}

fn prop(ty: &str, desc: &str) -> Value {
    json!({"type": ty, "description": desc})
}

fn tool(name: &str, title: &str, desc: &str, props: Vec<(&str, Value)>, required: &[&str], read_only: bool, destructive: bool) -> Value {
    let mut m = Map::new();
    for (k, v) in props {
        m.insert(k.to_string(), v);
    }
    json!({
        "name": name,
        "title": title,
        "description": desc,
        "inputSchema": {"type": "object", "properties": m, "required": required},
        "annotations": {"title": title, "readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": read_only, "openWorldHint": false},
    })
}

pub fn tools() -> Vec<Value> {
    let project = || prop("string", "Project slug or id (see list_projects)");
    let issue = || prop("string", "Issue id or short id such as WEB-12");
    let since = || prop("string", "Start of the window: relative (30m, 24h, 7d) or RFC 3339");
    let until = || prop("string", "End of the window (default now)");
    let filters = || {
        json!({"type": "object", "additionalProperties": {"type": "string"},
               "description": format!("Filter by dimension, e.g. {{\"path\": \"/pricing\", \"country\": \"SE\"}}. Prefix a value with ~ for contains or ! for not-equal. Dimensions: {}", query::DIMENSIONS.join(", "))})
    };
    vec![
        tool("list_projects", "List projects", "List the projects this token can access, with platform, retention and ingest keys.", vec![], &[], true, false),
        tool(
            "create_project",
            "Create project",
            "Create a project and its ingest key. Requires a write token not scoped to one project. Follow up with get_install_instructions.",
            vec![("name", prop("string", "Human-readable name")), ("platform", prop("string", &format!("One of {}", crate::api::PLATFORMS.join(", ")))), ("slug", prop("string", "Optional URL slug"))],
            &["name", "platform"],
            false,
            false,
        ),
        tool(
            "get_install_instructions",
            "Get SDK install instructions",
            "Step-by-step instructions (Markdown) to add the Vaka SDK to an app, with this project's endpoint and ingest key filled in, including how to upload debug files and verify the setup.",
            vec![("project", project()), ("platform", prop("string", &format!("One of {} (defaults to the project's platform)", crate::docs::INSTALL_PLATFORMS.join(", "))))],
            &["project"],
            true,
            false,
        ),
        tool(
            "list_issues",
            "List issues",
            "List error issues (grouped crashes) in a project with event and user counts for a time window.",
            vec![
                ("project", project()),
                ("status", prop("string", "open (default), resolved, ignored or all")),
                ("query", prop("string", "Text to search in title and culprit")),
                ("release", prop("string", "Only issues seen in this release")),
                ("environment", prop("string", "Only issues seen in this environment")),
                ("since", since()),
                ("sort", prop("string", "last_seen (default), first_seen, times_seen, events (in window) or users (in window)")),
                ("limit", prop("integer", "Max issues (default 25, max 200)")),
            ],
            &["project"],
            true,
            false,
        ),
        tool(
            "get_issue",
            "Get issue",
            "Full detail for an issue: latest event with symbolicated stack trace (frames[0] is the crash site), source context, breadcrumbs, tags, and breakdowns by release, environment, OS and device, plus comments, owners (from ownership rules) and suspect_commits (commits in the issue's first release that touched files in its stack trace).",
            vec![("issue", issue())],
            &["issue"],
            true,
            false,
        ),
        tool("list_issue_events", "List issue events", "Recent individual events for an issue.", vec![("issue", issue()), ("limit", prop("integer", "Max events (default 20)"))], &["issue"], true, false),
        tool("get_event", "Get event", "One error event with full stack trace and breadcrumbs.", vec![("project", project()), ("event_id", prop("string", "Event UUID"))], &["project", "event_id"], true, false),
        tool(
            "search_events",
            "Search error events",
            "Search individual error events by text, level, release, environment, user or exception type.",
            vec![
                ("project", project()),
                ("query", prop("string", "Text in exception type, value, message or culprit")),
                ("level", prop("string", "fatal, error, warning, info")),
                ("release", prop("string", "Release version")),
                ("environment", prop("string", "Environment, e.g. production")),
                ("user_id", prop("string", "Identified user id")),
                ("exception_type", prop("string", "Exact exception type")),
                ("since", since()),
                ("until", until()),
                ("limit", prop("integer", "Max events (default 50)")),
            ],
            &["project"],
            true,
            false,
        ),
        tool(
            "release_health",
            "Release health",
            "Crash-free sessions and users, session counts, errors and new issues per release.",
            vec![("project", project()), ("release", prop("string", "Only this release")), ("since", since())],
            &["project"],
            true,
            false,
        ),
        tool(
            "compare_releases",
            "Compare releases",
            "Compare two releases: health side by side, issues new in the target, issues gone, and issues that increased.",
            vec![("project", project()), ("base", prop("string", "Older release")), ("target", prop("string", "Newer release"))],
            &["project", "base", "target"],
            true,
            false,
        ),
        tool(
            "explain_spike",
            "Explain error spike",
            "Explain why errors increased: compares a recent window with a baseline and ranks the issues, releases, OS versions, devices, countries and exception types that grew the most.",
            vec![("project", project()), ("window", prop("string", "Recent window (default 1h)")), ("baseline", prop("string", "Baseline start (default 24h before the window)"))],
            &["project"],
            true,
            false,
        ),
        tool(
            "analytics_summary",
            "Analytics summary",
            "Visitors, pageviews, screen views, custom events and identified users for a window, with the previous period for comparison.",
            vec![("project", project()), ("since", since()), ("until", until()), ("type", prop("string", "pageview, screen_view or custom")), ("filters", filters())],
            &["project"],
            true,
            false,
        ),
        tool(
            "analytics_timeseries",
            "Analytics time series",
            "Analytics metrics bucketed by hour, day, week or month.",
            vec![("project", project()), ("since", since()), ("until", until()), ("interval", prop("string", "hour, day, week or month (auto by default)")), ("type", prop("string", "pageview, screen_view or custom")), ("filters", filters())],
            &["project"],
            true,
            false,
        ),
        tool(
            "analytics_breakdown",
            "Analytics breakdown",
            "Top values of a dimension (pages, referrers, countries, browsers, events, custom props via prop:<name>) ranked by visitors.",
            vec![
                ("project", project()),
                ("dimension", prop("string", &format!("One of {}", query::DIMENSIONS.join(", ")))),
                ("since", since()),
                ("until", until()),
                ("type", prop("string", "pageview, screen_view or custom")),
                ("filters", filters()),
                ("limit", prop("integer", "Max rows (default 10)")),
            ],
            &["project", "dimension"],
            true,
            false,
        ),
        tool(
            "list_event_names",
            "List event names",
            "The event catalog: every event name (pageview, screen_view, custom events) with counts, visitors and the property keys it carries. Call this before building funnels or retention.",
            vec![("project", project()), ("since", since()), ("until", until()), ("limit", prop("integer", "Max events (default 100)"))],
            &["project"],
            true,
            false,
        ),
        tool(
            "analytics_funnel",
            "Funnel",
            "Conversion through ordered steps within a time window: people per step, conversion and drop-off, optionally broken down by a dimension.",
            vec![
                ("project", project()),
                ("steps", json!({"type": "array", "minItems": 2, "maxItems": 10, "description": "Ordered steps. Each is an event name (\"signup\"), a string \"pageview|path:/pricing\", or {\"event\": \"pageview\", \"filters\": {\"path\": \"/pricing\"}}. Event names come from list_event_names.", "items": {"anyOf": [{"type": "string"}, {"type": "object", "properties": {"event": {"type": "string"}, "filters": {"type": "object", "additionalProperties": {"type": "string"}}}, "required": ["event"]}]}})),
                ("window", prop("string", "Max time from first to last step: 30m, 1h, 1d (default), 7d")),
                ("breakdown", prop("string", "Optional dimension to split by, e.g. country, utm_source, os, prop:plan")),
                ("identity", prop("string", "visitor (default), user (identified only) or auto")),
                ("since", since()),
                ("until", until()),
                ("filters", filters()),
                ("limit", prop("integer", "Max breakdown rows (default 10)")),
            ],
            &["project", "steps"],
            true,
            false,
        ),
        tool(
            "analytics_retention",
            "Retention",
            "Cohort retention: people grouped by the day or week they first did the start event, and the share who did the return event in each later period.",
            vec![
                ("project", project()),
                ("start", json!({"description": "Event that puts someone in a cohort (default: any event). Same format as a funnel step.", "anyOf": [{"type": "string"}, {"type": "object"}]})),
                ("return", json!({"description": "Event that counts as coming back (default: any event). Same format as a funnel step.", "anyOf": [{"type": "string"}, {"type": "object"}]})),
                ("period", prop("string", "day or week (default)")),
                ("periods", prop("integer", "Periods to report after the cohort period (default 8 weeks or 14 days, max 30)")),
                ("identity", prop("string", "auto (default: user id when identified, else visitor), user or visitor")),
                ("since", prop("string", "Earliest cohort start (default 8 weeks or 14 days ago)")),
                ("until", until()),
                ("filters", filters()),
            ],
            &["project"],
            true,
            false,
        ),
        tool(
            "analytics_paths",
            "Paths",
            "Most common sequences of pages, screens and events after (or before) a starting point, within one visit. Labels look like `pageview /pricing`, `screen Checkout` or a custom event name.",
            vec![
                ("project", project()),
                ("from", prop("string", "Starting label, e.g. \"pageview /pricing\" or \"signup\"; omit for paths from the first event of each visit")),
                ("direction", prop("string", "after (default) or before")),
                ("depth", prop("integer", "Steps to follow (default 3, max 6)")),
                ("identity", prop("string", "visitor (default), user or auto")),
                ("since", since()),
                ("until", until()),
                ("filters", filters()),
                ("limit", prop("integer", "Max paths (default 20)")),
            ],
            &["project"],
            true,
            false,
        ),
        tool(
            "list_identified_users",
            "List identified users",
            "Users the app identified with identify(userId), with event and error counts, first and last seen, last release and platform.",
            vec![("project", project()), ("since", since()), ("query", prop("string", "Substring of the user id")), ("limit", prop("integer", "Max users (default 50)"))],
            &["project"],
            true,
            false,
        ),
        tool(
            "get_user_timeline",
            "User timeline",
            "Everything one identified user did, newest first: events, errors and crashes (with issue short ids) and sessions. Use it to see what led up to a crash.",
            vec![("project", project()), ("user_id", prop("string", "The id the app passed to identify()")), ("since", prop("string", "Start of the window (default 7d)")), ("limit", prop("integer", "Max entries (default 200)"))],
            &["project", "user_id"],
            true,
            false,
        ),
        tool("top_pages", "Top pages", "Most visited pages (shortcut for analytics_breakdown with dimension=path).", vec![("project", project()), ("since", since()), ("limit", prop("integer", "Max rows (default 10)"))], &["project"], true, false),
        tool(
            "resolve_issue",
            "Resolve issue",
            "Mark an issue resolved, optionally in the release that contains the fix. If it happens again it reopens as a regression. Needs a write token.",
            vec![("issue", issue()), ("resolved_in_release", prop("string", "Release with the fix"))],
            &["issue"],
            false,
            false,
        ),
        tool("ignore_issue", "Ignore issue", "Ignore an issue, forever or for a duration. Needs a write token.", vec![("issue", issue()), ("ignore_for", prop("string", "Duration such as 24h or 7d; omit for forever"))], &["issue"], false, false),
        tool("reopen_issue", "Reopen issue", "Set an issue back to open. Needs a write token.", vec![("issue", issue())], &["issue"], false, false),
        tool("assign_issue", "Assign issue", "Assign an issue to a user by email (empty string unassigns). Needs a write token.", vec![("issue", issue()), ("assignee", prop("string", "User email"))], &["issue", "assignee"], false, false),
        tool("get_release_commits", "Get release commits", "Commits recorded for a release (via `vaka releases set-commits`), with changed files. Used for suspect commits on issues.", vec![("project", project()), ("release", prop("string", "Release version"))], &["project", "release"], true, false),
        tool("get_ownership_rules", "Get ownership rules", "The project's code ownership rules (CODEOWNERS-like; last match wins), parsed, with any problems.", vec![("project", project())], &["project"], true, false),
        tool(
            "set_ownership_rules",
            "Set ownership rules",
            "Replace the project's ownership rules. One rule per line: `<pattern> <owner>...` where pattern is a path glob (src/payments/**), module:<glob>, type:<exception type> or tag:<key>=<value>; owners are emails or #team names; last matching line wins. New issues get these owners and are assigned to the first owner who is a Vaka user. Needs a write token.",
            vec![("project", project()), ("rules", prop("string", "The full rules text (empty clears them)"))],
            &["project", "rules"],
            false,
            false,
        ),
        tool("comment_issue", "Comment on issue", "Add a comment to an issue, e.g. root cause analysis or a link to the fix. Needs a write token.", vec![("issue", issue()), ("body", prop("string", "Comment text (Markdown)"))], &["issue", "body"], false, false),
        tool(
            "create_alert_rule",
            "Create alert rule",
            "Notify a webhook or email address on new issues, regressions or spikes. Needs a write token.",
            vec![
                ("project", prop("string", "Project slug; omit for all projects")),
                ("trigger", prop("string", "new_issue, regression or spike")),
                ("channel", prop("string", "webhook or email")),
                ("target", prop("string", "Webhook URL or comma-separated emails")),
                ("secret", prop("string", "Webhook signing secret (HMAC-SHA256 in X-Vaka-Signature)")),
                ("threshold", prop("integer", "Spike only: minimum events in the last hour (default 50)")),
                ("name", prop("string", "Optional name")),
            ],
            &["trigger", "channel", "target"],
            false,
            false,
        ),
        tool(
            "delete_user_data",
            "Delete user data (GDPR)",
            "Permanently delete all errors, events and sessions stored for a user id in a project. Cannot be undone. Needs a write token.",
            vec![("project", project()), ("user_id", prop("string", "The user id passed to identify()"))],
            &["project", "user_id"],
            false,
            true,
        ),
    ]
}
