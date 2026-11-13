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
list_projects -> list_issues (sort=events, since=24h) -> get_fix_context (in-app frames with repository source links, \
breadcrumbs, suspect commits, owners) -> fix the code with `Fixes <ID>` in the commit message -> link_issue with the PR URL \
(or resolve_issue with resolved_in_release). For 'why did X spike' use explain_spike; \
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

fn f(args: &Value, k: &str) -> Option<f64> {
    args.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

fn perf_args(a: &Value) -> Result<crate::perf::PerfParams, ToolError> {
    Ok(crate::perf::PerfParams {
        project: req(a, "project")?,
        since: s(a, "since"),
        until: s(a, "until"),
        environment: s(a, "environment"),
        release: s(a, "release"),
        op: s(a, "op"),
        transaction: s(a, "transaction"),
        limit: i(a, "limit"),
        sort: s(a, "sort"),
    })
}

fn flag_in(a: &Value) -> Result<crate::flags::FlagIn, ToolError> {
    let mut f: crate::flags::FlagIn = serde_json::from_value(a.clone()).map_err(|e| ToolError::Failed(format!("invalid flag: {e}")))?;
    f.key = s(a, "key");
    Ok(f)
}

fn engagement_args(a: &Value) -> Result<crate::cohorts::EngagementParams, ToolError> {
    Ok(crate::cohorts::EngagementParams { base: analytics_args(a)?, event: s(a, "event"), identity: s(a, "identity"), period: s(a, "period") })
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
        "release_adoption" => {
            crate::insights::release_adoption(state, p, &req(&a, "project")?, s(&a, "since").as_deref(), s(&a, "interval").as_deref(), s(&a, "environment").as_deref(), i(&a, "limit")).await?
        }
        "realtime_visitors" => crate::insights::realtime(state, p, &req(&a, "project")?, i(&a, "minutes")).await?,
        "analytics_revenue" => {
            crate::product::revenue(
                state,
                p,
                crate::product::RevenueParams {
                    base: analytics_args(&a)?,
                    event: s(&a, "event"),
                    value_prop: s(&a, "value_prop"),
                    currency_prop: s(&a, "currency_prop"),
                    breakdown: s(&a, "breakdown"),
                    limit: i(&a, "limit"),
                },
            )
            .await?
        }
        "list_goals" => crate::product::list_goals(state, p, analytics_args(&a)?).await?,
        "create_goal" => {
            crate::product::create_goal(state, p, &req(&a, "project")?, crate::product::GoalIn { name: req(&a, "name")?, step: req(&a, "step")?, value_prop: s(&a, "value_prop") }).await?
        }
        "list_flags" => crate::flags::list_flags(state, p, &req(&a, "project")?).await?,
        "get_flag" => crate::flags::get_flag(state, p, &req(&a, "project")?, &req(&a, "key")?).await?,
        "create_flag" => crate::flags::create_flag(state, p, &req(&a, "project")?, flag_in(&a)?).await?,
        "update_flag" => {
            let mut f = flag_in(&a)?;
            f.key = None;
            crate::flags::update_flag(state, p, &req(&a, "project")?, &req(&a, "key")?, f).await?
        }
        "delete_flag" => crate::flags::delete_flag(state, p, &req(&a, "project")?, &req(&a, "key")?).await?,
        "flag_health" => crate::flags::flag_health(state, p, &req(&a, "project")?, &req(&a, "key")?, s(&a, "since").as_deref(), s(&a, "release").as_deref()).await?,
        "create_experiment" => {
            let e = crate::flags::ExperimentIn { name: req(&a, "name")?, flag: req(&a, "flag")?, goal: req(&a, "goal")?, control: s(&a, "control"), description: s(&a, "description") };
            crate::flags::create_experiment(state, p, &req(&a, "project")?, e).await?
        }
        "list_experiments" => crate::flags::list_experiments(state, p, &req(&a, "project")?).await?,
        "experiment_results" => {
            let r = crate::flags::ResultsParams {
                project: req(&a, "project")?,
                experiment: s(&a, "experiment"),
                flag: s(&a, "flag"),
                goal: s(&a, "goal"),
                control: s(&a, "control"),
                since: s(&a, "since"),
                identity: s(&a, "identity"),
            };
            crate::flags::experiment_results(state, p, r).await?
        }
        "create_cohort" => {
            let def: crate::cohorts::CohortIn = serde_json::from_value(a.clone()).map_err(|e| ToolError::Failed(format!("invalid cohort: {e}")))?;
            crate::cohorts::create_cohort(state, p, &req(&a, "project")?, def).await?
        }
        "list_cohorts" => crate::cohorts::list_cohorts(state, p, &req(&a, "project")?, a.get("sizes").and_then(|v| v.as_bool()).unwrap_or(true)).await?,
        "get_cohort" => crate::cohorts::get_cohort(state, p, &req(&a, "project")?, &req(&a, "cohort")?, i(&a, "limit")).await?,
        "delete_cohort" => crate::cohorts::delete_cohort(state, p, &req(&a, "project")?, &req(&a, "cohort")?).await?,
        "analytics_stickiness" => crate::cohorts::stickiness(state, p, engagement_args(&a)?).await?,
        "analytics_lifecycle" => crate::cohorts::lifecycle(state, p, engagement_args(&a)?).await?,
        "delete_goal" => crate::product::delete_goal(state, p, &req(&a, "project")?, &req(&a, "goal")?).await?,
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
        "performance_summary" => crate::perf::summary(state, p, perf_args(&a)?).await?,
        "slow_endpoints" => crate::perf::slow_endpoints(state, p, perf_args(&a)?, i(&a, "min_count")).await?,
        "app_startup" => crate::perf::app_startup(state, p, perf_args(&a)?).await?,
        "web_vitals" => crate::perf::web_vitals(state, p, perf_args(&a)?, a.get("by_page").and_then(|v| v.as_bool())).await?,
        "list_slow_traces" => crate::perf::slow_traces(state, p, perf_args(&a)?, f(&a, "min_ms")).await?,
        "get_trace" => crate::perf::get_trace(state, p, &req(&a, "project")?, &req(&a, "trace_id")?).await?,
        "span_breakdown" => crate::perf::span_breakdown(state, p, perf_args(&a)?).await?,
        "compare_release_performance" => {
            crate::perf::compare_releases(state, p, &req(&a, "project")?, &req(&a, "base")?, &req(&a, "target")?, i(&a, "min_count"), f(&a, "threshold")).await?
        }
        "get_session_timeline" => crate::timeline::session_timeline(state, p, &req(&a, "project")?, &req(&a, "session_id")?, i(&a, "limit")).await?,
        "issue_precursors" => crate::timeline::precursors(state, p, &req(&a, "issue")?, i(&a, "minutes"), i(&a, "limit")).await?,
        "get_fix_context" => crate::fixctx::fix_context(state, p, &req(&a, "issue")?).await?,
        "link_issue" => {
            crate::fixctx::add_link(state, p, &req(&a, "issue")?, crate::fixctx::LinkIn { url: req(&a, "url")?, kind: s(&a, "kind"), title: s(&a, "title") }).await?
        }
        "get_code_mappings" => crate::fixctx::get_code_mappings(state, p, &req(&a, "project")?).await?,
        "set_code_mappings" => {
            let mappings: Vec<crate::fixctx::CodeMapping> = serde_json::from_value(a.get("mappings").cloned().unwrap_or(json!([])))
                .map_err(|e| ToolError::Failed(format!("`mappings` must be an array of {{stack_root, source_root, repository, default_branch}}: {e}")))?;
            crate::fixctx::set_code_mappings(state, p, &req(&a, "project")?, mappings).await?
        }
        "apply_ownership_rules" => crate::ownership::apply_ownership(state, p, &req(&a, "project")?, a.get("all").and_then(|v| v.as_bool()).unwrap_or(false)).await?,
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
                    filters: a.get("filters").cloned(),
                    max_per_hour: i(&a, "max_per_hour"),
                },
            )
            .await?
        }
        "list_alert_rules" => crate::api::list_rules_inner(state, p).await?,
        "update_alert_rule" => {
            crate::api::update_rule_inner(
                state,
                p,
                &req(&a, "rule_id")?,
                crate::api::RuleBody {
                    name: s(&a, "name"),
                    target: s(&a, "target"),
                    secret: s(&a, "secret"),
                    threshold: i(&a, "threshold"),
                    enabled: a.get("enabled").and_then(|v| v.as_bool()),
                    filters: a.get("filters").cloned(),
                    max_per_hour: i(&a, "max_per_hour"),
                    ..Default::default()
                },
            )
            .await?
        }
        "delete_alert_rule" => crate::api::delete_rule_inner(state, p, &req(&a, "rule_id")?).await?,
        "test_alert_rule" => crate::api::test_rule_inner(state, p, &req(&a, "rule_id")?).await?,
        "list_alert_deliveries" => {
            crate::api::deliveries_inner(state, p, &req(&a, "rule_id")?, a.get("include_payload").and_then(|v| v.as_bool()).unwrap_or(false), i(&a, "limit")).await?
        }
        "redeliver_alert" => crate::api::redeliver_inner(state, p, &req(&a, "rule_id")?, &req(&a, "delivery_id")?).await?,
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

fn flag_props(create: bool) -> Vec<(&'static str, Value)> {
    vec![
        ("project", prop("string", "Project slug or id (see list_projects)")),
        ("key", prop("string", if create { "Flag key, e.g. new_checkout" } else { "Flag key" })),
        ("description", prop("string", "What the flag controls")),
        ("enabled", prop("boolean", "Master switch (default true)")),
        ("rollout", prop("integer", "Percent of people who get the flag (0 to 100, default 100)")),
        ("variants", json!({"type": "array", "description": "Optional variants [{key, weight}], e.g. [{\"key\": \"control\", \"weight\": 1}, {\"key\": \"new\", \"weight\": 1}]", "items": {"type": "object", "properties": {"key": {"type": "string"}, "weight": {"type": "integer"}}, "required": ["key"]}})),
        ("rules", json!({"type": "array", "description": "Targeting rules, first match wins: {user_ids?: [..], properties?: {plan: \"pro\"}, variant?: forced variant, rollout?: percent}", "items": {"type": "object"}})),
        ("environments", json!({"type": "object", "additionalProperties": {"type": "boolean"}, "description": "Per-environment switch, e.g. {\"staging\": true, \"production\": false}"})),
    ]
}

fn perf_props(extra: &[(&'static str, Value)]) -> Vec<(&'static str, Value)> {
    let mut v = vec![
        ("project", prop("string", "Project slug or id (see list_projects)")),
        ("since", prop("string", "Start: ISO time or relative like 24h, 7d (default 24h)")),
        ("until", prop("string", "End (default now)")),
        ("environment", prop("string", "Environment")),
        ("release", prop("string", "Release")),
        ("transaction", prop("string", "Transaction name (page path, screen, `GET /api/users/:id`)")),
        ("limit", prop("integer", "Max rows")),
    ];
    v.extend(extra.iter().cloned());
    v
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
               "description": format!("Filter by dimension, e.g. {{\"path\": \"/pricing\", \"country\": \"SE\"}}. Prefix a value with ~ for contains or ! for not-equal. {{\"cohort\": \"<name>\"}} keeps people in a saved cohort (\"!<name>\" excludes them). Dimensions: {}", query::DIMENSIONS.join(", "))})
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
            "Crash-free sessions and users, session counts, errors, new issues and ANR/app hang rates (anr_rate_sessions, anr_rate_users) per release.",
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
            "release_adoption",
            "Release adoption",
            "Share of sessions and users on each release over time (hour or day buckets), with first session, current share and peak share per release. Use after shipping to see how fast a release rolls out.",
            vec![
                ("project", project()),
                ("since", prop("string", "Start of the window (default 14d)")),
                ("interval", prop("string", "hour or day (default: hour for windows up to 2 days)")),
                ("environment", prop("string", "Only this environment")),
                ("limit", prop("integer", "Releases to show separately; the rest are `other` (default 5)")),
            ],
            &["project"],
            true,
            false,
        ),
        tool(
            "realtime_visitors",
            "Real-time visitors",
            "Traffic right now: visitors, pageviews, active sessions and errors in the last few minutes, top pages, referrers, countries and events, plus visitors per minute for the last 30 minutes.",
            vec![("project", project()), ("minutes", prop("integer", "Window in minutes (default 5, max 60)"))],
            &["project"],
            true,
            false,
        ),
        tool(
            "analytics_revenue",
            "Revenue",
            "Revenue from a purchase event's numeric property, per currency: total, conversions, customers and average, optionally broken down by a dimension (referrer, utm_campaign, country, prop:plan...).",
            vec![
                ("project", project()),
                ("event", prop("string", "Event in step syntax (default `purchase`), e.g. `checkout_completed|prop:plan:pro`")),
                ("value_prop", prop("string", "Numeric property with the amount (default `revenue`)")),
                ("currency_prop", prop("string", "Property with the currency code (default `currency`)")),
                ("breakdown", prop("string", "Optional dimension to split revenue by")),
                ("since", since()),
                ("until", until()),
                ("filters", filters()),
                ("limit", prop("integer", "Max breakdown rows (default 10)")),
            ],
            &["project"],
            true,
            false,
        ),
        tool(
            "list_goals",
            "List goals",
            "Saved conversion goals with conversions, unique converters, conversion rate (converters / visitors) and revenue in the window.",
            vec![("project", project()), ("since", since()), ("until", until()), ("filters", filters())],
            &["project"],
            true,
            false,
        ),
        tool(
            "create_goal",
            "Create goal",
            "Save a named conversion goal. Needs a write token.",
            vec![
                ("project", project()),
                ("name", prop("string", "Goal name, e.g. Signup")),
                ("step", prop("string", "Event in step syntax: `signup`, `pageview|path:/thanks`, `purchase|prop:plan:pro`")),
                ("value_prop", prop("string", "Optional numeric property to sum as revenue (uses the `currency` property)")),
            ],
            &["project", "name", "step"],
            false,
            false,
        ),
        tool("list_flags", "List feature flags", "Feature flags with rollout, variants, targeting rules and per-environment switches.", vec![("project", project())], &["project"], true, false),
        tool(
            "get_flag",
            "Get feature flag",
            "One flag's configuration and how many people saw each variant in the last 7 days.",
            vec![("project", project()), ("key", prop("string", "Flag key"))],
            &["project", "key"],
            true,
            false,
        ),
        tool(
            "create_flag",
            "Create feature flag",
            "Create a flag. Without variants it is boolean (on for `rollout` percent of people); with 2 to 10 variants the rolled-out people are split by weight. Bucketing is deterministic per person (sha256 of key and id). Needs a write token.",
            flag_props(true),
            &["project", "key"],
            false,
            false,
        ),
        tool(
            "update_flag",
            "Update feature flag",
            "Change a flag: roll out (rollout 0 to 100), turn off (enabled false, or environments {\"production\": false}), or force a safe variant with a rule {\"variant\": \"control\"}. Only given fields change. Needs a write token.",
            flag_props(false),
            &["project", "key"],
            false,
            false,
        ),
        tool("delete_flag", "Delete feature flag", "Delete a flag; SDKs then get false for it. Needs a write token.", vec![("project", project()), ("key", prop("string", "Flag key"))], &["project", "key"], false, true),
        tool(
            "flag_health",
            "Flag health",
            "Release health split by flag variant: sessions, crash-free and healthy session rates, errors per session, and issues that happen mostly in one variant (with lift). Use it to decide whether to roll a variant back.",
            vec![("project", project()), ("key", prop("string", "Flag key")), ("since", prop("string", "Window start (default 7d)")), ("release", prop("string", "Only this release"))],
            &["project", "key"],
            true,
            false,
        ),
        tool(
            "create_experiment",
            "Create experiment",
            "Save an experiment: a flag and a goal event to compare variants on. Needs a write token.",
            vec![
                ("project", project()),
                ("name", prop("string", "Experiment name")),
                ("flag", prop("string", "Flag key")),
                ("goal", prop("string", "Goal event in step syntax: `signup`, `purchase|prop:plan:pro`")),
                ("control", prop("string", "Control variant (default: the first variant)")),
                ("description", prop("string", "Hypothesis")),
            ],
            &["project", "name", "flag", "goal"],
            false,
            false,
        ),
        tool("list_experiments", "List experiments", "Saved experiments with flag, goal and control.", vec![("project", project())], &["project"], true, false),
        tool(
            "experiment_results",
            "Experiment results",
            "Conversion per variant (people exposed to the flag who did the goal), uplift against the control, 95% confidence interval, p-value of a two-proportion z-test and a plain verdict. Give `experiment`, or `flag` and `goal`.",
            vec![
                ("project", project()),
                ("experiment", prop("string", "Saved experiment name")),
                ("flag", prop("string", "Flag key (without a saved experiment)")),
                ("goal", prop("string", "Goal step (without a saved experiment)")),
                ("control", prop("string", "Control variant")),
                ("since", prop("string", "Window start (default: experiment start or 14d)")),
                ("identity", prop("string", "auto (default), user or visitor")),
            ],
            &["project"],
            true,
            false,
        ),
        tool(
            "create_cohort",
            "Create cohort",
            "Save a group of people defined by behavior, e.g. power users (`purchase` at least 3 times in 30d), churn risk (active in 30d but no `login` in 14d), or Swedish Pro users. Use it afterwards as filters {\"cohort\": name} in any analytics tool or breakdown `cohort:<name>` in funnels. Needs a write token.",
            vec![
                ("project", project()),
                ("name", prop("string", "Cohort name, e.g. power_buyers")),
                ("description", prop("string", "What the cohort means")),
                ("criteria", json!({"type": "array", "maxItems": 6, "description": "All must hold. Each: {event or step (step syntax: \"purchase|prop:plan:pro\"), filters?, min_count? (default 1), within? (look-back from now, e.g. 14d), negate? (true = did NOT do it)}",
                    "items": {"type": "object", "properties": {"event": {"type": "string"}, "step": {"type": "string"}, "filters": {"type": "object", "additionalProperties": {"type": "string"}}, "min_count": {"type": "integer"}, "within": {"type": "string"}, "negate": {"type": "boolean"}}}})),
                ("properties", json!({"type": "object", "additionalProperties": {"type": "string"}, "description": "Dimension filters at least one of the person's events must match, e.g. {\"country\": \"SE\", \"prop:plan\": \"pro\"}"})),
                ("within", prop("string", "Default look-back window and activity window (default 30d, max 365d)")),
                ("identity", prop("string", "auto (default: user id when identified, else visitor id), user (identified only) or visitor")),
            ],
            &["project", "name"],
            false,
            false,
        ),
        tool("list_cohorts", "List cohorts", "Saved cohorts with their definitions and current sizes.", vec![("project", project()), ("sizes", prop("boolean", "Compute sizes (default true)"))], &["project"], true, false),
        tool(
            "get_cohort",
            "Get cohort",
            "A cohort's definition, current size and most recently active members (user ids for identified people, use get_user_timeline on them).",
            vec![("project", project()), ("cohort", prop("string", "Cohort name or id")), ("limit", prop("integer", "Max sample members (default 20)"))],
            &["project", "cohort"],
            true,
            false,
        ),
        tool("delete_cohort", "Delete cohort", "Delete a saved cohort. Needs a write token.", vec![("project", project()), ("cohort", prop("string", "Cohort name or id"))], &["project", "cohort"], false, true),
        tool(
            "analytics_stickiness",
            "Stickiness",
            "How habitual usage is: people active in the window (MAU for 30d), WAU, average DAU, DAU/MAU and how many days per week people are active. Optionally for one event (e.g. the core action).",
            vec![
                ("project", project()),
                ("event", prop("string", "Only count this event (step syntax), default any event")),
                ("identity", prop("string", "auto (default), user or visitor")),
                ("since", prop("string", "Window start (default 30d)")),
                ("until", until()),
                ("filters", filters()),
            ],
            &["project"],
            true,
            false,
        ),
        tool(
            "analytics_lifecycle",
            "Lifecycle",
            "Growth accounting per day or week: new, returning, resurrecting and dormant (negative) people, active total and quick ratio ((new + resurrecting) / dormant).",
            vec![
                ("project", project()),
                ("period", prop("string", "day or week (default)")),
                ("event", prop("string", "Only count this event (step syntax), default any event")),
                ("identity", prop("string", "auto (default), user or visitor")),
                ("since", prop("string", "Window start (default 8 weeks or 14 days)")),
                ("until", until()),
                ("filters", filters()),
            ],
            &["project"],
            true,
            false,
        ),
        tool("delete_goal", "Delete goal", "Delete a saved goal by id or name. Needs a write token.", vec![("project", project()), ("goal", prop("string", "Goal id or name"))], &["project", "goal"], false, true),
        tool(
            "analytics_funnel",
            "Funnel",
            "Conversion through ordered steps within a time window: people per step, conversion and drop-off, optionally broken down by a dimension.",
            vec![
                ("project", project()),
                ("steps", json!({"type": "array", "minItems": 2, "maxItems": 10, "description": "Ordered steps. Each is an event name (\"signup\"), a string \"pageview|path:/pricing\", or {\"event\": \"pageview\", \"filters\": {\"path\": \"/pricing\"}}. Event names come from list_event_names.", "items": {"anyOf": [{"type": "string"}, {"type": "object", "properties": {"event": {"type": "string"}, "filters": {"type": "object", "additionalProperties": {"type": "string"}}}, "required": ["event"]}]}})),
                ("window", prop("string", "Max time from first to last step: 30m, 1h, 1d (default), 7d")),
                ("breakdown", prop("string", "Optional dimension to split by, e.g. country, utm_source, os, prop:plan, or cohort:<name> (in vs not in a saved cohort)")),
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
        tool(
            "get_fix_context",
            "Get fix context",
            "Everything needed to fix an issue in one call: the crash's in-app frames with repository source links (url, path, line at the release's commit) and context lines, last breadcrumbs, tags, suspect commits, owners, linked PRs, and a suggested branch name, PR title and `Fixes <ID>` commit trailer that auto-resolves the issue when the release's commits reach Vaka. Start here when asked to fix a crash.",
            vec![("issue", issue())],
            &["issue"],
            true,
            false,
        ),
        tool(
            "link_issue",
            "Link PR to issue",
            "Record a pull request, ticket or commit URL on an issue (e.g. right after opening a fix PR) so people and other agents can see work is in progress. Kind is guessed from the URL when omitted. Needs a write token.",
            vec![("issue", issue()), ("url", prop("string", "https URL of the PR, ticket or commit")), ("kind", prop("string", "pull_request, issue, commit or other")), ("title", prop("string", "Optional title"))],
            &["issue", "url"],
            false,
            false,
        ),
        tool(
            "performance_summary",
            "Performance summary",
            "Latency per transaction (root span: page load, screen, HTTP endpoint, job): count, per-minute throughput, p50/p75/p95/p99, error rate, plus the overall numbers. Start here for 'is the app slow'.",
            perf_props(&[("op", prop("string", "Only this op, e.g. http.server, pageload, navigation, app.start.cold; a trailing * matches a prefix")), ("sort", prop("string", "p95 (default), p50, p99, count, errors or total"))]),
            &["project"],
            true,
            false,
        ),
        tool(
            "slow_endpoints",
            "Slow endpoints",
            "Slowest HTTP server endpoints (op http.server*) by p95, with total time spent and 5xx counts. Paths are grouped (/users/:id).",
            perf_props(&[("min_count", prop("integer", "Minimum requests per endpoint (default 5)"))]),
            &["project"],
            true,
            false,
        ),
        tool(
            "app_startup",
            "App startup",
            "Cold and warm app start durations (op app.start.cold / app.start.warm) per release: p50/p95/p99 and counts.",
            perf_props(&[]),
            &["project"],
            true,
            false,
        ),
        tool(
            "web_vitals",
            "Web Vitals",
            "Core Web Vitals (LCP, INP, CLS) plus FCP and TTFB: p75, rating (good / needs-improvement / poor by web.dev thresholds) and the share of good and poor page views, overall and for the busiest pages. `transaction` filters to one page path.",
            perf_props(&[("by_page", prop("boolean", "Include per-page numbers (default true)"))]),
            &["project"],
            true,
            false,
        ),
        tool(
            "list_slow_traces",
            "List slow traces",
            "Slowest individual traces (root spans) matching the filters, newest data first in the window. Open one with get_trace.",
            perf_props(&[("op", prop("string", "Only this op (trailing * for prefix)")), ("min_ms", prop("number", "Minimum duration in ms"))]),
            &["project"],
            true,
            false,
        ),
        tool(
            "get_trace",
            "Get trace",
            "One trace as a depth-first span tree with offsets, durations and self time, plus errors (and logs) that carry the trace id. The span with the largest self_time_ms is usually the bottleneck.",
            vec![("project", project()), ("trace_id", prop("string", "Trace id"))],
            &["project", "trace_id"],
            true,
            false,
        ),
        tool(
            "span_breakdown",
            "Span breakdown",
            "Where a transaction's time goes: its child spans (db, http.client, render, ...) grouped by op and name across the latest 1000 traces, with count per transaction, total, p95 and share of transaction time.",
            perf_props(&[]),
            &["project", "transaction"],
            true,
            false,
        ),
        tool(
            "compare_release_performance",
            "Compare release performance",
            "Performance regressions between two releases: transactions whose p95 grew by more than `threshold` with enough samples, improvements, and Web Vitals p75 changes. Use after a deploy or when release_health looks fine but users complain.",
            vec![
                ("project", project()),
                ("base", prop("string", "Older release")),
                ("target", prop("string", "Newer release")),
                ("min_count", prop("integer", "Minimum samples per release (default 20)")),
                ("threshold", prop("number", "Relative p95 increase that counts as a regression (default 0.2 = 20%)")),
            ],
            &["project", "base", "target"],
            true,
            false,
        ),
        tool(
            "get_session_timeline",
            "Get session timeline",
            "Everything recorded in one session, oldest first: session start, pageviews, screen views, custom events, breadcrumbs (clicks, navigation, HTTP, console, lifecycle), errors, crashes and how the session ended. Session ids come from events (get_issue latest_event.session_id, get_user_timeline).",
            vec![("project", project()), ("session_id", prop("string", "Session id")), ("limit", prop("integer", "Max entries, newest kept (default 500)"))],
            &["project", "session_id"],
            true,
            false,
        ),
        tool(
            "issue_precursors",
            "Issue precursors",
            "What users most often did in the minutes before an issue across all affected sessions: pages, screens and events with their share of crash sessions and lift versus all sessions, plus the most common breadcrumbs. Use it to find the trigger of a crash that is hard to reproduce.",
            vec![("issue", issue()), ("minutes", prop("integer", "Window before each crash (default 5, max 60)")), ("limit", prop("integer", "Max rows (default 15)"))],
            &["issue"],
            true,
            false,
        ),
        tool("get_code_mappings", "Get code mappings", "How stack-trace paths map to repository paths (used for source links on frames).", vec![("project", project())], &["project"], true, false),
        tool(
            "set_code_mappings",
            "Set code mappings",
            "Replace the project's code mappings. Each maps a stack path prefix to a repository path: {stack_root: \"build/src/\", source_root: \"packages/web/src/\", repository: \"https://github.com/acme/shop\", default_branch: \"main\"}. Longest stack_root wins; empty stack_root matches everything. Needs a write token.",
            vec![("project", project()), ("mappings", json!({"type": "array", "items": {"type": "object", "properties": {"stack_root": {"type": "string"}, "source_root": {"type": "string"}, "repository": {"type": "string"}, "default_branch": {"type": "string"}}}}))],
            &["project", "mappings"],
            false,
            false,
        ),
        tool(
            "apply_ownership_rules",
            "Apply ownership rules",
            "Re-apply the project's ownership rules to open issues: by default only issues without owners, with all=true every open issue. Needs a write token.",
            vec![("project", project()), ("all", prop("boolean", "Also recompute issues that already have owners"))],
            &["project"],
            false,
            false,
        ),
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
            "Notify or wake an agent when something happens. Triggers: new_issue, regression, spike, threshold (issue reaches N events), issue_resolved, issue_assigned, issue_commented, release_created, perf_regression (newest release slower than the previous one; threshold = p95 increase in percent, default 20), digest. Channels: webhook (signed JSON with the full fix context), slack, email, github_dispatch (repository_dispatch `vaka.<trigger>` to start a GitHub Actions workflow, e.g. a coding agent), github_issue (opens one GitHub issue per Vaka issue; later alerts comment). Failed deliveries are retried with backoff. Needs a write token.",
            vec![
                ("project", prop("string", "Project slug; omit for all projects")),
                ("trigger", prop("string", "new_issue, regression, spike, threshold, issue_resolved, issue_assigned, issue_commented, release_created, perf_regression or digest")),
                ("channel", prop("string", "webhook, slack, email, github_dispatch or github_issue")),
                ("target", prop("string", "Webhook URL, Slack incoming webhook URL, comma-separated emails, or owner/repo for GitHub channels")),
                ("secret", prop("string", "Webhook signing secret (HMAC-SHA256 in X-Vaka-Signature), or the GitHub token for GitHub channels")),
                ("threshold", prop("integer", "Spike: minimum events in the last hour (default 50). Threshold: events (default 100). Digest: UTC hour (default 8). perf_regression: p95 increase in percent (default 20)")),
                ("filters", json!({"type": "object", "description": "Optional: {environment, min_level (debug|info|warning|error|fatal), release (glob), owner (email), unhandled_only (bool), tags: {key: glob}, mention (github_issue: first line of the issue body, e.g. an agent mention)}"})),
                ("max_per_hour", prop("integer", "Rate limit per rule (default 10 for GitHub channels, otherwise unlimited)")),
                ("name", prop("string", "Optional name")),
            ],
            &["trigger", "channel", "target"],
            false,
            false,
        ),
        tool("list_alert_rules", "List alert rules", "Alert rules visible to this token, with triggers, channels and filters (secrets are never returned).", vec![], &[], true, false),
        tool(
            "update_alert_rule",
            "Update alert rule",
            "Change an alert rule: name, target, secret, threshold, enabled, filters (null clears), max_per_hour. Needs a write token.",
            vec![
                ("rule_id", prop("string", "Rule id (see list_alert_rules)")),
                ("name", prop("string", "New name")),
                ("target", prop("string", "New target")),
                ("secret", prop("string", "New secret or token")),
                ("threshold", prop("integer", "New threshold")),
                ("enabled", prop("boolean", "Enable or disable")),
                ("filters", json!({"type": ["object", "null"], "description": "New filters (see create_alert_rule)"})),
                ("max_per_hour", prop("integer", "New rate limit (0 = unlimited)")),
            ],
            &["rule_id"],
            false,
            false,
        ),
        tool("delete_alert_rule", "Delete alert rule", "Delete an alert rule. Needs a write token.", vec![("rule_id", prop("string", "Rule id"))], &["rule_id"], false, true),
        tool("test_alert_rule", "Test alert rule", "Send a test notification (digests: a real digest) through a rule now. Needs a write token.", vec![("rule_id", prop("string", "Rule id"))], &["rule_id"], false, false),
        tool(
            "list_alert_deliveries",
            "List alert deliveries",
            "Recent deliveries for a rule: status (ok, retrying, failed, rate_limited, skipped), attempts, next retry and error detail, optionally with the payload that was sent. Needs a write token.",
            vec![("rule_id", prop("string", "Rule id")), ("include_payload", prop("boolean", "Include the JSON payload")), ("limit", prop("integer", "Max rows (default 50)"))],
            &["rule_id"],
            true,
            false,
        ),
        tool("redeliver_alert", "Redeliver alert", "Send a stored delivery's payload again now. Needs a write token.", vec![("rule_id", prop("string", "Rule id")), ("delivery_id", prop("string", "Delivery id from list_alert_deliveries"))], &["rule_id", "delivery_id"], false, false),
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
