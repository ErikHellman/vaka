//! The shared query layer. Every read and write that the REST API and the MCP
//! server expose is implemented here once, returning JSON values, so both
//! interfaces stay identical.

use crate::auth::{ApiError, Principal};
use crate::db::RowExt;
use crate::state::{AppState, ProjectInfo};
use crate::util::{iso_time, now_ms, parse_time_expr};
use serde::Deserialize;
use serde_json::{json, Value};

pub type QResult<T> = Result<T, ApiError>;

fn iso_opt(v: Option<i64>) -> Value {
    v.map(|t| Value::String(iso_time(t))).unwrap_or(Value::Null)
}

pub fn short_id(project_slug: &str, n: i64) -> String {
    format!("{}-{n}", project_slug.to_uppercase())
}

/// Resolve a project the principal may read.
pub async fn project_for(state: &AppState, p: &Principal, id_or_slug: &str) -> QResult<ProjectInfo> {
    let project = crate::projects::find(state, id_or_slug).await?.ok_or_else(|| ApiError::not_found("project"))?;
    if !p.can_read(&project.id) {
        return Err(ApiError::not_found("project"));
    }
    Ok(project)
}

pub async fn list_projects(state: &AppState, p: &Principal) -> QResult<Value> {
    let mut out = Vec::new();
    for proj in crate::projects::list(state).await? {
        if !p.can_read(&proj.id) {
            continue;
        }
        out.push(project_json(state, &proj, p.is_admin() || p.can_write(&proj.id)).await?);
    }
    Ok(json!({ "projects": out }))
}

pub async fn project_json(state: &AppState, proj: &ProjectInfo, include_keys: bool) -> QResult<Value> {
    let mut v = json!({
        "id": proj.id,
        "slug": proj.slug,
        "name": proj.name,
        "platform": proj.platform,
        "retention_days": proj.retention_days,
        "scrub_enabled": proj.scrub_enabled,
        "scrub_fields": proj.scrub_fields,
    });
    if include_keys {
        v["ingest_keys"] = json!(crate::projects::ingest_keys(state, &proj.id).await?);
    }
    Ok(v)
}

const ISSUE_COLS: &str = "i.id, i.short_id, i.project_id, i.fingerprint, i.title, i.culprit, i.level, i.platform, i.status, i.assignee_id, \
    i.first_seen, i.last_seen, i.times_seen, i.first_release, i.last_release, i.resolved_at, i.resolved_in_release, i.regressed_at, \
    i.ignored_until, i.owners, p.slug AS project_slug, u.email AS assignee_email";

fn issue_json(state: &AppState, r: &sqlx::any::AnyRow) -> Value {
    let slug = r.s("project_slug");
    let id = r.s("id");
    json!({
        "id": id,
        "short_id": short_id(&slug, r.i("short_id")),
        "project_id": r.s("project_id"),
        "project": slug,
        "title": r.s("title"),
        "culprit": r.s("culprit"),
        "level": r.s("level"),
        "platform": r.s("platform"),
        "status": r.s("status"),
        "assignee": r.os("assignee_email"),
        "owners": r.s("owners").split_whitespace().collect::<Vec<_>>(),
        "first_seen": iso_time(r.i("first_seen")),
        "last_seen": iso_time(r.i("last_seen")),
        "times_seen": r.i("times_seen"),
        "first_release": r.s("first_release"),
        "last_release": r.s("last_release"),
        "resolved_at": iso_opt(r.oi("resolved_at")),
        "resolved_in_release": r.os("resolved_in_release"),
        "regressed_at": iso_opt(r.oi("regressed_at")),
        "ignored_until": iso_opt(r.oi("ignored_until").filter(|v| *v > 0)),
        "url": format!("{}/ui/projects/{}/issues/{}", state.config.public_url, slug, id),
    })
}

fn issue_select() -> String {
    format!("SELECT {ISSUE_COLS} FROM issues i JOIN projects p ON p.id = i.project_id LEFT JOIN users u ON u.id = i.assignee_id")
}

/// Fetch one issue by id or short id ("WEB-12") without permission checks.
pub async fn get_issue_row(state: &AppState, id: &str) -> anyhow::Result<Option<Value>> {
    let row = if let Some((slug, n)) = id.rsplit_once('-').and_then(|(s, n)| Some((s.to_lowercase(), n.parse::<i64>().ok()?))) {
        sqlx::query(&format!("{} WHERE (p.slug = $1 AND i.short_id = $2) OR i.id = $3", issue_select()))
            .bind(slug)
            .bind(n)
            .bind(id)
            .fetch_optional(&state.db)
            .await?
    } else {
        sqlx::query(&format!("{} WHERE i.id = $1", issue_select())).bind(id).fetch_optional(&state.db).await?
    };
    Ok(row.map(|r| issue_json(state, &r)))
}

async fn issue_for(state: &AppState, p: &Principal, id: &str) -> QResult<Value> {
    let issue = get_issue_row(state, id).await?.ok_or_else(|| ApiError::not_found("issue"))?;
    if !p.can_read(issue["project_id"].as_str().unwrap_or("")) {
        return Err(ApiError::not_found("issue"));
    }
    Ok(issue)
}

#[derive(Debug, Default, Deserialize)]
pub struct IssueListParams {
    pub project: String,
    /// open (default), resolved, ignored, all
    pub status: Option<String>,
    /// Text search in title and culprit.
    pub query: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    /// Time window for event counts, e.g. "24h", "7d" (default 24h).
    pub since: Option<String>,
    /// last_seen (default), first_seen, times_seen, events (in window), users (in window)
    pub sort: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_issues(state: &AppState, p: &Principal, params: IssueListParams) -> QResult<Value> {
    let project = project_for(state, p, &params.project).await?;
    let now = now_ms();
    let since = params.since.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 86_400_000);
    let limit = params.limit.unwrap_or(25).clamp(1, 200);
    let status = params.status.clone().unwrap_or_else(|| "open".into());
    let sort = params.sort.clone().unwrap_or_else(|| "last_seen".into());

    // Event counts in the window from ClickHouse.
    let mut conds = vec!["project_id = {p:String}".to_string(), "timestamp >= fromUnixTimestamp64Milli({since:Int64})".to_string()];
    let mut ch_params = vec![("p", project.id.clone()), ("since", since.to_string())];
    if let Some(r) = params.release.as_ref().filter(|r| !r.is_empty()) {
        conds.push("release = {release:String}".into());
        ch_params.push(("release", r.clone()));
    }
    if let Some(e) = params.environment.as_ref().filter(|e| !e.is_empty()) {
        conds.push("environment = {env:String}".into());
        ch_params.push(("env", e.clone()));
    }
    let counts = state
        .ch
        .query(
            &format!(
                "SELECT issue_id, count() AS events, uniqIf(if(user_id != '', user_id, anon_id), user_id != '' OR anon_id != '') AS users \
                 FROM errors WHERE {} GROUP BY issue_id",
                conds.join(" AND ")
            ),
            &ch_params,
        )
        .await?;
    let count_map: std::collections::HashMap<String, (i64, i64)> = counts
        .iter()
        .map(|r| (r["issue_id"].as_str().unwrap_or("").to_string(), (r["events"].as_i64().unwrap_or(0), r["users"].as_i64().unwrap_or(0))))
        .collect();

    let mut sql = format!("{} WHERE i.project_id = $1", issue_select());
    if status != "all" {
        sql.push_str(" AND i.status = $2");
    } else {
        sql.push_str(" AND $2 = $2");
    }
    let q = params.query.clone().unwrap_or_default();
    if !q.is_empty() {
        sql.push_str(" AND (LOWER(i.title) LIKE $3 OR LOWER(i.culprit) LIKE $3)");
    } else {
        sql.push_str(" AND $3 = $3");
    }
    let windowed = params.release.is_some() || params.environment.is_some();
    sql.push_str(match sort.as_str() {
        "first_seen" => " ORDER BY i.first_seen DESC",
        "times_seen" => " ORDER BY i.times_seen DESC",
        _ => " ORDER BY i.last_seen DESC",
    });
    sql.push_str(" LIMIT 1000");
    let rows = sqlx::query(&sql)
        .bind(&project.id)
        .bind(&status)
        .bind(format!("%{}%", q.to_lowercase()))
        .fetch_all(&state.db)
        .await?;
    let mut issues: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = issue_json(state, r);
            let (events, users) = count_map.get(&r.s("id")).copied().unwrap_or((0, 0));
            v["events_in_window"] = json!(events);
            v["users_in_window"] = json!(users);
            v
        })
        .filter(|v| !windowed || v["events_in_window"].as_i64().unwrap_or(0) > 0)
        .collect();
    match sort.as_str() {
        "events" => issues.sort_by_key(|v| -v["events_in_window"].as_i64().unwrap_or(0)),
        "users" => issues.sort_by_key(|v| -v["users_in_window"].as_i64().unwrap_or(0)),
        _ => {}
    }
    let total = issues.len();
    issues.truncate(limit as usize);
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(since), "until": iso_time(now)},
        "total_matching": total,
        "issues": issues,
    }))
}

const EVENT_COLS: &str = "toString(event_id) AS event_id, issue_id, toUnixTimestamp64Milli(timestamp) AS ts, platform, level, release, environment, \
    exception_type, exception_value, message, culprit, handled, fatal, exceptions, breadcrumbs, tags, contexts, debug_images, user_id, anon_id, \
    session_id, device_model, os_name, os_version, browser, country, url, sdk_name, sdk_version, symbolicated";

fn parse_json_str(v: &Value) -> Value {
    v.as_str().and_then(|s| if s.is_empty() { None } else { serde_json::from_str(s).ok() }).unwrap_or(Value::Null)
}

fn event_json(r: &Value, full: bool) -> Value {
    let mut v = json!({
        "event_id": r["event_id"],
        "issue_id": r["issue_id"],
        "timestamp": iso_time(r["ts"].as_i64().unwrap_or(0)),
        "platform": r["platform"],
        "level": r["level"],
        "release": r["release"],
        "environment": r["environment"],
        "exception_type": r["exception_type"],
        "exception_value": r["exception_value"],
        "message": r["message"],
        "culprit": r["culprit"],
        "handled": r["handled"].as_u64() == Some(1),
        "fatal": r["fatal"].as_u64() == Some(1),
        "user_id": r["user_id"],
        "device": {"model": r["device_model"], "os_name": r["os_name"], "os_version": r["os_version"], "browser": r["browser"]},
        "country": r["country"],
        "url": r["url"],
        "tags": r["tags"],
    });
    if full {
        v["exceptions"] = parse_json_str(&r["exceptions"]);
        v["breadcrumbs"] = parse_json_str(&r["breadcrumbs"]);
        v["contexts"] = parse_json_str(&r["contexts"]);
        v["sdk"] = json!({"name": r["sdk_name"], "version": r["sdk_version"]});
        v["session_id"] = r["session_id"].clone();
        v["symbolicated"] = json!(r["symbolicated"].as_u64() == Some(1));
        v["debug_images"] = parse_json_str(&r["debug_images"]);
    }
    v
}

/// Re-symbolicate a stored event on read when its debug files arrived later.
async fn late_symbolicate(state: &AppState, project_id: &str, ev: &mut Value) {
    if ev["symbolicated"] == json!(true) {
        return;
    }
    let Ok(mut exceptions) = serde_json::from_value::<Vec<crate::models::ExceptionData>>(ev["exceptions"].clone()) else { return };
    let images: Vec<crate::models::DebugImage> = serde_json::from_value(ev["debug_images"].clone()).unwrap_or_default();
    let release = ev["release"].as_str().unwrap_or("").to_string();
    let platform = ev["platform"].as_str().unwrap_or("").to_string();
    if crate::symbolicate::symbolicate(state, project_id, &release, &platform, &mut exceptions, &images).await {
        ev["exceptions"] = json!(exceptions);
        ev["symbolicated"] = json!(true);
        ev["symbolicated_on_read"] = json!(true);
    }
}

pub async fn get_issue(state: &AppState, p: &Principal, id: &str) -> QResult<Value> {
    let mut issue = issue_for(state, p, id).await?;
    let issue_id = issue["id"].as_str().unwrap_or("").to_string();
    let project_id = issue["project_id"].as_str().unwrap_or("").to_string();
    let params = [("p", project_id.clone()), ("i", issue_id.clone())];
    let latest = state
        .ch
        .query(&format!("SELECT {EVENT_COLS} FROM errors WHERE project_id = {{p:String}} AND issue_id = {{i:String}} ORDER BY timestamp DESC LIMIT 1"), &params)
        .await?;
    let mut latest_event = latest.first().map(|r| event_json(r, true)).unwrap_or(Value::Null);
    if !latest_event.is_null() {
        late_symbolicate(state, &project_id, &mut latest_event).await;
    }
    let stats = state
        .ch
        .query(
            "SELECT count() AS events, uniqIf(if(user_id != '', user_id, anon_id), user_id != '' OR anon_id != '') AS users, \
             countIf(timestamp > now64(3) - INTERVAL 24 HOUR) AS events_24h \
             FROM errors WHERE project_id = {p:String} AND issue_id = {i:String}",
            &params,
        )
        .await?;
    let by = |col: &str| {
        format!(
            "SELECT {col} AS value, count() AS events FROM errors WHERE project_id = {{p:String}} AND issue_id = {{i:String}} \
             GROUP BY value ORDER BY events DESC LIMIT 10"
        )
    };
    let releases = state.ch.query(&by("release"), &params).await?;
    let environments = state.ch.query(&by("environment"), &params).await?;
    let os = state.ch.query(&by("concat(os_name, ' ', os_version)"), &params).await?;
    let devices = state.ch.query(&by("if(device_model != '', device_model, browser)"), &params).await?;
    let daily = state
        .ch
        .query(
            "SELECT toString(toDate(timestamp)) AS day, count() AS events FROM errors WHERE project_id = {p:String} AND issue_id = {i:String} \
             AND timestamp > now64(3) - INTERVAL 14 DAY GROUP BY day ORDER BY day",
            &params,
        )
        .await?;
    let comments = sqlx::query("SELECT author, body, created_at FROM issue_comments WHERE issue_id = $1 ORDER BY created_at")
        .bind(&issue_id)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| json!({"author": r.s("author"), "body": r.s("body"), "created_at": iso_time(r.i("created_at"))}))
        .collect::<Vec<_>>();
    issue["stats"] = stats.first().cloned().unwrap_or(json!({}));
    issue["by_release"] = json!(releases);
    issue["by_environment"] = json!(environments);
    issue["by_os"] = json!(os);
    issue["by_device"] = json!(devices);
    issue["daily_last_14d"] = json!(daily);
    issue["comments"] = json!(comments);
    let first_release = issue["first_release"].as_str().unwrap_or("").to_string();
    issue["suspect_commits"] = if latest_event.is_null() {
        json!([])
    } else {
        crate::ownership::suspect_commits(state, &project_id, &first_release, &latest_event["exceptions"]).await.unwrap_or_else(|e| {
            tracing::warn!("suspect commits failed: {e:#}");
            json!([])
        })
    };
    issue["latest_event"] = latest_event;
    Ok(issue)
}

pub async fn list_issue_events(state: &AppState, p: &Principal, id: &str, limit: Option<i64>) -> QResult<Value> {
    let issue = issue_for(state, p, id).await?;
    let rows = state
        .ch
        .query(
            &format!("SELECT {EVENT_COLS} FROM errors WHERE project_id = {{p:String}} AND issue_id = {{i:String}} ORDER BY timestamp DESC LIMIT {{l:UInt32}}"),
            &[
                ("p", issue["project_id"].as_str().unwrap_or("").to_string()),
                ("i", issue["id"].as_str().unwrap_or("").to_string()),
                ("l", limit.unwrap_or(20).clamp(1, 100).to_string()),
            ],
        )
        .await?;
    Ok(json!({"issue": issue["short_id"], "events": rows.iter().map(|r| event_json(r, false)).collect::<Vec<_>>()}))
}

pub async fn get_event(state: &AppState, p: &Principal, project: &str, event_id: &str) -> QResult<Value> {
    let project = project_for(state, p, project).await?;
    let rows = state
        .ch
        .query(
            &format!("SELECT {EVENT_COLS} FROM errors WHERE project_id = {{p:String}} AND event_id = toString(toUUIDOrZero({{e:String}})) LIMIT 1"),
            &[("p", project.id.clone()), ("e", event_id.to_string())],
        )
        .await?;
    let mut ev = rows.first().map(|r| event_json(r, true)).ok_or_else(|| ApiError::not_found("event"))?;
    late_symbolicate(state, &project.id, &mut ev).await;
    Ok(ev)
}

#[derive(Debug, Default, Deserialize)]
pub struct EventSearchParams {
    pub project: String,
    pub query: Option<String>,
    pub level: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub user_id: Option<String>,
    pub exception_type: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<i64>,
}

pub async fn search_events(state: &AppState, p: &Principal, params: EventSearchParams) -> QResult<Value> {
    let project = project_for(state, p, &params.project).await?;
    let now = now_ms();
    let since = params.since.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 7 * 86_400_000);
    let until = params.until.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now);
    let mut conds = vec![
        "project_id = {p:String}".to_string(),
        "timestamp >= fromUnixTimestamp64Milli({since:Int64})".to_string(),
        "timestamp <= fromUnixTimestamp64Milli({until:Int64})".to_string(),
    ];
    let mut ps = vec![("p", project.id.clone()), ("since", since.to_string()), ("until", until.to_string())];
    let mut add = |name: &'static str, col: &str, v: &Option<String>| {
        if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
            conds.push(format!("{col} = {{{name}:String}}"));
            ps.push((name, v.clone()));
        }
    };
    add("level", "level", &params.level);
    add("release", "release", &params.release);
    add("env", "environment", &params.environment);
    add("uid", "user_id", &params.user_id);
    add("etype", "exception_type", &params.exception_type);
    if let Some(q) = params.query.as_ref().filter(|q| !q.is_empty()) {
        conds.push("(positionCaseInsensitive(exception_value, {q:String}) > 0 OR positionCaseInsensitive(message, {q:String}) > 0 OR positionCaseInsensitive(exception_type, {q:String}) > 0 OR positionCaseInsensitive(culprit, {q:String}) > 0)".into());
        ps.push(("q", q.clone()));
    }
    ps.push(("l", params.limit.unwrap_or(50).clamp(1, 500).to_string()));
    let rows = state
        .ch
        .query(&format!("SELECT {EVENT_COLS} FROM errors WHERE {} ORDER BY timestamp DESC LIMIT {{l:UInt32}}", conds.join(" AND ")), &ps)
        .await?;
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(since), "until": iso_time(until)},
        "events": rows.iter().map(|r| event_json(r, false)).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Default, Deserialize)]
pub struct IssueUpdate {
    /// open | resolved | ignored
    pub status: Option<String>,
    /// Email of a user, or "" to unassign.
    pub assignee: Option<String>,
    /// For status=resolved: the release that contains the fix.
    pub resolved_in_release: Option<String>,
    /// For status=ignored: ignore for this long ("24h", "7d"); omit to ignore forever.
    pub ignore_for: Option<String>,
}

pub async fn update_issue(state: &AppState, p: &Principal, id: &str, upd: IssueUpdate) -> QResult<Value> {
    let issue = issue_for(state, p, id).await?;
    let issue_id = issue["id"].as_str().unwrap_or("").to_string();
    if !p.can_write(issue["project_id"].as_str().unwrap_or("")) {
        return Err(ApiError::forbidden());
    }
    let now = now_ms();
    if let Some(status) = &upd.status {
        match status.as_str() {
            "open" => {
                sqlx::query("UPDATE issues SET status = 'open', resolved_at = NULL, resolved_in_release = NULL, ignored_until = NULL WHERE id = $1")
                    .bind(&issue_id).execute(&state.db).await?;
            }
            "resolved" => {
                sqlx::query("UPDATE issues SET status = 'resolved', resolved_at = $1, resolved_in_release = $2, ignored_until = NULL WHERE id = $3")
                    .bind(now).bind(upd.resolved_in_release.clone()).bind(&issue_id).execute(&state.db).await?;
            }
            "ignored" => {
                let until = upd.ignore_for.as_deref().and_then(|d| parse_time_expr(d, 0)).map(|neg| now - neg);
                sqlx::query("UPDATE issues SET status = 'ignored', ignored_until = $1 WHERE id = $2")
                    .bind(until).bind(&issue_id).execute(&state.db).await?;
            }
            other => return Err(ApiError::bad(format!("unknown status `{other}`: use open, resolved or ignored"))),
        }
    }
    if let Some(assignee) = &upd.assignee {
        let user_id = if assignee.is_empty() {
            None
        } else {
            let row = sqlx::query("SELECT id FROM users WHERE email = $1").bind(assignee.to_lowercase()).fetch_optional(&state.db).await?;
            Some(row.ok_or_else(|| ApiError::bad(format!("no user with email {assignee}")))?.s("id"))
        };
        sqlx::query("UPDATE issues SET assignee_id = $1 WHERE id = $2").bind(user_id).bind(&issue_id).execute(&state.db).await?;
    }
    crate::auth::audit(state, &p.label(), "update_issue", &issue_id, &serde_json::to_string(&json!({"status": upd.status, "assignee": upd.assignee})).unwrap_or_default()).await;
    Ok(get_issue_row(state, &issue_id).await?.unwrap_or(Value::Null))
}

pub async fn comment_issue(state: &AppState, p: &Principal, id: &str, body: &str) -> QResult<Value> {
    let issue = issue_for(state, p, id).await?;
    if !p.can_write(issue["project_id"].as_str().unwrap_or("")) {
        return Err(ApiError::forbidden());
    }
    if body.trim().is_empty() {
        return Err(ApiError::bad("comment body is empty"));
    }
    sqlx::query("INSERT INTO issue_comments (id, issue_id, author, body, created_at) VALUES ($1, $2, $3, $4, $5)")
        .bind(crate::util::new_id())
        .bind(issue["id"].as_str().unwrap_or(""))
        .bind(p.label())
        .bind(body)
        .bind(now_ms())
        .execute(&state.db)
        .await?;
    Ok(json!({"ok": true, "issue": issue["short_id"]}))
}

fn health_sql(group: &str, cond: &str) -> String {
    format!(
        "SELECT {group} AS release, count() AS sessions, countIf(s_status = 'crashed') AS crashed, countIf(s_status = 'errored') AS errored, \
         countIf(s_status = 'abnormal') AS abnormal, uniq(s_distinct) AS users, uniqIf(s_distinct, s_status = 'crashed') AS crashed_users, \
         round(1 - crashed / greatest(sessions, 1), 4) AS crash_free_sessions, round(1 - crashed_users / greatest(users, 1), 4) AS crash_free_users, \
         round(avg(s_duration) / 1000, 1) AS avg_session_seconds, toString(min(s_started)) AS first_session, toString(max(s_last)) AS last_session \
         FROM (SELECT session_id, argMax(status, timestamp) AS s_status, any(distinct_id) AS s_distinct, any(release) AS s_release, \
               max(duration_ms) AS s_duration, min(started) AS s_started, max(timestamp) AS s_last \
               FROM sessions WHERE {cond} GROUP BY session_id) GROUP BY s_release ORDER BY last_session DESC"
    )
}

pub async fn release_health(state: &AppState, p: &Principal, project: &str, release: Option<&str>, since: Option<&str>) -> QResult<Value> {
    let project = project_for(state, p, project).await?;
    let now = now_ms();
    let since = since.and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 14 * 86_400_000);
    let mut cond = "project_id = {p:String} AND started >= fromUnixTimestamp64Milli({since:Int64})".to_string();
    let mut ps = vec![("p", project.id.clone()), ("since", since.to_string())];
    if let Some(r) = release.filter(|r| !r.is_empty()) {
        cond.push_str(" AND release = {r:String}");
        ps.push(("r", r.to_string()));
    }
    let rows = state.ch.query(&format!("{} LIMIT 50", health_sql("s_release", &cond)), &ps).await?;
    // Error counts and new issues per release.
    let mut econd = "project_id = {p:String} AND timestamp >= fromUnixTimestamp64Milli({since:Int64})".to_string();
    if release.is_some() {
        econd.push_str(" AND release = {r:String}");
    }
    let errs = state
        .ch
        .query(&format!("SELECT release, count() AS errors, uniq(issue_id) AS issues FROM errors WHERE {econd} GROUP BY release"), &ps)
        .await?;
    let mut out = Vec::new();
    for mut r in rows {
        let rel = r["release"].as_str().unwrap_or("").to_string();
        if let Some(e) = errs.iter().find(|e| e["release"].as_str() == Some(rel.as_str())) {
            r["errors"] = e["errors"].clone();
            r["issues_seen"] = e["issues"].clone();
        }
        let new_issues = sqlx::query("SELECT COUNT(*) AS n FROM issues WHERE project_id = $1 AND first_release = $2")
            .bind(&project.id)
            .bind(&rel)
            .fetch_one(&state.db)
            .await?
            .i("n");
        r["new_issues"] = json!(new_issues);
        out.push(r);
    }
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(since), "until": iso_time(now)},
        "note": "crash_free_* are fractions (0.995 = 99.5%). Sessions come from SDK session tracking.",
        "releases": out,
    }))
}

pub async fn compare_releases(state: &AppState, p: &Principal, project: &str, base: &str, target: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let health = |r: String| {
        let proj = proj.slug.clone();
        async move { release_health(state, p, &proj, Some(&r), Some("90d")).await }
    };
    let a = health(base.to_string()).await?;
    let b = health(target.to_string()).await?;
    let ps = [("p", proj.id.clone()), ("a", base.to_string()), ("b", target.to_string())];
    let issues = state
        .ch
        .query(
            "SELECT issue_id, countIf(release = {a:String}) AS base_events, countIf(release = {b:String}) AS target_events \
             FROM errors WHERE project_id = {p:String} AND release IN ({a:String}, {b:String}) GROUP BY issue_id \
             ORDER BY (target_events - base_events) DESC LIMIT 50",
            &ps,
        )
        .await?;
    let mut new_in_target = Vec::new();
    let mut gone_in_target = Vec::new();
    let mut increased = Vec::new();
    for r in issues {
        let id = r["issue_id"].as_str().unwrap_or("");
        let Some(mut issue) = get_issue_row(state, id).await? else { continue };
        let (ba, ta) = (r["base_events"].as_i64().unwrap_or(0), r["target_events"].as_i64().unwrap_or(0));
        issue["base_events"] = json!(ba);
        issue["target_events"] = json!(ta);
        let slim = json!({"short_id": issue["short_id"], "id": issue["id"], "title": issue["title"], "culprit": issue["culprit"], "status": issue["status"], "base_events": ba, "target_events": ta});
        if ba == 0 && ta > 0 {
            new_in_target.push(slim);
        } else if ta == 0 && ba > 0 {
            gone_in_target.push(slim);
        } else if ta > ba {
            increased.push(slim);
        }
    }
    Ok(json!({
        "project": proj.slug,
        "base": {"release": base, "health": a["releases"].get(0)},
        "target": {"release": target, "health": b["releases"].get(0)},
        "issues_new_in_target": new_in_target,
        "issues_not_seen_in_target": gone_in_target,
        "issues_increased": increased,
        "note": "Event counts are raw totals per release; compare with session counts to normalize for adoption.",
    }))
}

// ---------------------------------------------------------------------------
// Analytics
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Clone)]
pub struct AnalyticsParams {
    pub project: String,
    pub since: Option<String>,
    pub until: Option<String>,
    /// Filter as `dimension=value` pairs, e.g. {"path": "/pricing", "country": "SE"}.
    #[serde(default)]
    pub filters: std::collections::BTreeMap<String, String>,
    /// Event type filter: pageview, screen_view, custom (default: all).
    pub r#type: Option<String>,
}

/// Map a public dimension name to a ClickHouse expression.
pub fn dimension_expr(d: &str) -> Option<String> {
    Some(
        match d {
            "path" | "page" => "path",
            "url" => "url",
            "hostname" => "hostname",
            "referrer" | "source" => "if(referrer_host = '', if(utm_source != '', utm_source, 'Direct / None'), referrer_host)",
            "referrer_url" => "referrer",
            "utm_source" => "utm_source",
            "utm_medium" => "utm_medium",
            "utm_campaign" => "utm_campaign",
            "utm_term" => "utm_term",
            "utm_content" => "utm_content",
            "country" => "country",
            "device" | "device_type" => "device_type",
            "device_model" => "device_model",
            "browser" => "browser",
            "browser_version" => "concat(browser, ' ', browser_version)",
            "os" => "os_name",
            "os_version" => "concat(os_name, ' ', os_version)",
            "app_version" => "app_version",
            "release" => "release",
            "environment" => "environment",
            "platform" => "platform",
            "event" | "name" => "name",
            "type" => "type",
            "screen" => "props['screen']",
            other => {
                let prop = other.strip_prefix("prop:")?;
                if prop.is_empty() || prop.len() > 128 || !prop.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) {
                    return None;
                }
                return Some(format!("props['{prop}']"));
            }
        }
        .to_string(),
    )
}

pub const DIMENSIONS: &[&str] = &[
    "path", "url", "hostname", "referrer", "referrer_url", "utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content",
    "country", "device", "device_model", "browser", "browser_version", "os", "os_version", "app_version", "release", "environment",
    "platform", "event", "type", "screen", "prop:<name>",
];

pub(crate) struct Where {
    pub sql: String,
    pub params: Vec<(&'static str, String)>,
    pub since: i64,
    pub until: i64,
}

pub(crate) fn analytics_where(project_id: &str, a: &AnalyticsParams) -> QResult<Where> {
    let now = now_ms();
    let since = a.since.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 30 * 86_400_000);
    let until = a.until.as_deref().and_then(|s| parse_time_expr(s, now)).unwrap_or(now);
    let mut conds = vec![
        "project_id = {p:String}".to_string(),
        "timestamp >= fromUnixTimestamp64Milli({since:Int64})".to_string(),
        "timestamp <= fromUnixTimestamp64Milli({until:Int64})".to_string(),
    ];
    let mut params: Vec<(&'static str, String)> = vec![("p", project_id.to_string()), ("since", since.to_string()), ("until", until.to_string())];
    if let Some(t) = a.r#type.as_ref().filter(|t| !t.is_empty()) {
        conds.push("type = {etype:String}".into());
        params.push(("etype", t.clone()));
    }
    const FILTER_NAMES: [&str; 8] = ["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7"];
    for (i, (dim, val)) in a.filters.iter().enumerate() {
        let name = *FILTER_NAMES.get(i).ok_or_else(|| ApiError::bad("at most 8 filters"))?;
        let (cond, value) = filter_cond(dim, val, name)?;
        conds.push(cond);
        params.push((name, value));
    }
    Ok(Where { sql: conds.join(" AND "), params, since, until })
}

/// One `dimension=value` filter as SQL with a named parameter. A value starting with `~` means
/// contains (case-insensitive), `!` means not equal.
pub(crate) fn filter_cond(dim: &str, val: &str, name: &str) -> QResult<(String, String)> {
    let expr = dimension_expr(dim).ok_or_else(|| ApiError::bad(format!("unknown dimension `{dim}`; valid: {}", DIMENSIONS.join(", "))))?;
    Ok(if let Some(v) = val.strip_prefix('~') {
        (format!("positionCaseInsensitive({expr}, {{{name}:String}}) > 0"), v.to_string())
    } else if let Some(v) = val.strip_prefix('!') {
        (format!("{expr} != {{{name}:String}}"), v.to_string())
    } else {
        (format!("{expr} = {{{name}:String}}"), val.to_string())
    })
}

const METRICS: &str = "uniq(visitor_id) AS visitors, countIf(type = 'pageview') AS pageviews, countIf(type = 'screen_view') AS screen_views, \
    countIf(type = 'custom') AS custom_events, count() AS events, uniqIf(user_id, user_id != '') AS identified_users";

pub async fn analytics_summary(state: &AppState, p: &Principal, a: AnalyticsParams) -> QResult<Value> {
    let project = project_for(state, p, &a.project).await?;
    let w = analytics_where(&project.id, &a)?;
    let rows = state.ch.query(&format!("SELECT {METRICS} FROM events WHERE {}", w.sql), &w.params).await?;
    // Previous period for comparison.
    let len = w.until - w.since;
    let mut prev = a.clone();
    prev.since = Some((w.since - len).to_string());
    prev.until = Some(w.since.to_string());
    let pw = analytics_where(&project.id, &prev)?;
    let prev_rows = state.ch.query(&format!("SELECT {METRICS} FROM events WHERE {}", pw.sql), &pw.params).await?;
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(w.since), "until": iso_time(w.until)},
        "filters": a.filters,
        "totals": rows.first(),
        "previous_period": prev_rows.first(),
    }))
}

pub async fn analytics_timeseries(state: &AppState, p: &Principal, a: AnalyticsParams, interval: Option<&str>) -> QResult<Value> {
    let project = project_for(state, p, &a.project).await?;
    let w = analytics_where(&project.id, &a)?;
    let interval = match interval {
        Some("hour") => "hour",
        Some("day") => "day",
        Some("week") => "week",
        Some("month") => "month",
        None => if w.until - w.since <= 2 * 86_400_000 { "hour" } else { "day" },
        Some(other) => return Err(ApiError::bad(format!("unknown interval `{other}`: hour, day, week or month"))),
    };
    let bucket = match interval {
        "hour" => "toStartOfHour(timestamp)",
        "day" => "toStartOfDay(timestamp)",
        "week" => "toStartOfWeek(timestamp, 1)",
        _ => "toStartOfMonth(timestamp)",
    };
    let rows = state
        .ch
        .query(&format!("SELECT formatDateTime({bucket}, '%Y-%m-%dT%H:%i:%SZ') AS bucket, {METRICS} FROM events WHERE {} GROUP BY bucket ORDER BY bucket", w.sql), &w.params)
        .await?;
    Ok(json!({"project": project.slug, "interval": interval, "window": {"since": iso_time(w.since), "until": iso_time(w.until)}, "filters": a.filters, "series": rows}))
}

pub async fn analytics_breakdown(state: &AppState, p: &Principal, a: AnalyticsParams, dimension: &str, limit: Option<i64>) -> QResult<Value> {
    let project = project_for(state, p, &a.project).await?;
    let expr = dimension_expr(dimension).ok_or_else(|| ApiError::bad(format!("unknown dimension `{dimension}`; valid: {}", DIMENSIONS.join(", "))))?;
    let mut w = analytics_where(&project.id, &a)?;
    w.params.push(("lim", limit.unwrap_or(10).clamp(1, 1000).to_string()));
    let rows = state
        .ch
        .query(&format!("SELECT {expr} AS value, {METRICS} FROM events WHERE {} GROUP BY value ORDER BY visitors DESC, events DESC LIMIT {{lim:UInt32}}", w.sql), &w.params)
        .await?;
    Ok(json!({"project": project.slug, "dimension": dimension, "window": {"since": iso_time(w.since), "until": iso_time(w.until)}, "filters": a.filters, "rows": rows}))
}

/// Explain why errors spiked: compare a recent window with a baseline and show what changed.
pub async fn explain_spike(state: &AppState, p: &Principal, project: &str, window: Option<&str>, baseline: Option<&str>) -> QResult<Value> {
    let project = project_for(state, p, project).await?;
    let now = now_ms();
    let win_start = window.and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 3_600_000);
    let win_len = now - win_start;
    let base_start = baseline.and_then(|s| parse_time_expr(s, now)).unwrap_or(now - 24 * 3_600_000 - win_len);
    let base_len = (win_start - base_start).max(1);
    let scale = win_len as f64 / base_len as f64;
    let ps = vec![("p", project.id.clone()), ("ws", win_start.to_string()), ("bs", base_start.to_string()), ("scale", format!("{scale:.6}"))];
    let totals = state
        .ch
        .query(
            "SELECT countIf(timestamp >= fromUnixTimestamp64Milli({ws:Int64})) AS window_events, \
             countIf(timestamp < fromUnixTimestamp64Milli({ws:Int64})) AS baseline_events, \
             round(baseline_events * {scale:Float64}, 1) AS expected_window_events \
             FROM errors WHERE project_id = {p:String} AND timestamp >= fromUnixTimestamp64Milli({bs:Int64})",
            &ps,
        )
        .await?;
    let diff = |col: &str| {
        format!(
            "SELECT {col} AS value, countIf(timestamp >= fromUnixTimestamp64Milli({{ws:Int64}})) AS window_events, \
             countIf(timestamp < fromUnixTimestamp64Milli({{ws:Int64}})) AS baseline_events, \
             round(window_events - baseline_events * {{scale:Float64}}, 1) AS increase \
             FROM errors WHERE project_id = {{p:String}} AND timestamp >= fromUnixTimestamp64Milli({{bs:Int64}}) \
             GROUP BY value HAVING window_events > 0 ORDER BY increase DESC LIMIT 5"
        )
    };
    let mut top_issues = state.ch.query(&diff("issue_id"), &ps).await?;
    for r in top_issues.iter_mut() {
        if let Some(issue) = get_issue_row(state, r["value"].as_str().unwrap_or("")).await? {
            r["issue"] = json!({"short_id": issue["short_id"], "title": issue["title"], "culprit": issue["culprit"], "status": issue["status"], "first_seen": issue["first_seen"], "first_release": issue["first_release"]});
        }
    }
    let mut breakdowns = serde_json::Map::new();
    for (name, col) in [
        ("release", "release"),
        ("environment", "environment"),
        ("os", "concat(os_name, ' ', os_version)"),
        ("device_or_browser", "if(device_model != '', device_model, browser)"),
        ("country", "country"),
        ("exception_type", "exception_type"),
    ] {
        breakdowns.insert(name.into(), json!(state.ch.query(&diff(col), &ps).await?));
    }
    let new_issues = sqlx::query("SELECT i.id FROM issues i WHERE i.project_id = $1 AND i.first_seen >= $2 ORDER BY i.first_seen DESC LIMIT 10")
        .bind(&project.id)
        .bind(win_start)
        .fetch_all(&state.db)
        .await?;
    let mut new_list = Vec::new();
    for r in new_issues {
        if let Some(i) = get_issue_row(state, &r.s("id")).await? {
            new_list.push(json!({"short_id": i["short_id"], "title": i["title"], "first_release": i["first_release"]}));
        }
    }
    Ok(json!({
        "project": project.slug,
        "window": {"since": iso_time(win_start), "until": iso_time(now)},
        "baseline": {"since": iso_time(base_start), "until": iso_time(win_start)},
        "totals": totals.first(),
        "top_issue_increases": top_issues,
        "new_issues_in_window": new_list,
        "breakdown_increases": breakdowns,
        "how_to_read": "increase = window_events - baseline_events scaled to the window length. Large increases concentrated in one release, OS or device usually point at the cause.",
    }))
}
