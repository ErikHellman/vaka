//! REST integration API: `/api/v1/*`. Authenticated with API tokens (or a web
//! session). Thin wrappers over the shared query layer.

use crate::auth::{ApiError, Principal, Role};
use crate::db::RowExt;
use crate::query::{self, AnalyticsParams, EventSearchParams, IssueListParams, IssueUpdate, QResult};
use crate::state::AppState;
use crate::util::{iso_time, new_id, now_ms};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::io::AsyncWriteExt;

type J = QResult<Json<Value>>;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me", get(me))
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route("/api/v1/projects/{p}", get(get_project).patch(update_project))
        .route("/api/v1/projects/{p}/keys", post(create_key))
        .route("/api/v1/projects/{p}/keys/{key}", delete(revoke_key))
        .route("/api/v1/projects/{p}/issues", get(list_issues))
        .route("/api/v1/projects/{p}/events", get(search_events))
        .route("/api/v1/projects/{p}/events/{event_id}", get(get_event))
        .route("/api/v1/projects/{p}/releases", get(list_releases))
        .route("/api/v1/projects/{p}/releases/health", get(release_health))
        .route("/api/v1/projects/{p}/releases/compare", get(compare_releases))
        .route(
            "/api/v1/projects/{p}/releases/{version}/commits",
            get(get_release_commits).put(put_release_commits).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/api/v1/projects/{p}/ownership", get(get_ownership).put(put_ownership))
        .route("/api/v1/projects/{p}/ownership/apply", post(apply_ownership))
        .route("/api/v1/projects/{p}/code-mappings", get(get_code_mappings).put(put_code_mappings))
        .route("/api/v1/issues/{id}/fix-context", get(fix_context))
        .route("/api/v1/issues/{id}/precursors", get(issue_precursors))
        .route("/api/v1/projects/{p}/sessions/{session_id}/timeline", get(session_timeline))
        .route("/api/v1/projects/{p}/performance/summary", get(perf_summary))
        .route("/api/v1/projects/{p}/performance/endpoints", get(perf_endpoints))
        .route("/api/v1/projects/{p}/performance/app-start", get(perf_app_start))
        .route("/api/v1/projects/{p}/performance/web-vitals", get(perf_web_vitals))
        .route("/api/v1/projects/{p}/performance/traces", get(perf_traces))
        .route("/api/v1/projects/{p}/performance/breakdown", get(perf_breakdown))
        .route("/api/v1/projects/{p}/performance/compare", get(perf_compare))
        .route("/api/v1/projects/{p}/traces/{trace_id}", get(get_trace))
        .route("/api/v1/issues/{id}/links", get(list_links).post(add_link))
        .route("/api/v1/issues/{id}/links/{link}", delete(delete_link))
        .route("/api/v1/projects/{p}/analytics/summary", get(analytics_summary))
        .route("/api/v1/projects/{p}/analytics/timeseries", get(analytics_timeseries))
        .route("/api/v1/projects/{p}/analytics/breakdown", get(analytics_breakdown))
        .route("/api/v1/projects/{p}/explain-spike", get(explain_spike))
        .route("/api/v1/projects/{p}/analytics/funnel", get(analytics_funnel))
        .route("/api/v1/projects/{p}/analytics/retention", get(analytics_retention))
        .route("/api/v1/projects/{p}/analytics/paths", get(analytics_paths))
        .route("/api/v1/projects/{p}/analytics/events", get(analytics_events))
        .route("/api/v1/projects/{p}/analytics/revenue", get(analytics_revenue))
        .route("/api/v1/projects/{p}/analytics/realtime", get(analytics_realtime))
        .route("/api/v1/projects/{p}/goals", get(list_goals).post(create_goal))
        .route("/api/v1/projects/{p}/flags", get(list_flags).post(create_flag))
        .route("/api/v1/projects/{p}/flags/{key}", get(get_flag).patch(update_flag).delete(delete_flag))
        .route("/api/v1/projects/{p}/flags/{key}/health", get(flag_health))
        .route("/api/v1/projects/{p}/anomalies", get(anomalies))
        .route("/api/v1/projects/{p}/logs", get(search_logs))
        .route("/api/v1/projects/{p}/digest/preview", get(digest_preview))
        .route("/api/v1/projects/{p}/flags/{key}/results", get(flag_results))
        .route("/api/v1/projects/{p}/experiments", get(list_experiments).post(create_experiment))
        .route("/api/v1/projects/{p}/experiments/{name}/results", get(experiment_results))
        .route("/api/v1/projects/{p}/cohorts", get(list_cohorts).post(create_cohort))
        .route("/api/v1/projects/{p}/cohorts/{cohort}", get(get_cohort).delete(delete_cohort))
        .route("/api/v1/projects/{p}/analytics/stickiness", get(analytics_stickiness))
        .route("/api/v1/projects/{p}/analytics/lifecycle", get(analytics_lifecycle))
        .route("/api/v1/projects/{p}/goals/{goal}", delete(delete_goal))
        .route("/api/v1/projects/{p}/releases/adoption", get(release_adoption))
        .route("/api/v1/projects/{p}/users", get(list_identified_users))
        .route("/api/v1/projects/{p}/users/{user_id}/timeline", get(user_timeline))
        .route(
            "/api/v1/projects/{p}/artifacts",
            get(list_artifacts).post(upload_artifact).layer(DefaultBodyLimit::max(1024 * 1024 * 1024)),
        )
        .route("/api/v1/projects/{p}/artifacts/{id}", delete(delete_artifact))
        .route("/api/v1/projects/{p}/users/{user_id}", delete(erase_user))
        .route("/api/v1/projects/{p}/install/{platform}", get(install_instructions))
        .route("/api/v1/projects/{p}/feedback", get(list_feedback))
        .route("/api/v1/projects/{p}/feedback/summary", get(feedback_summary))
        .route("/api/v1/feedback/{id}", get(get_feedback).patch(update_feedback))
        .route("/api/v1/feedback/{id}/attachments/{n}", get(feedback_attachment))
        .route("/api/v1/issues/{id}", get(get_issue).patch(update_issue))
        .route("/api/v1/issues/{id}/events", get(issue_events))
        .route("/api/v1/issues/{id}/comments", post(comment_issue))
        .route("/api/v1/projects/{p}/monitors", get(list_monitors).post(create_monitor))
        .route("/api/v1/projects/{p}/monitors/{slug}", get(get_monitor).patch(update_monitor).delete(delete_monitor))
        .route("/api/v1/projects/{p}/monitors/{slug}/checks", get(monitor_checks))
        .route("/api/v1/projects/{p}/incidents", get(list_incidents))
        .route("/api/v1/incidents/{id}", get(get_incident).patch(update_incident))
        .route("/api/v1/alert-rules", get(list_rules).post(create_rule))
        .route("/api/v1/alert-rules/{id}", patch(update_rule).delete(delete_rule))
        .route("/api/v1/alert-rules/{id}/test", post(test_rule))
        .route("/api/v1/alert-rules/{id}/deliveries", get(rule_deliveries))
        .route("/api/v1/alert-rules/{id}/deliveries/{delivery}/redeliver", post(redeliver))
        .route("/api/v1/tokens", get(list_tokens).post(create_token))
        .route("/api/v1/tokens/{id}", delete(revoke_token))
        .route("/api/v1/admin/read-only", get(get_read_only).put(put_read_only))
        .route("/api/v1/users", get(list_users))
        .route("/api/v1/users/invite", post(invite_user))
        .route("/api/v1/users/{id}", patch(update_user).delete(delete_user))
        .route("/api/v1/openapi.json", get(|| async { Json(crate::openapi::spec()) }))
}

async fn list_feedback(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(mut q): Query<crate::feedback::ListParams>) -> J {
    q.project = project;
    Ok(Json(crate::feedback::list(&s, &p, q).await?))
}

async fn feedback_summary(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(mut q): Query<crate::feedback::SummaryParams>) -> J {
    q.project = project;
    Ok(Json(crate::feedback::summary(&s, &p, q).await?))
}

async fn get_feedback(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    Ok(Json(crate::feedback::get(&s, &p, &id).await?))
}

async fn update_feedback(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Json(u): Json<crate::feedback::Update>) -> J {
    Ok(Json(crate::feedback::update(&s, &p, &id, u).await?))
}

async fn feedback_attachment(State(s): State<AppState>, p: Principal, Path((id, n)): Path<(String, i64)>) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let (ct, bytes) = crate::feedback::attachment(&s, &p, &id, n).await?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, ct),
            (axum::http::header::CACHE_CONTROL, "private, max-age=3600".to_string()),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response())
}

async fn me(p: Principal) -> J {
    Ok(Json(match p {
        Principal::User { id, email, name, role } => json!({"type": "user", "id": id, "email": email, "name": name, "role": role}),
        Principal::Token { id, name, project_id, write } => {
            json!({"type": "token", "id": id, "name": name, "project_id": project_id, "scope": if write {"write"} else {"read"}})
        }
    }))
}

async fn list_projects(State(s): State<AppState>, p: Principal) -> J {
    Ok(Json(query::list_projects(&s, &p).await?))
}

async fn get_release_commits(State(s): State<AppState>, p: Principal, Path((project, version)): Path<(String, String)>) -> J {
    Ok(Json(crate::ownership::get_release_commits(&s, &p, &project, &version).await?))
}

async fn put_release_commits(
    State(s): State<AppState>,
    p: Principal,
    Path((project, version)): Path<(String, String)>,
    Json(body): Json<crate::ownership::SetCommits>,
) -> J {
    Ok(Json(crate::ownership::put_release_commits(&s, &p, &project, &version, body).await?))
}

#[derive(Deserialize)]
struct ApplyQuery {
    #[serde(default)]
    all: Option<bool>,
}

async fn apply_ownership(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<ApplyQuery>) -> J {
    Ok(Json(crate::ownership::apply_ownership(&s, &p, &project, q.all.unwrap_or(false)).await?))
}

async fn get_code_mappings(State(s): State<AppState>, p: Principal, Path(project): Path<String>) -> J {
    Ok(Json(crate::fixctx::get_code_mappings(&s, &p, &project).await?))
}

#[derive(Deserialize)]
struct MappingsBody {
    mappings: Vec<crate::fixctx::CodeMapping>,
}

async fn put_code_mappings(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Json(b): Json<MappingsBody>) -> J {
    Ok(Json(crate::fixctx::set_code_mappings(&s, &p, &project, b.mappings).await?))
}

#[derive(Deserialize)]
struct PrecursorQuery {
    minutes: Option<i64>,
    limit: Option<i64>,
}

#[derive(Deserialize, Default)]
struct PerfQuery {
    since: Option<String>,
    until: Option<String>,
    environment: Option<String>,
    release: Option<String>,
    op: Option<String>,
    transaction: Option<String>,
    limit: Option<i64>,
    sort: Option<String>,
    min_count: Option<i64>,
    min_ms: Option<f64>,
    by_page: Option<bool>,
    base: Option<String>,
    target: Option<String>,
    threshold: Option<f64>,
}

impl PerfQuery {
    fn params(&self, project: String) -> crate::perf::PerfParams {
        crate::perf::PerfParams {
            project,
            since: self.since.clone(),
            until: self.until.clone(),
            environment: self.environment.clone(),
            release: self.release.clone(),
            op: self.op.clone(),
            transaction: self.transaction.clone(),
            limit: self.limit,
            sort: self.sort.clone(),
        }
    }
}

async fn perf_summary(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<PerfQuery>) -> J {
    Ok(Json(crate::perf::summary(&s, &p, q.params(project)).await?))
}

async fn perf_endpoints(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<PerfQuery>) -> J {
    Ok(Json(crate::perf::slow_endpoints(&s, &p, q.params(project), q.min_count).await?))
}

async fn perf_app_start(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<PerfQuery>) -> J {
    Ok(Json(crate::perf::app_startup(&s, &p, q.params(project)).await?))
}

async fn perf_web_vitals(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<PerfQuery>) -> J {
    Ok(Json(crate::perf::web_vitals(&s, &p, q.params(project), q.by_page).await?))
}

async fn perf_traces(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<PerfQuery>) -> J {
    Ok(Json(crate::perf::slow_traces(&s, &p, q.params(project), q.min_ms).await?))
}

async fn perf_breakdown(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<PerfQuery>) -> J {
    Ok(Json(crate::perf::span_breakdown(&s, &p, q.params(project)).await?))
}

async fn perf_compare(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<PerfQuery>) -> J {
    let base = q.base.clone().ok_or_else(|| ApiError::bad("`base` is required"))?;
    let target = q.target.clone().ok_or_else(|| ApiError::bad("`target` is required"))?;
    Ok(Json(crate::perf::compare_releases(&s, &p, &project, &base, &target, q.min_count, q.threshold).await?))
}

async fn get_trace(State(s): State<AppState>, p: Principal, Path((project, trace_id)): Path<(String, String)>) -> J {
    Ok(Json(crate::perf::get_trace(&s, &p, &project, &trace_id).await?))
}

async fn issue_precursors(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Query(q): Query<PrecursorQuery>) -> J {
    Ok(Json(crate::timeline::precursors(&s, &p, &id, q.minutes, q.limit).await?))
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<i64>,
}

async fn session_timeline(State(s): State<AppState>, p: Principal, Path((project, sid)): Path<(String, String)>, Query(q): Query<LimitQuery>) -> J {
    Ok(Json(crate::timeline::session_timeline(&s, &p, &project, &sid, q.limit).await?))
}

async fn fix_context(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    Ok(Json(crate::fixctx::fix_context(&s, &p, &id).await?))
}

async fn list_links(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    let issue = query::issue_for(&s, &p, &id).await?;
    Ok(Json(json!({"issue": issue["short_id"], "links": crate::fixctx::links_for(&s, issue["id"].as_str().unwrap_or("")).await?})))
}

async fn add_link(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Json(l): Json<crate::fixctx::LinkIn>) -> J {
    Ok(Json(crate::fixctx::add_link(&s, &p, &id, l).await?))
}

async fn delete_link(State(s): State<AppState>, p: Principal, Path((id, link)): Path<(String, String)>) -> J {
    Ok(Json(crate::fixctx::delete_link(&s, &p, &id, &link).await?))
}

async fn get_ownership(State(s): State<AppState>, p: Principal, Path(project): Path<String>) -> J {
    Ok(Json(crate::ownership::get_ownership(&s, &p, &project).await?))
}

#[derive(Deserialize)]
struct OwnershipBody {
    rules: String,
}

async fn put_ownership(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Json(body): Json<OwnershipBody>) -> J {
    Ok(Json(crate::ownership::set_ownership(&s, &p, &project, &body.rules).await?))
}

#[derive(Deserialize)]
pub struct CreateProject {
    pub name: String,
    pub platform: String,
    pub slug: Option<String>,
}

pub const PLATFORMS: &[&str] = &["javascript", "android", "ios", "rust", "node", "jvm", "go", "dotnet", "python", "other"];

pub async fn create_project_inner(s: &AppState, p: &Principal, req: CreateProject) -> QResult<Value> {
    if !p.can_create_project() {
        return Err(ApiError::forbidden());
    }
    if req.name.trim().is_empty() {
        return Err(ApiError::bad("project name is required"));
    }
    let platform = req.platform.to_lowercase();
    if !PLATFORMS.contains(&platform.as_str()) {
        return Err(ApiError::bad(format!("unknown platform `{platform}`; use one of {}", PLATFORMS.join(", "))));
    }
    let (proj, key) = crate::projects::create(s, req.name.trim(), &platform, req.slug.as_deref()).await?;
    crate::auth::audit(s, &p.label(), "create_project", &proj.id, &proj.slug).await;
    let mut v = query::project_json(s, &proj, true).await?;
    v["ingest_key"] = json!(key);
    v["endpoint"] = json!(s.config.public_url);
    v["install_instructions_url"] = json!(format!("{}/api/v1/projects/{}/install/{}", s.config.public_url, proj.slug, platform));
    Ok(v)
}

async fn create_project(State(s): State<AppState>, p: Principal, Json(req): Json<CreateProject>) -> Result<(StatusCode, Json<Value>), ApiError> {
    Ok((StatusCode::CREATED, Json(create_project_inner(&s, &p, req).await?)))
}

async fn get_project(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    let proj = query::project_for(&s, &p, &id).await?;
    Ok(Json(query::project_json(&s, &proj, p.can_write(&proj.id)).await?))
}

#[derive(Deserialize)]
struct UpdateProject {
    name: Option<String>,
    retention_days: Option<i64>,
    scrub_enabled: Option<bool>,
    scrub_fields: Option<String>,
}

async fn update_project(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Json(req): Json<UpdateProject>) -> J {
    let proj = query::project_for(&s, &p, &id).await?;
    if !p.is_admin() {
        return Err(ApiError::forbidden());
    }
    if let Some(d) = req.retention_days {
        if !(1..=3650).contains(&d) {
            return Err(ApiError::bad("retention_days must be between 1 and 3650"));
        }
    }
    sqlx::query(
        "UPDATE projects SET name = COALESCE($1, name), retention_days = COALESCE($2, retention_days), \
         scrub_enabled = COALESCE($3, scrub_enabled), scrub_fields = COALESCE($4, scrub_fields) WHERE id = $5",
    )
    .bind(req.name)
    .bind(req.retention_days)
    .bind(req.scrub_enabled.map(|b| b as i64))
    .bind(req.scrub_fields)
    .bind(&proj.id)
    .execute(&s.db)
    .await?;
    crate::projects::invalidate_cache(&s).await;
    crate::auth::audit(&s, &p.label(), "update_project", &proj.id, "").await;
    let proj = query::project_for(&s, &p, &proj.id).await?;
    Ok(Json(query::project_json(&s, &proj, true).await?))
}

async fn create_key(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    let proj = query::project_for(&s, &p, &id).await?;
    if !p.is_admin() {
        return Err(ApiError::forbidden());
    }
    let key = crate::projects::create_ingest_key(&s, &proj.id).await?;
    Ok(Json(json!({"ingest_key": key})))
}

async fn revoke_key(State(s): State<AppState>, p: Principal, Path((id, key)): Path<(String, String)>) -> J {
    let proj = query::project_for(&s, &p, &id).await?;
    if !p.is_admin() {
        return Err(ApiError::forbidden());
    }
    sqlx::query("UPDATE ingest_keys SET revoked_at = $1 WHERE key = $2 AND project_id = $3")
        .bind(now_ms())
        .bind(&key)
        .bind(&proj.id)
        .execute(&s.db)
        .await?;
    crate::projects::invalidate_cache(&s).await;
    Ok(Json(json!({"revoked": key})))
}

#[derive(Deserialize, Default)]
struct IssueQuery {
    status: Option<String>,
    query: Option<String>,
    release: Option<String>,
    environment: Option<String>,
    since: Option<String>,
    sort: Option<String>,
    limit: Option<i64>,
}

async fn list_issues(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<IssueQuery>) -> J {
    Ok(Json(
        query::list_issues(
            &s,
            &p,
            IssueListParams { project, status: q.status, query: q.query, release: q.release, environment: q.environment, since: q.since, sort: q.sort, limit: q.limit },
        )
        .await?,
    ))
}

#[derive(Deserialize, Default)]
struct EventQuery {
    query: Option<String>,
    level: Option<String>,
    release: Option<String>,
    environment: Option<String>,
    user_id: Option<String>,
    exception_type: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<i64>,
}

async fn search_events(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<EventQuery>) -> J {
    Ok(Json(
        query::search_events(
            &s,
            &p,
            EventSearchParams {
                project,
                query: q.query,
                level: q.level,
                release: q.release,
                environment: q.environment,
                user_id: q.user_id,
                exception_type: q.exception_type,
                since: q.since,
                until: q.until,
                limit: q.limit,
            },
        )
        .await?,
    ))
}

async fn get_event(State(s): State<AppState>, p: Principal, Path((project, event_id)): Path<(String, String)>) -> J {
    Ok(Json(query::get_event(&s, &p, &project, &event_id).await?))
}

async fn list_releases(State(s): State<AppState>, p: Principal, Path(project): Path<String>) -> J {
    let proj = query::project_for(&s, &p, &project).await?;
    let rows = sqlx::query("SELECT version, created_at FROM releases WHERE project_id = $1 ORDER BY created_at DESC LIMIT 200")
        .bind(&proj.id)
        .fetch_all(&s.db)
        .await?;
    Ok(Json(json!({"project": proj.slug, "releases": rows.iter().map(|r| json!({"version": r.s("version"), "first_seen": iso_time(r.i("created_at"))})).collect::<Vec<_>>()})))
}

#[derive(Deserialize)]
struct HealthQuery {
    release: Option<String>,
    since: Option<String>,
}

async fn release_health(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<HealthQuery>) -> J {
    Ok(Json(query::release_health(&s, &p, &project, q.release.as_deref(), q.since.as_deref()).await?))
}

#[derive(Deserialize)]
struct CompareQuery {
    base: String,
    target: String,
}

async fn compare_releases(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<CompareQuery>) -> J {
    Ok(Json(query::compare_releases(&s, &p, &project, &q.base, &q.target).await?))
}

/// Parse analytics query strings: since, until, type, interval, dimension, limit, and
/// repeated `filter=dimension:value` (prefix the value with `~` for contains, `!` for not-equal).
fn analytics_params(project: String, raw: Option<String>) -> (AnalyticsParams, BTreeMap<String, String>) {
    let mut a = AnalyticsParams { project, ..Default::default() };
    let mut extra = BTreeMap::new();
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "since" => a.since = Some(v.to_string()),
            "until" => a.until = Some(v.to_string()),
            "type" => a.r#type = Some(v.to_string()),
            "filter" => {
                if let Some((d, val)) = v.split_once(':') {
                    a.filters.insert(d.to_string(), val.to_string());
                }
            }
            other => {
                extra.insert(other.to_string(), v.to_string());
            }
        }
    }
    (a, extra)
}

async fn analytics_summary(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let (a, _) = analytics_params(project, raw);
    Ok(Json(query::analytics_summary(&s, &p, a).await?))
}

async fn analytics_timeseries(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let (a, extra) = analytics_params(project, raw);
    Ok(Json(query::analytics_timeseries(&s, &p, a, extra.get("interval").map(|s| s.as_str())).await?))
}

async fn analytics_breakdown(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let (a, extra) = analytics_params(project, raw);
    let dim = extra.get("dimension").cloned().ok_or_else(|| ApiError::bad(format!("`dimension` is required; one of {}", query::DIMENSIONS.join(", "))))?;
    let limit = extra.get("limit").and_then(|l| l.parse().ok());
    Ok(Json(query::analytics_breakdown(&s, &p, a, &dim, limit).await?))
}

// ---------------------------------------------------------------------------
// Product analytics
// ---------------------------------------------------------------------------

fn opt_step(v: Option<&String>) -> QResult<Option<crate::product::Step>> {
    v.filter(|s| !s.is_empty()).map(|s| crate::product::Step::parse(s)).transpose()
}

fn opt_int(extra: &BTreeMap<String, String>, k: &str) -> QResult<Option<i64>> {
    extra.get(k).map(|v| v.parse::<i64>().map_err(|_| ApiError::bad(format!("`{k}` must be a number")))).transpose()
}

/// Funnel: repeated `step=event|dimension:value`, plus `window`, `identity`, `breakdown`, `limit`.
async fn analytics_funnel(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let steps = url::form_urlencoded::parse(raw.as_deref().unwrap_or("").as_bytes())
        .filter(|(k, _)| k == "step")
        .map(|(_, v)| crate::product::Step::parse(&v))
        .collect::<QResult<Vec<_>>>()?;
    let (base, extra) = analytics_params(project, raw);
    let f = crate::product::FunnelParams {
        base,
        steps,
        window: extra.get("window").cloned(),
        identity: extra.get("identity").cloned(),
        breakdown: extra.get("breakdown").cloned(),
        limit: opt_int(&extra, "limit")?,
    };
    Ok(Json(crate::product::funnel(&s, &p, f).await?))
}

/// Retention: `start` and `return` steps (same syntax as funnel steps), `period`, `periods`, `identity`.
async fn analytics_retention(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let (base, extra) = analytics_params(project, raw);
    let r = crate::product::RetentionParams {
        base,
        start: opt_step(extra.get("start"))?,
        return_step: opt_step(extra.get("return"))?,
        period: extra.get("period").cloned(),
        periods: opt_int(&extra, "periods")?,
        identity: extra.get("identity").cloned(),
    };
    Ok(Json(crate::product::retention(&s, &p, r).await?))
}

/// Paths: `from` (a label like `pageview /pricing`), `direction`, `depth`, `identity`, `limit`.
async fn analytics_paths(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let (base, extra) = analytics_params(project, raw);
    let a = crate::product::PathParams {
        base,
        from: extra.get("from").cloned().filter(|f| !f.is_empty()),
        direction: extra.get("direction").cloned(),
        depth: opt_int(&extra, "depth")?,
        identity: extra.get("identity").cloned(),
        limit: opt_int(&extra, "limit")?,
    };
    Ok(Json(crate::product::paths(&s, &p, a).await?))
}

async fn analytics_events(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let (a, extra) = analytics_params(project, raw);
    Ok(Json(crate::product::event_catalog(&s, &p, a, opt_int(&extra, "limit")?).await?))
}

async fn analytics_revenue(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let (base, extra) = analytics_params(project, raw);
    let r = crate::product::RevenueParams {
        base,
        event: extra.get("event").cloned(),
        value_prop: extra.get("value_prop").cloned(),
        currency_prop: extra.get("currency_prop").cloned(),
        breakdown: extra.get("breakdown").cloned(),
        limit: opt_int(&extra, "limit")?,
    };
    Ok(Json(crate::product::revenue(&s, &p, r).await?))
}

#[derive(Deserialize)]
struct RealtimeQuery {
    minutes: Option<i64>,
}

async fn analytics_realtime(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<RealtimeQuery>) -> J {
    Ok(Json(crate::insights::realtime(&s, &p, &project, q.minutes).await?))
}

async fn list_goals(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    let (a, _) = analytics_params(project, raw);
    Ok(Json(crate::product::list_goals(&s, &p, a).await?))
}

async fn create_goal(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Json(g): Json<crate::product::GoalIn>) -> Result<(StatusCode, Json<Value>), ApiError> {
    Ok((StatusCode::CREATED, Json(crate::product::create_goal(&s, &p, &project, g).await?)))
}

async fn list_monitors(State(s): State<AppState>, p: Principal, Path(project): Path<String>) -> J {
    Ok(Json(crate::monitors::list(&s, &p, &project).await?))
}

async fn create_monitor(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Json(m): Json<crate::monitors::MonitorIn>) -> Result<(StatusCode, Json<Value>), ApiError> {
    Ok((StatusCode::CREATED, Json(crate::monitors::create(&s, &p, &project, m).await?)))
}

async fn get_monitor(State(s): State<AppState>, p: Principal, Path((project, slug)): Path<(String, String)>) -> J {
    Ok(Json(crate::monitors::get(&s, &p, &project, &slug).await?))
}

async fn update_monitor(State(s): State<AppState>, p: Principal, Path((project, slug)): Path<(String, String)>, Json(m): Json<crate::monitors::MonitorIn>) -> J {
    Ok(Json(crate::monitors::update(&s, &p, &project, &slug, m).await?))
}

async fn delete_monitor(State(s): State<AppState>, p: Principal, Path((project, slug)): Path<(String, String)>) -> J {
    Ok(Json(crate::monitors::delete(&s, &p, &project, &slug).await?))
}

async fn monitor_checks(State(s): State<AppState>, p: Principal, Path((project, slug)): Path<(String, String)>, Query(q): Query<crate::monitors::ChecksQuery>) -> J {
    Ok(Json(crate::monitors::checks(&s, &p, &project, &slug, q).await?))
}

async fn list_incidents(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<crate::monitors::IncidentQuery>) -> J {
    Ok(Json(crate::monitors::list_incidents(&s, &p, &project, q).await?))
}

async fn get_incident(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    Ok(Json(crate::monitors::get_incident(&s, &p, &id).await?))
}

async fn update_incident(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Json(u): Json<crate::monitors::IncidentUpdate>) -> J {
    Ok(Json(crate::monitors::update_incident(&s, &p, &id, u).await?))
}

async fn list_flags(State(s): State<AppState>, p: Principal, Path(project): Path<String>) -> J {
    Ok(Json(crate::flags::list_flags(&s, &p, &project).await?))
}

async fn create_flag(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Json(f): Json<crate::flags::FlagIn>) -> Result<(StatusCode, Json<Value>), ApiError> {
    Ok((StatusCode::CREATED, Json(crate::flags::create_flag(&s, &p, &project, f).await?)))
}

async fn get_flag(State(s): State<AppState>, p: Principal, Path((project, key)): Path<(String, String)>) -> J {
    Ok(Json(crate::flags::get_flag(&s, &p, &project, &key).await?))
}

async fn search_logs(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(mut q): Query<crate::otel::LogParams>) -> J {
    q.project = project;
    Ok(Json(crate::otel::search_logs(&s, &p, q).await?))
}

#[derive(Deserialize)]
struct AnomalyQuery {
    window: Option<String>,
    frequency: Option<String>,
    narrative: Option<bool>,
}

async fn anomalies(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<AnomalyQuery>) -> J {
    Ok(Json(crate::anomalies::detect(&s, &p, &project, q.window.as_deref()).await?))
}

pub async fn digest_preview_inner(s: &AppState, p: &Principal, project: &str, frequency: Option<&str>, narrative: bool) -> QResult<Value> {
    let proj = query::project_for(s, p, project).await?;
    let sched = crate::alerts::Schedule::parse(&json!({"frequency": frequency.unwrap_or("daily")})).map_err(ApiError::bad)?;
    let mut payload = crate::alerts::build_digest(s, Some(&proj.id), &sched, now_ms(), narrative).await?;
    payload["text"] = json!(crate::alerts::digest_lines(&payload, false).join("\n"));
    Ok(payload)
}

async fn digest_preview(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<AnomalyQuery>) -> J {
    Ok(Json(digest_preview_inner(&s, &p, &project, q.frequency.as_deref(), q.narrative.unwrap_or(true)).await?))
}

async fn update_flag(State(s): State<AppState>, p: Principal, Path((project, key)): Path<(String, String)>, Json(f): Json<crate::flags::FlagIn>) -> J {
    Ok(Json(crate::flags::update_flag(&s, &p, &project, &key, f).await?))
}

async fn delete_flag(State(s): State<AppState>, p: Principal, Path((project, key)): Path<(String, String)>) -> J {
    Ok(Json(crate::flags::delete_flag(&s, &p, &project, &key).await?))
}

#[derive(Deserialize, Default)]
struct FlagQuery {
    since: Option<String>,
    release: Option<String>,
    goal: Option<String>,
    control: Option<String>,
    identity: Option<String>,
}

async fn flag_health(State(s): State<AppState>, p: Principal, Path((project, key)): Path<(String, String)>, Query(q): Query<FlagQuery>) -> J {
    Ok(Json(crate::flags::flag_health(&s, &p, &project, &key, q.since.as_deref(), q.release.as_deref()).await?))
}

async fn flag_results(State(s): State<AppState>, p: Principal, Path((project, key)): Path<(String, String)>, Query(q): Query<FlagQuery>) -> J {
    let a = crate::flags::ResultsParams { project, experiment: None, flag: Some(key), goal: q.goal, control: q.control, since: q.since, identity: q.identity };
    Ok(Json(crate::flags::experiment_results(&s, &p, a).await?))
}

async fn list_experiments(State(s): State<AppState>, p: Principal, Path(project): Path<String>) -> J {
    Ok(Json(crate::flags::list_experiments(&s, &p, &project).await?))
}

async fn create_experiment(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Json(e): Json<crate::flags::ExperimentIn>) -> Result<(StatusCode, Json<Value>), ApiError> {
    Ok((StatusCode::CREATED, Json(crate::flags::create_experiment(&s, &p, &project, e).await?)))
}

async fn experiment_results(State(s): State<AppState>, p: Principal, Path((project, name)): Path<(String, String)>, Query(q): Query<FlagQuery>) -> J {
    let a = crate::flags::ResultsParams { project, experiment: Some(name), flag: None, goal: None, control: q.control, since: q.since, identity: q.identity };
    Ok(Json(crate::flags::experiment_results(&s, &p, a).await?))
}

#[derive(Deserialize)]
struct CohortListQuery {
    sizes: Option<bool>,
}

async fn list_cohorts(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<CohortListQuery>) -> J {
    Ok(Json(crate::cohorts::list_cohorts(&s, &p, &project, q.sizes.unwrap_or(true)).await?))
}

async fn create_cohort(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Json(c): Json<crate::cohorts::CohortIn>) -> Result<(StatusCode, Json<Value>), ApiError> {
    Ok((StatusCode::CREATED, Json(crate::cohorts::create_cohort(&s, &p, &project, c).await?)))
}

async fn get_cohort(State(s): State<AppState>, p: Principal, Path((project, cohort)): Path<(String, String)>, Query(q): Query<LimitQuery>) -> J {
    Ok(Json(crate::cohorts::get_cohort(&s, &p, &project, &cohort, q.limit).await?))
}

async fn delete_cohort(State(s): State<AppState>, p: Principal, Path((project, cohort)): Path<(String, String)>) -> J {
    Ok(Json(crate::cohorts::delete_cohort(&s, &p, &project, &cohort).await?))
}

fn engagement_params(project: String, raw: Option<String>) -> crate::cohorts::EngagementParams {
    let (base, extra) = analytics_params(project, raw);
    crate::cohorts::EngagementParams { base, event: extra.get("event").cloned(), identity: extra.get("identity").cloned(), period: extra.get("period").cloned() }
}

/// Stickiness: DAU/WAU/MAU and days active per week (`event`, `identity`).
async fn analytics_stickiness(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    Ok(Json(crate::cohorts::stickiness(&s, &p, engagement_params(project, raw)).await?))
}

/// Lifecycle: new, returning, resurrecting, dormant per `period` (`event`, `identity`).
async fn analytics_lifecycle(State(s): State<AppState>, p: Principal, Path(project): Path<String>, RawQuery(raw): RawQuery) -> J {
    Ok(Json(crate::cohorts::lifecycle(&s, &p, engagement_params(project, raw)).await?))
}

async fn delete_goal(State(s): State<AppState>, p: Principal, Path((project, goal)): Path<(String, String)>) -> J {
    Ok(Json(crate::product::delete_goal(&s, &p, &project, &goal).await?))
}

#[derive(Deserialize)]
struct AdoptionQuery {
    since: Option<String>,
    interval: Option<String>,
    environment: Option<String>,
    limit: Option<i64>,
}

async fn release_adoption(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<AdoptionQuery>) -> J {
    Ok(Json(
        crate::insights::release_adoption(&s, &p, &project, q.since.as_deref(), q.interval.as_deref(), q.environment.as_deref(), q.limit).await?,
    ))
}

#[derive(Deserialize)]
struct UsersQuery {
    since: Option<String>,
    query: Option<String>,
    limit: Option<i64>,
}

async fn list_identified_users(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<UsersQuery>) -> J {
    Ok(Json(crate::product::list_users(&s, &p, crate::product::UserListParams { project, since: q.since, query: q.query, limit: q.limit }).await?))
}

async fn user_timeline(State(s): State<AppState>, p: Principal, Path((project, user_id)): Path<(String, String)>, Query(q): Query<UsersQuery>) -> J {
    Ok(Json(crate::product::user_timeline(&s, &p, &project, &user_id, q.since.as_deref(), q.limit).await?))
}

#[derive(Deserialize)]
struct SpikeQuery {
    window: Option<String>,
    baseline: Option<String>,
}

async fn explain_spike(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<SpikeQuery>) -> J {
    Ok(Json(query::explain_spike(&s, &p, &project, q.window.as_deref(), q.baseline.as_deref()).await?))
}

async fn get_issue(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    Ok(Json(query::get_issue(&s, &p, &id).await?))
}

async fn update_issue(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Json(u): Json<IssueUpdate>) -> J {
    Ok(Json(query::update_issue(&s, &p, &id, u).await?))
}


async fn issue_events(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Query(q): Query<LimitQuery>) -> J {
    Ok(Json(query::list_issue_events(&s, &p, &id, q.limit).await?))
}

#[derive(Deserialize)]
struct CommentBody {
    body: String,
}

async fn comment_issue(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Json(c): Json<CommentBody>) -> J {
    Ok(Json(query::comment_issue(&s, &p, &id, &c.body).await?))
}

// ---------------------------------------------------------------------------
// Artifacts (debug files)
// ---------------------------------------------------------------------------

pub const ARTIFACT_KINDS: &[&str] = &["sourcemap", "proguard", "native"];

async fn list_artifacts(State(s): State<AppState>, p: Principal, Path(project): Path<String>, Query(q): Query<HealthQuery>) -> J {
    let proj = query::project_for(&s, &p, &project).await?;
    let rows = sqlx::query("SELECT id, release, kind, name, debug_id, size, created_at FROM artifacts WHERE project_id = $1 AND ($2 = '' OR release = $2) ORDER BY created_at DESC LIMIT 500")
        .bind(&proj.id)
        .bind(q.release.unwrap_or_default())
        .fetch_all(&s.db)
        .await?;
    Ok(Json(json!({"artifacts": rows.iter().map(|r| json!({
        "id": r.s("id"), "release": r.s("release"), "kind": r.s("kind"), "name": r.s("name"),
        "debug_id": r.s("debug_id"), "size": r.i("size"), "created_at": iso_time(r.i("created_at")),
    })).collect::<Vec<_>>()})))
}

/// Multipart upload: fields `kind` (sourcemap | proguard | native), `release`,
/// `name` (sourcemaps: URL path of the minified file), then `file`.
async fn upload_artifact(State(s): State<AppState>, p: Principal, Path(project): Path<String>, mut mp: Multipart) -> Result<(StatusCode, Json<Value>), ApiError> {
    let proj = query::project_for(&s, &p, &project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let mut kind = String::new();
    let mut release = String::new();
    let mut name = String::new();
    let mut saved: Option<(String, i64)> = None;
    let dir = format!("{}/{}", s.config.artifact_dir(), proj.id);
    tokio::fs::create_dir_all(&dir).await.map_err(anyhow::Error::from)?;
    while let Some(mut field) = mp.next_field().await.map_err(|e| ApiError::bad(format!("bad multipart body: {e}")))? {
        match field.name().unwrap_or("") {
            "kind" => kind = field.text().await.map_err(|e| ApiError::bad(e.to_string()))?,
            "release" => release = field.text().await.map_err(|e| ApiError::bad(e.to_string()))?,
            "name" => name = field.text().await.map_err(|e| ApiError::bad(e.to_string()))?,
            "file" => {
                if name.is_empty() {
                    name = field.file_name().unwrap_or("upload").to_string();
                }
                let path = format!("{dir}/{}", new_id());
                let mut f = tokio::fs::File::create(&path).await.map_err(anyhow::Error::from)?;
                let mut size = 0i64;
                while let Some(chunk) = field.chunk().await.map_err(|e| ApiError::bad(format!("upload interrupted: {e}")))? {
                    size += chunk.len() as i64;
                    f.write_all(&chunk).await.map_err(anyhow::Error::from)?;
                }
                f.flush().await.map_err(anyhow::Error::from)?;
                saved = Some((path, size));
            }
            _ => {}
        }
    }
    let (path, size) = saved.ok_or_else(|| ApiError::bad("missing `file` field"))?;
    if !ARTIFACT_KINDS.contains(&kind.as_str()) {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(ApiError::bad(format!("`kind` must be one of {} (send it before `file`)", ARTIFACT_KINDS.join(", "))));
    }
    if kind != "native" && release.is_empty() {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(ApiError::bad("`release` is required for sourcemap and proguard uploads"));
    }
    let now = now_ms();
    let mut created = Vec::new();
    match kind.as_str() {
        "native" => {
            let bytes = tokio::fs::read(&path).await.map_err(anyhow::Error::from)?;
            let objects = tokio::task::spawn_blocking(move || crate::symbolicate::inspect_debug_file(bytes))
                .await
                .map_err(|e| anyhow::anyhow!(e))?
                .map_err(|e| ApiError::bad(format!("not a supported debug file (Mach-O/dSYM, ELF, PE/PDB): {e}")))?;
            if objects.is_empty() {
                return Err(ApiError::bad("no objects found in debug file"));
            }
            for (debug_id, arch, has_debug_info) in objects {
                let id = new_id();
                sqlx::query("INSERT INTO artifacts (id, project_id, release, kind, name, debug_id, path, size, created_at) VALUES ($1, $2, $3, 'native', $4, $5, $6, $7, $8)")
                    .bind(&id).bind(&proj.id).bind(&release).bind(&name).bind(&debug_id).bind(&path).bind(size).bind(now)
                    .execute(&s.db).await?;
                created.push(json!({"id": id, "debug_id": debug_id, "arch": arch, "has_debug_info": has_debug_info}));
            }
        }
        _ => {
            if kind == "sourcemap" {
                let bytes = tokio::fs::read(&path).await.map_err(anyhow::Error::from)?;
                if sourcemap::decode_slice(&bytes).is_err() {
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(ApiError::bad("file is not a valid source map"));
                }
                name = crate::symbolicate::sourcemap_key(&name);
            }
            let id = new_id();
            sqlx::query("INSERT INTO artifacts (id, project_id, release, kind, name, debug_id, path, size, created_at) VALUES ($1, $2, $3, $4, $5, '', $6, $7, $8)")
                .bind(&id).bind(&proj.id).bind(&release).bind(&kind).bind(&name).bind(&path).bind(size).bind(now)
                .execute(&s.db).await?;
            created.push(json!({"id": id, "name": name}));
        }
    }
    if let Err(e) = crate::s3::store_artifact(&s, &path).await {
        tracing::error!("storing debug file in S3 failed: {e:#}");
        sqlx::query("DELETE FROM artifacts WHERE path = $1").bind(&path).execute(&s.db).await?;
        let _ = tokio::fs::remove_file(&path).await;
        return Err(ApiError(StatusCode::BAD_GATEWAY, "could not store the file in S3 storage; see the server log".into()));
    }
    if !release.is_empty() {
        sqlx::query("INSERT INTO releases (project_id, version, created_at) VALUES ($1, $2, $3) ON CONFLICT (project_id, version) DO NOTHING")
            .bind(&proj.id).bind(&release).bind(now).execute(&s.db).await?;
    }
    Ok((StatusCode::CREATED, Json(json!({"kind": kind, "release": release, "size": size, "artifacts": created}))))
}

async fn delete_artifact(State(s): State<AppState>, p: Principal, Path((project, id)): Path<(String, String)>) -> J {
    let proj = query::project_for(&s, &p, &project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    let row = sqlx::query("SELECT path FROM artifacts WHERE id = $1 AND project_id = $2").bind(&id).bind(&proj.id).fetch_optional(&s.db).await?;
    let Some(row) = row else { return Err(ApiError::not_found("artifact")) };
    let path = row.s("path");
    sqlx::query("DELETE FROM artifacts WHERE id = $1").bind(&id).execute(&s.db).await?;
    let still_used = sqlx::query("SELECT COUNT(*) AS n FROM artifacts WHERE path = $1").bind(&path).fetch_one(&s.db).await?.i("n");
    if still_used == 0 {
        let _ = tokio::fs::remove_file(&path).await;
        crate::s3::delete_artifact(&s, &path).await;
    }
    Ok(Json(json!({"deleted": id})))
}

async fn erase_user(State(s): State<AppState>, p: Principal, Path((project, user_id)): Path<(String, String)>) -> J {
    let proj = query::project_for(&s, &p, &project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    Ok(Json(json!(crate::maintenance::erase_user(&s, &proj.id, &user_id, &p.label()).await?)))
}

async fn install_instructions(State(s): State<AppState>, p: Principal, Path((project, platform)): Path<(String, String)>) -> J {
    let proj = query::project_for(&s, &p, &project).await?;
    let key = crate::projects::ingest_keys(&s, &proj.id).await?.into_iter().next().unwrap_or_else(|| "<create an ingest key>".into());
    let md = crate::docs::install_markdown(&platform, &s.config.public_url, &key, &proj.slug)
        .ok_or_else(|| ApiError::bad(format!("no instructions for `{platform}`; available: {}", crate::docs::INSTALL_PLATFORMS.join(", "))))?;
    Ok(Json(json!({"platform": platform, "project": proj.slug, "ingest_key": key, "endpoint": s.config.public_url, "markdown": md})))
}

// ---------------------------------------------------------------------------
// Alert rules
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct RuleBody {
    pub project: Option<String>,
    pub name: Option<String>,
    pub trigger: Option<String>,
    pub channel: Option<String>,
    pub target: Option<String>,
    pub secret: Option<String>,
    pub threshold: Option<i64>,
    pub enabled: Option<bool>,
    pub filters: Option<Value>,
    pub max_per_hour: Option<i64>,
    /// Digests: {frequency: daily|weekly, hour, weekday, timezone}.
    pub schedule: Option<Value>,
}

fn schedule_str(trigger: &str, v: &Option<Value>) -> QResult<Option<String>> {
    match v {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(String::new())),
        Some(_) if trigger != "digest" => Err(ApiError::bad("schedule only applies to digest rules")),
        Some(v) => {
            let s = crate::alerts::Schedule::parse(v).map_err(ApiError::bad)?;
            Ok(Some(serde_json::to_string(&s).map_err(anyhow::Error::from)?))
        }
    }
}

pub async fn list_rules_inner(s: &AppState, p: &Principal) -> QResult<Value> {
    let rows = sqlx::query(&format!("SELECT {} FROM alert_rules ORDER BY created_at", crate::alerts::RULE_COLS)).fetch_all(&s.db).await?;
    let rules: Vec<Value> = rows
        .iter()
        .map(crate::alerts::rule_from_row)
        .filter(|r| r.project_id.as_deref().map(|pid| p.can_read(pid)).unwrap_or(p.project_scope().is_none()))
        .map(|r| json!(r))
        .collect();
    Ok(json!({"rules": rules, "triggers": crate::alerts::TRIGGERS, "channels": crate::alerts::CHANNELS, "filters": crate::alerts::FILTER_KEYS}))
}

async fn list_rules(State(s): State<AppState>, p: Principal) -> J {
    Ok(Json(list_rules_inner(&s, &p).await?))
}

fn validate_target(channel: &str, target: &str) -> QResult<()> {
    match channel {
        "webhook" if !(target.starts_with("https://") || target.starts_with("http://")) => Err(ApiError::bad("webhook target must be an http(s) URL")),
        "slack" if !(target.starts_with("https://") || target.starts_with("http://")) => {
            Err(ApiError::bad("slack target must be an incoming webhook URL (https://hooks.slack.com/services/...)"))
        }
        "email" if !target.contains('@') => Err(ApiError::bad("email target must be one or more comma-separated email addresses")),
        "sms" => crate::sms::parse_numbers(target).map(|_| ()).map_err(ApiError::bad),
        "github_dispatch" | "github_issue" if crate::github::repo_slug(target).is_none() => Err(ApiError::bad("GitHub target must be owner/repo or https://github.com/owner/repo")),
        _ => Ok(()),
    }
}

fn validate_filters(f: &Option<Value>) -> QResult<()> {
    match f {
        Some(f) if !f.is_null() => crate::alerts::validate_filters(f).map_err(ApiError::bad),
        _ => Ok(()),
    }
}

pub async fn create_rule_inner(s: &AppState, p: &Principal, b: RuleBody) -> QResult<Value> {
    let project_id = match b.project.as_deref().filter(|x| !x.is_empty()) {
        Some(pr) => Some(query::project_for(s, p, pr).await?.id),
        None => None,
    };
    let allowed = match &project_id {
        Some(pid) => p.can_write(pid),
        None => p.is_admin() || matches!(p, Principal::Token { write: true, project_id: None, .. }),
    };
    if !allowed {
        return Err(ApiError::forbidden());
    }
    let trigger = b.trigger.unwrap_or_else(|| "new_issue".into());
    let channel = b.channel.unwrap_or_else(|| "webhook".into());
    if !crate::alerts::TRIGGERS.contains(&trigger.as_str()) {
        return Err(ApiError::bad(format!("trigger must be one of {}", crate::alerts::TRIGGERS.join(", "))));
    }
    if !crate::alerts::CHANNELS.contains(&channel.as_str()) {
        return Err(ApiError::bad(format!("channel must be one of {}", crate::alerts::CHANNELS.join(", "))));
    }
    let target = b.target.unwrap_or_default();
    validate_target(&channel, &target)?;
    validate_filters(&b.filters)?;
    if trigger == "digest" && !(0..=23).contains(&b.threshold.unwrap_or(8)) {
        return Err(ApiError::bad("for digests, threshold is the UTC hour to send at (0 to 23, default 8)"));
    }
    if channel == "github_issue" && matches!(trigger.as_str(), "monitor_down" | "monitor_up") {
        return Err(ApiError::bad("github_issue works with issue triggers; use github_dispatch or a webhook for monitor alerts"));
    }
    if channel == "github_issue" && matches!(trigger.as_str(), "digest" | "release_created" | "perf_regression") {
        return Err(ApiError::bad("github_issue works with issue triggers; use github_dispatch or a webhook for digests, releases and performance regressions"));
    }
    if trigger == "perf_regression" && !(1..=1000).contains(&b.threshold.unwrap_or(20)) {
        return Err(ApiError::bad("for perf_regression, threshold is the p95 increase in percent (1 to 1000, default 20)"));
    }
    if channel.starts_with("github_") && b.secret.as_deref().unwrap_or("").is_empty() {
        return Err(ApiError::bad("GitHub channels need `secret`: a GitHub token that can write to the repository (contents for dispatch, issues for issues)"));
    }
    let schedule = schedule_str(&trigger, &b.schedule)?.unwrap_or_default();
    let id = new_id();
    sqlx::query(
        "INSERT INTO alert_rules (id, project_id, name, trigger, channel, target, secret, threshold, enabled, created_at, filters, max_per_hour, schedule) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(b.name.unwrap_or_else(|| format!("{trigger} via {channel}")))
    .bind(&trigger)
    .bind(&channel)
    .bind(&target)
    .bind(b.secret.unwrap_or_default())
    .bind(b.threshold.unwrap_or(match trigger.as_str() {
        "spike" => 50,
        "digest" => 8,
        "threshold" => 100,
        "perf_regression" => 20,
        _ => 0,
    }))
    .bind(b.enabled.unwrap_or(true) as i64)
    .bind(now_ms())
    .bind(b.filters.filter(|f| !f.is_null()).map(|f| f.to_string()).unwrap_or_default())
    .bind(b.max_per_hour.unwrap_or(if channel.starts_with("github_") || channel == "sms" { 10 } else { 0 }).max(0))
    .bind(schedule)
    .execute(&s.db)
    .await?;
    crate::auth::audit(s, &p.label(), "create_alert_rule", &id, &trigger).await;
    let row = sqlx::query(&format!("SELECT {} FROM alert_rules WHERE id = $1", crate::alerts::RULE_COLS)).bind(&id).fetch_one(&s.db).await?;
    Ok(json!(crate::alerts::rule_from_row(&row)))
}

async fn create_rule(State(s): State<AppState>, p: Principal, Json(b): Json<RuleBody>) -> Result<(StatusCode, Json<Value>), ApiError> {
    Ok((StatusCode::CREATED, Json(create_rule_inner(&s, &p, b).await?)))
}

async fn rule_for_write(s: &AppState, p: &Principal, id: &str) -> QResult<crate::alerts::Rule> {
    let row = sqlx::query(&format!("SELECT {} FROM alert_rules WHERE id = $1", crate::alerts::RULE_COLS))
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or_else(|| ApiError::not_found("alert rule"))?;
    let rule = crate::alerts::rule_from_row(&row);
    let ok = match &rule.project_id {
        Some(pid) => p.can_write(pid),
        None => p.is_admin() || matches!(p, Principal::Token { write: true, project_id: None, .. }),
    };
    if !ok {
        return Err(ApiError::forbidden());
    }
    Ok(rule)
}

pub async fn update_rule_inner(s: &AppState, p: &Principal, id: &str, b: RuleBody) -> QResult<Value> {
    let rule = rule_for_write(s, p, id).await?;
    if let Some(t) = &b.target {
        validate_target(&rule.channel, t)?;
    }
    validate_filters(&b.filters)?;
    let schedule = schedule_str(&rule.trigger, &b.schedule)?;
    sqlx::query(
        "UPDATE alert_rules SET name = COALESCE($1, name), target = COALESCE($2, target), secret = COALESCE($3, secret), threshold = COALESCE($4, threshold), \
         enabled = COALESCE($5, enabled), filters = COALESCE($6, filters), max_per_hour = COALESCE($7, max_per_hour), schedule = COALESCE($8, schedule) WHERE id = $9",
    )
    .bind(b.name)
    .bind(b.target)
    .bind(b.secret)
    .bind(b.threshold)
    .bind(b.enabled.map(|e| e as i64))
    .bind(b.filters.map(|f| if f.is_null() { String::new() } else { f.to_string() }))
    .bind(b.max_per_hour.map(|m| m.max(0)))
    .bind(schedule)
    .bind(id)
    .execute(&s.db)
    .await?;
    let row = sqlx::query(&format!("SELECT {} FROM alert_rules WHERE id = $1", crate::alerts::RULE_COLS)).bind(id).fetch_one(&s.db).await?;
    Ok(json!(crate::alerts::rule_from_row(&row)))
}

async fn update_rule(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Json(b): Json<RuleBody>) -> J {
    Ok(Json(update_rule_inner(&s, &p, &id, b).await?))
}

pub async fn delete_rule_inner(s: &AppState, p: &Principal, id: &str) -> QResult<Value> {
    rule_for_write(s, p, id).await?;
    sqlx::query("DELETE FROM alert_rules WHERE id = $1").bind(id).execute(&s.db).await?;
    crate::auth::audit(s, &p.label(), "delete_alert_rule", id, "").await;
    Ok(json!({"deleted": id}))
}

async fn delete_rule(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    Ok(Json(delete_rule_inner(&s, &p, &id).await?))
}

pub async fn test_rule_inner(s: &AppState, p: &Principal, id: &str) -> QResult<Value> {
    rule_for_write(s, p, id).await?;
    Ok(match crate::alerts::test_rule(s, id).await {
        Ok(detail) => json!({"ok": true, "detail": detail}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    })
}

async fn test_rule(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    Ok(Json(test_rule_inner(&s, &p, &id).await?))
}

pub async fn deliveries_inner(s: &AppState, p: &Principal, id: &str, include_payload: bool, limit: Option<i64>) -> QResult<Value> {
    rule_for_write(s, p, id).await?;
    let rows = sqlx::query(
        "SELECT id, issue_id, trigger, status, detail, created_at, attempts, next_attempt_at, payload FROM alert_deliveries WHERE rule_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(id)
    .bind(limit.unwrap_or(50).clamp(1, 500))
    .fetch_all(&s.db)
    .await?;
    let deliveries: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = json!({
                "id": r.s("id"), "issue_id": r.s("issue_id"), "trigger": r.s("trigger"), "status": r.s("status"), "detail": r.s("detail"),
                "created_at": iso_time(r.i("created_at")), "attempts": r.i("attempts"), "next_attempt_at": r.oi("next_attempt_at").map(iso_time),
            });
            if include_payload {
                v["payload"] = serde_json::from_str(&r.s("payload")).unwrap_or(Value::Null);
            }
            v
        })
        .collect();
    Ok(json!({"deliveries": deliveries}))
}

#[derive(Deserialize)]
struct DeliveriesQuery {
    payload: Option<bool>,
    limit: Option<i64>,
}

async fn rule_deliveries(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Query(q): Query<DeliveriesQuery>) -> J {
    Ok(Json(deliveries_inner(&s, &p, &id, q.payload.unwrap_or(false), q.limit).await?))
}

pub async fn redeliver_inner(s: &AppState, p: &Principal, id: &str, delivery: &str) -> QResult<Value> {
    rule_for_write(s, p, id).await?;
    crate::alerts::redeliver(s, id, delivery).await.map_err(|e| ApiError::bad(e.to_string()))
}

async fn redeliver(State(s): State<AppState>, p: Principal, Path((id, delivery)): Path<(String, String)>) -> J {
    Ok(Json(redeliver_inner(&s, &p, &id, &delivery).await?))
}

// ---------------------------------------------------------------------------
// Tokens and users (admin only)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Read-only maintenance mode (metadata database migration)
// ---------------------------------------------------------------------------

/// Admins, and write tokens for all projects (so an operator script can drive the cutover).
fn require_operator(p: &Principal) -> QResult<()> {
    if p.is_admin() || matches!(p, Principal::Token { write: true, project_id: None, .. }) {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

async fn get_read_only(p: Principal) -> J {
    require_operator(&p)?;
    Ok(Json(crate::readonly::status_json()))
}

#[derive(Deserialize)]
struct ReadOnlyBody {
    enabled: bool,
    /// Seconds to wait for queued error events to be stored before switching (default 30).
    drain_seconds: Option<u64>,
}

async fn put_read_only(State(s): State<AppState>, p: Principal, Json(b): Json<ReadOnlyBody>) -> J {
    require_operator(&p)?;
    let mut out = if b.enabled {
        let drain = std::time::Duration::from_secs(b.drain_seconds.unwrap_or(30).min(600));
        let drained = crate::readonly::enable(&s.db, drain).await?;
        tracing::warn!(actor = %p.label(), drained, "read-only mode enabled");
        let mut v = crate::readonly::status_json();
        v["drained"] = json!(drained);
        v
    } else {
        crate::readonly::disable(&s.db).await?;
        crate::auth::audit(&s, &p.label(), "read_only.disable", "", "").await;
        crate::readonly::status_json()
    };
    out["message"] = json!(if b.enabled { crate::readonly::MESSAGE } else { "metadata writes are enabled" });
    Ok(Json(out))
}

fn require_admin(p: &Principal) -> QResult<()> {
    if p.is_admin() { Ok(()) } else { Err(ApiError::forbidden()) }
}

async fn list_tokens(State(s): State<AppState>, p: Principal) -> J {
    require_admin(&p)?;
    let rows = sqlx::query("SELECT t.id, t.name, t.token_prefix, t.project_id, t.scope, t.created_at, t.expires_at, t.last_used_at, p.slug FROM api_tokens t LEFT JOIN projects p ON p.id = t.project_id WHERE t.revoked_at IS NULL ORDER BY t.created_at DESC")
        .fetch_all(&s.db)
        .await?;
    Ok(Json(json!({"tokens": rows.iter().map(|r| json!({
        "id": r.s("id"), "name": r.s("name"), "prefix": r.s("token_prefix"), "project": r.os("slug"), "scope": r.s("scope"),
        "created_at": iso_time(r.i("created_at")), "expires_at": r.oi("expires_at").map(iso_time), "last_used_at": r.oi("last_used_at").map(iso_time),
    })).collect::<Vec<_>>()})))
}

#[derive(Deserialize)]
struct TokenBody {
    name: String,
    project: Option<String>,
    scope: Option<String>,
    expires_in_days: Option<i64>,
}

async fn create_token(State(s): State<AppState>, p: Principal, Json(b): Json<TokenBody>) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_admin(&p)?;
    let project_id = match b.project.as_deref().filter(|x| !x.is_empty()) {
        Some(pr) => Some(query::project_for(&s, &p, pr).await?.id),
        None => None,
    };
    let write = match b.scope.as_deref().unwrap_or("read") {
        "read" => false,
        "write" => true,
        other => return Err(ApiError::bad(format!("scope must be read or write, not `{other}`"))),
    };
    let expires = b.expires_in_days.map(|d| now_ms() + d * 86_400_000);
    let t = crate::auth::create_api_token(&s, &b.name, project_id.as_deref(), write, expires, &p.label()).await?;
    crate::auth::audit(&s, &p.label(), "create_token", &t.id, &b.name).await;
    Ok((StatusCode::CREATED, Json(json!({"id": t.id, "token": t.token, "note": "Store this token now; it is not shown again."}))))
}

async fn revoke_token(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    require_admin(&p)?;
    sqlx::query("UPDATE api_tokens SET revoked_at = $1 WHERE id = $2").bind(now_ms()).bind(&id).execute(&s.db).await?;
    crate::auth::audit(&s, &p.label(), "revoke_token", &id, "").await;
    Ok(Json(json!({"revoked": id})))
}

async fn list_users(State(s): State<AppState>, p: Principal) -> J {
    require_admin(&p)?;
    let rows = sqlx::query("SELECT id, email, name, role, created_at, disabled FROM users ORDER BY created_at").fetch_all(&s.db).await?;
    Ok(Json(json!({"users": rows.iter().map(|r| json!({"id": r.s("id"), "email": r.s("email"), "name": r.s("name"), "role": r.s("role"), "disabled": r.i("disabled") != 0, "created_at": iso_time(r.i("created_at"))})).collect::<Vec<_>>()})))
}

#[derive(Deserialize)]
pub struct InviteBody {
    pub email: String,
    pub role: String,
}

pub async fn invite_inner(s: &AppState, p: &Principal, b: InviteBody) -> QResult<Value> {
    require_admin(p)?;
    let role = Role::parse(&b.role).ok_or_else(|| ApiError::bad("role must be viewer, member, admin or owner"))?;
    if let Principal::User { role: my, .. } = p {
        if role > *my {
            return Err(ApiError::bad("you cannot invite someone with a higher role than your own"));
        }
    }
    let token = crate::util::random_token(24);
    let now = now_ms();
    sqlx::query("INSERT INTO invites (token_hash, email, role, created_by, created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(crate::util::sha256_hex(&token))
        .bind(b.email.trim().to_lowercase())
        .bind(role.as_str())
        .bind(p.label())
        .bind(now)
        .bind(now + 7 * 86_400_000)
        .execute(&s.db)
        .await?;
    let link = format!("{}/ui/invite/{token}", s.config.public_url);
    let emailed = crate::admin::send_invite_email(s, &b.email, &link).await;
    crate::auth::audit(s, &p.label(), "invite_user", &b.email, role.as_str()).await;
    Ok(json!({"email": b.email, "role": role, "invite_link": link, "emailed": emailed, "expires_in_days": 7}))
}

async fn invite_user(State(s): State<AppState>, p: Principal, Json(b): Json<InviteBody>) -> Result<(StatusCode, Json<Value>), ApiError> {
    Ok((StatusCode::CREATED, Json(invite_inner(&s, &p, b).await?)))
}

#[derive(Deserialize)]
struct UserUpdate {
    role: Option<String>,
    disabled: Option<bool>,
}

async fn update_user(State(s): State<AppState>, p: Principal, Path(id): Path<String>, Json(b): Json<UserUpdate>) -> J {
    require_admin(&p)?;
    if let Some(r) = &b.role {
        let role = Role::parse(r).ok_or_else(|| ApiError::bad("role must be viewer, member, admin or owner"))?;
        if let Principal::User { role: my, .. } = &p {
            if role > *my {
                return Err(ApiError::bad("you cannot grant a higher role than your own"));
            }
        }
    }
    ensure_not_last_owner(&s, &id, b.role.as_deref(), b.disabled).await?;
    sqlx::query("UPDATE users SET role = COALESCE($1, role), disabled = COALESCE($2, disabled) WHERE id = $3")
        .bind(b.role)
        .bind(b.disabled.map(|d| d as i64))
        .bind(&id)
        .execute(&s.db)
        .await?;
    crate::auth::audit(&s, &p.label(), "update_user", &id, "").await;
    Ok(Json(json!({"updated": id})))
}

async fn ensure_not_last_owner(s: &AppState, user_id: &str, new_role: Option<&str>, disabled: Option<bool>) -> QResult<()> {
    let row = sqlx::query("SELECT role FROM users WHERE id = $1").bind(user_id).fetch_optional(&s.db).await?.ok_or_else(|| ApiError::not_found("user"))?;
    let demoting = row.s("role") == "owner" && (new_role.map(|r| r != "owner").unwrap_or(false) || disabled == Some(true));
    if demoting {
        let owners = sqlx::query("SELECT COUNT(*) AS n FROM users WHERE role = 'owner' AND disabled = 0").fetch_one(&s.db).await?.i("n");
        if owners <= 1 {
            return Err(ApiError::bad("cannot remove the last owner"));
        }
    }
    Ok(())
}

async fn delete_user(State(s): State<AppState>, p: Principal, Path(id): Path<String>) -> J {
    require_admin(&p)?;
    ensure_not_last_owner(&s, &id, Some("removed"), Some(true)).await?;
    sqlx::query("DELETE FROM web_sessions WHERE user_id = $1").bind(&id).execute(&s.db).await?;
    sqlx::query("UPDATE issues SET assignee_id = NULL WHERE assignee_id = $1").bind(&id).execute(&s.db).await?;
    sqlx::query("DELETE FROM users WHERE id = $1").bind(&id).execute(&s.db).await?;
    crate::auth::audit(&s, &p.label(), "delete_user", &id, "").await;
    Ok(Json(json!({"deleted": id})))
}
