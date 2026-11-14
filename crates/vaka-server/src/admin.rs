//! Minimal server-rendered admin UI: login, projects and keys, users and
//! roles, API tokens, alert rules. Everything else is meant for the API/MCP.

use crate::auth::{self, Principal, Role};
use crate::db::RowExt;
use crate::query;
use crate::state::AppState;
use crate::util::{iso_time, now_ms, sha256_hex};
use axum::extract::{Form, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use maud::{html, Markup, PreEscaped, DOCTYPE};
use serde::Deserialize;
use std::collections::HashMap;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { Redirect::to("/ui") }))
        .route("/ui", get(dashboard))
        .route("/ui/login", get(login_page).post(login))
        .route("/ui/login/code", get(code_page).post(request_code))
        .route("/ui/login/verify", post(verify_code))
        .route("/ui/logout", post(logout))
        .route("/ui/invite/{token}", get(invite_page).post(accept_invite))
        .route("/ui/account", get(account_page).post(change_password))
        .route("/ui/projects", post(create_project))
        .route("/ui/projects/{slug}", get(project_page).post(update_project))
        .route("/ui/projects/{slug}/keys", post(project_keys))
        .route("/ui/projects/{slug}/issues/{id}", get(issue_page).post(issue_action))
        .route("/ui/projects/{slug}/feedback", get(feedback_page))
        .route("/ui/projects/{slug}/feedback/{id}", get(feedback_detail).post(feedback_action))
        .route("/ui/tokens", get(tokens_page).post(tokens_action))
        .route("/ui/users", get(users_page).post(users_action))
        .route("/ui/alerts", get(alerts_page).post(alerts_action))
}

const CSS: &str = r#"
:root{--bg:#fafaf9;--fg:#1c1917;--muted:#78716c;--line:#e7e5e4;--card:#fff;--accent:#2563eb;--bad:#dc2626;--ok:#16a34a}
@media (prefers-color-scheme:dark){:root{--bg:#0c0a09;--fg:#e7e5e4;--muted:#a8a29e;--line:#292524;--card:#1c1917;--accent:#60a5fa}}
*{box-sizing:border-box}body{margin:0;font:15px/1.5 system-ui,-apple-system,sans-serif;background:var(--bg);color:var(--fg)}
header{display:flex;gap:20px;align-items:center;padding:12px 24px;border-bottom:1px solid var(--line);background:var(--card);flex-wrap:wrap}
header a{color:var(--fg);text-decoration:none}header .brand{font-weight:700}header .sp{flex:1}
main{max-width:1000px;margin:24px auto;padding:0 16px}h1{font-size:22px}h2{font-size:17px;margin-top:28px}
.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:16px;margin:12px 0}
table{width:100%;border-collapse:collapse}td,th{text-align:left;padding:8px;border-bottom:1px solid var(--line);vertical-align:top}th{color:var(--muted);font-weight:500;font-size:13px}
input,select,textarea{font:inherit;padding:6px 8px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--fg)}
button{font:inherit;padding:6px 12px;border:1px solid var(--accent);border-radius:6px;background:var(--accent);color:#fff;cursor:pointer}
button.ghost{background:transparent;color:var(--accent)}button.danger{background:var(--bad);border-color:var(--bad)}
form.inline{display:inline}.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}
code,pre{font:13px ui-monospace,SFMono-Regular,Menlo,monospace}pre{background:var(--bg);border:1px solid var(--line);padding:12px;border-radius:6px;overflow:auto;white-space:pre-wrap}
.muted{color:var(--muted)}.flash{padding:10px 14px;border-radius:6px;border:1px solid var(--accent);margin:12px 0}.err{border-color:var(--bad);color:var(--bad)}
.badge{display:inline-block;padding:1px 8px;border-radius:999px;border:1px solid var(--line);font-size:12px}
.frame{padding:4px 0;border-bottom:1px dashed var(--line)}.frame.app{font-weight:600}
.thumb{width:72px;height:48px;object-fit:cover;border:1px solid var(--line);border-radius:4px}.shot{max-width:100%;border:1px solid var(--line);border-radius:6px;margin:8px 0}
.s-negative{border-color:var(--bad);color:var(--bad)}.s-positive{border-color:var(--ok);color:var(--ok)}.msg{white-space:pre-wrap}
"#;

fn page(title: &str, user: Option<&Principal>, flash: Option<&str>, body: Markup) -> Response {
    let m = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Vaka" }
                style { (PreEscaped(CSS)) }
            }
            body {
                header {
                    a.brand href="/ui" { "Vaka" }
                    @if let Some(Principal::User { email, role, .. }) = user {
                        a href="/ui" { "Projects" }
                        @if *role >= Role::Admin {
                            a href="/ui/tokens" { "API tokens" }
                            a href="/ui/users" { "Users" }
                        }
                        a href="/ui/alerts" { "Alerts" }
                        a href="/docs/" { "Docs" }
                        span.sp {}
                        a.muted href="/ui/account" { (email) }
                        form.inline method="post" action="/ui/logout" { button.ghost type="submit" { "Log out" } }
                    }
                }
                main {
                    @if let Some(f) = flash {
                        @if let Some(e) = f.strip_prefix("error:") { div.flash.err { (e) } } @else { div.flash { (f) } }
                    }
                    (body)
                }
            }
        }
    };
    m.into_response()
}

async fn ui_user(state: &AppState, headers: &HeaderMap) -> Option<Principal> {
    let cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == auth::SESSION_COOKIE)
        .map(|(_, v)| v.to_string())?;
    auth::principal_from_session(state, &cookie).await.ok().flatten()
}

/// Reject cross-site form posts (defense in depth on top of SameSite=Lax).
fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        None => true,
        Some(origin) => {
            let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
            origin.ends_with(&format!("//{host}")) || origin == state.config.public_url
        }
    }
}

macro_rules! require_user {
    ($state:expr, $headers:expr) => {
        match ui_user(&$state, &$headers).await {
            Some(u) => u,
            None => return Redirect::to("/ui/login").into_response(),
        }
    };
}

fn back(to: &str, msg: &str) -> Response {
    let sep = if to.contains('?') { '&' } else { '?' };
    Redirect::to(&format!("{to}{sep}flash={}", url::form_urlencoded::byte_serialize(msg.as_bytes()).collect::<String>())).into_response()
}

fn flash_of(q: &HashMap<String, String>) -> Option<String> {
    q.get("flash").cloned()
}

type Q = axum::extract::Query<HashMap<String, String>>;

async fn login_page(State(s): State<AppState>, axum::extract::Query(q): Q) -> Response {
    let no_users = sqlx::query("SELECT COUNT(*) AS n FROM users").fetch_one(&s.db).await.map(|r| r.i("n") == 0).unwrap_or(false);
    let (email_login, password_login) = (s.config.email_login(), s.config.password_login_enabled());
    page("Log in", None, flash_of(&q).as_deref(), html! {
        div.card style="max-width:380px;margin:60px auto" {
            h1 { "Log in to Vaka" }
            @if no_users {
                p.flash.err { "No users yet. Set VAKA_ADMIN_EMAIL and VAKA_ADMIN_PASSWORD and restart Vaka to create the first owner." }
            }
            @if email_login {
                form method="post" action="/ui/login/code" {
                    p { input type="email" name="email" placeholder="Email" required autofocus autocomplete="email" style="width:100%"; }
                    button type="submit" { "Email me a sign-in code" }
                }
            }
            @if email_login && password_login {
                p.muted style="margin:20px 0 8px" { "Or use your password" }
            }
            @if password_login {
                form method="post" action="/ui/login" {
                    p { input type="email" name="email" placeholder="Email" required autofocus[!email_login] autocomplete="username" style="width:100%"; }
                    p { input type="password" name="password" placeholder="Password" required autocomplete="current-password" style="width:100%"; }
                    @if email_login { button.ghost type="submit" { "Log in with password" } } @else { button type="submit" { "Log in" } }
                }
            }
        }
    })
}

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    password: String,
}

async fn login(State(s): State<AppState>, Form(f): Form<LoginForm>) -> Response {
    if !s.config.password_login_enabled() {
        return back("/ui/login", "error:Password login is turned off; use an email code");
    }
    let row = sqlx::query("SELECT id, password_hash, disabled FROM users WHERE email = $1")
        .bind(f.email.trim().to_lowercase())
        .fetch_optional(&s.db)
        .await
        .ok()
        .flatten();
    let Some(row) = row.filter(|r| r.i("disabled") == 0 && auth::verify_password(&f.password, &r.s("password_hash"))) else {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        return back("/ui/login", "error:Wrong email or password");
    };
    start_session(&s, &row.s("id")).await
}

async fn start_session(s: &AppState, user_id: &str) -> Response {
    let Ok(token) = auth::create_session(s, user_id).await else {
        return back("/ui/login", "error:Could not create session");
    };
    ([(header::SET_COOKIE, auth::session_cookie(s, &token, 30 * 86400))], Redirect::to("/ui")).into_response()
}

#[derive(Deserialize)]
struct CodeRequestForm {
    email: String,
}

fn code_page_url(email: &str, msg: &str) -> String {
    let enc = |v: &str| url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>();
    format!("/ui/login/code?email={}&flash={}", enc(email), enc(msg))
}

async fn request_code(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<CodeRequestForm>) -> Response {
    if !s.config.email_login() {
        return back("/ui/login", "error:Email codes are not available on this server");
    }
    if !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let email = f.email.trim().to_lowercase();
    if email.is_empty() {
        return back("/ui/login", "error:Enter your email address");
    }
    // Same message whether or not the account exists, or a code was sent moments ago,
    // so this form can't be used to find out which addresses have accounts.
    let msg = match crate::otp::request_code(&s, &email).await {
        crate::otp::RequestOutcome::Sent | crate::otp::RequestOutcome::TooSoon => {
            "If that address has an account, we've emailed it a 6-digit code. It's valid for 10 minutes, and you can ask for a new one once a minute."
        }
        crate::otp::RequestOutcome::SendFailed => "error:Could not send the email. Try again in a minute.",
    };
    Redirect::to(&code_page_url(&email, msg)).into_response()
}

async fn code_page(State(s): State<AppState>, axum::extract::Query(q): Q) -> Response {
    let Some(email) = q.get("email").filter(|e| !e.trim().is_empty()) else { return Redirect::to("/ui/login").into_response() };
    if !s.config.email_login() {
        return Redirect::to("/ui/login").into_response();
    }
    page("Enter code", None, flash_of(&q).as_deref(), html! {
        div.card style="max-width:380px;margin:60px auto" {
            h1 { "Check your email" }
            p.muted { "Enter the code sent to " strong { (email) } "." }
            form method="post" action="/ui/login/verify" {
                input type="hidden" name="email" value=(email);
                p { input name="code" placeholder="123456" required autofocus inputmode="numeric" autocomplete="one-time-code" pattern="[0-9 -]{6,9}" maxlength="9" style="width:100%;font-size:20px;letter-spacing:4px"; }
                button type="submit" { "Log in" }
            }
            form method="post" action="/ui/login/code" style="margin-top:12px" {
                input type="hidden" name="email" value=(email);
                button.ghost type="submit" { "Send a new code" }
                " " a href="/ui/login" { "Back" }
            }
        }
    })
}

#[derive(Deserialize)]
struct VerifyForm {
    email: String,
    code: String,
}

async fn verify_code(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<VerifyForm>) -> Response {
    if !s.config.email_login() {
        return back("/ui/login", "error:Email codes are not available on this server");
    }
    if !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let email = f.email.trim().to_lowercase();
    match crate::otp::verify_code(&s, &email, &f.code).await {
        Some(user_id) => start_session(&s, &user_id).await,
        None => {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            Redirect::to(&code_page_url(&email, "error:That code is wrong or has expired. After 5 wrong tries you need a new code.")).into_response()
        }
    }
}

async fn logout(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(c) = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == auth::SESSION_COOKIE)
    {
        let _ = sqlx::query("DELETE FROM web_sessions WHERE token_hash = $1").bind(sha256_hex(c.1)).execute(&s.db).await;
    }
    ([(header::SET_COOKIE, auth::session_cookie(&s, "", 0))], Redirect::to("/ui/login")).into_response()
}

async fn dashboard(State(s): State<AppState>, headers: HeaderMap, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    let projects = crate::projects::list(&s).await.unwrap_or_default();
    let mut counts = HashMap::new();
    for p in &projects {
        let n = sqlx::query("SELECT COUNT(*) AS n FROM issues WHERE project_id = $1 AND status = 'open'")
            .bind(&p.id)
            .fetch_one(&s.db)
            .await
            .map(|r| r.i("n"))
            .unwrap_or(0);
        counts.insert(p.id.clone(), n);
    }
    page("Projects", Some(&user), flash_of(&q).as_deref(), html! {
        h1 { "Projects" }
        @if projects.is_empty() {
            p.muted { "No projects yet. Create one below, or let a coding agent do it over MCP with the create_project tool." }
        } @else {
            div.card { table {
                tr { th { "Name" } th { "Slug" } th { "Platform" } th { "Open issues" } th { "Retention" } }
                @for p in &projects {
                    tr {
                        td { a href=(format!("/ui/projects/{}", p.slug)) { (p.name) } }
                        td { code { (p.slug) } }
                        td { (p.platform) }
                        td { (counts.get(&p.id).copied().unwrap_or(0)) }
                        td { (p.retention_days) " days" }
                    }
                }
            } }
        }
        @if user.is_admin() {
            h2 { "New project" }
            div.card { form.row method="post" action="/ui/projects" {
                input name="name" placeholder="Name" required;
                select name="platform" { @for pl in crate::api::PLATFORMS { option value=(pl) { (pl) } } }
                button type="submit" { "Create project" }
            } }
        }
        h2 { "Connect an agent" }
        div.card {
            p { "Agents use the MCP endpoint with an API token:" }
            pre { (format!("{{\n  \"mcpServers\": {{\n    \"vaka\": {{\n      \"type\": \"http\",\n      \"url\": \"{}/mcp\",\n      \"headers\": {{ \"Authorization\": \"Bearer <api token>\" }}\n    }}\n  }}\n}}", s.config.public_url)) }
            p.muted { "REST API: " code { (s.config.public_url) "/api/v1" } " · OpenAPI: " a href="/api/v1/openapi.json" { "/api/v1/openapi.json" } " · Agent docs: " a href="/llms.txt" { "/llms.txt" } }
        }
    })
}

#[derive(Deserialize)]
struct NewProject {
    name: String,
    platform: String,
}

async fn create_project(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<NewProject>) -> Response {
    let user = require_user!(s, headers);
    if !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match crate::api::create_project_inner(&s, &user, crate::api::CreateProject { name: f.name, platform: f.platform, slug: None }).await {
        Ok(v) => back(&format!("/ui/projects/{}", v["slug"].as_str().unwrap_or("")), "Project created. Install the SDK with the instructions below."),
        Err(e) => back("/ui", &format!("error:{}", e.1)),
    }
}

async fn project_page(State(s): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    let Ok(Some(p)) = crate::projects::find(&s, &slug).await else { return back("/ui", "error:Project not found") };
    let keys = crate::projects::ingest_keys(&s, &p.id).await.unwrap_or_default();
    let issues = query::list_issues(&s, &user, query::IssueListParams { project: p.id.clone(), since: Some("7d".into()), limit: Some(20), ..Default::default() })
        .await
        .map(|v| v["issues"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let install = crate::docs::install_markdown(&p.platform, &s.config.public_url, keys.first().map(|s| s.as_str()).unwrap_or(""), &p.slug);
    page(&p.name, Some(&user), flash_of(&q).as_deref(), html! {
        h1 { (p.name) " " span.badge { (p.platform) } }
        p { a href=(format!("/ui/projects/{}/feedback", p.slug)) { "User feedback →" } }
        h2 { "Open issues (last 7 days)" }
        div.card {
            @if issues.is_empty() { p.muted { "No open issues. " } }
            @else { table {
                tr { th { "Issue" } th { "Events (7d)" } th { "Last seen" } th { "Release" } }
                @for i in &issues {
                    tr {
                        td { a href=(format!("/ui/projects/{}/issues/{}", p.slug, i["id"].as_str().unwrap_or(""))) { code { (i["short_id"].as_str().unwrap_or("")) } " " (i["title"].as_str().unwrap_or("")) } br; span.muted { (i["culprit"].as_str().unwrap_or("")) } }
                        td { (i["events_in_window"]) }
                        td.muted { (i["last_seen"].as_str().unwrap_or("")) }
                        td { (i["last_release"].as_str().unwrap_or("")) }
                    }
                }
            } }
        }
        h2 { "Ingest keys" }
        div.card {
            p.muted { "Ingest keys are write-only and safe to ship inside apps." }
            table { @for k in &keys { tr { td { code { (k) } } td { @if user.is_admin() { form.inline method="post" action=(format!("/ui/projects/{}/keys", p.slug)) { input type="hidden" name="revoke" value=(k); button.danger type="submit" { "Revoke" } } } } } } }
            @if user.is_admin() { form method="post" action=(format!("/ui/projects/{}/keys", p.slug)) { button.ghost type="submit" { "Create another key" } } }
        }
        @if user.is_admin() {
            h2 { "Settings" }
            div.card { form method="post" action=(format!("/ui/projects/{}", p.slug)) {
                p { label { "Name " input name="name" value=(p.name); } }
                p { label { "Retention (days) " input type="number" min="1" max="3650" name="retention_days" value=(p.retention_days); } }
                p { label { input type="checkbox" name="scrub_enabled" value="1" checked[p.scrub_enabled]; " Scrub secrets and PII from error payloads" } }
                p { label { "Extra fields to scrub (comma-separated) " input name="scrub_fields" value=(p.scrub_fields) style="width:300px"; } }
                button type="submit" { "Save" }
            } }
        }
        @if let Some(md) = install {
            h2 { "Install" }
            div.card { pre { (md) } }
        }
    })
}

#[derive(Deserialize)]
struct ProjectForm {
    name: String,
    retention_days: i64,
    scrub_enabled: Option<String>,
    scrub_fields: String,
}

async fn update_project(State(s): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, Form(f): Form<ProjectForm>) -> Response {
    let user = require_user!(s, headers);
    if !user.is_admin() || !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(Some(p)) = crate::projects::find(&s, &slug).await else { return back("/ui", "error:Project not found") };
    if !(1..=3650).contains(&f.retention_days) {
        return back(&format!("/ui/projects/{slug}"), "error:Retention must be between 1 and 3650 days");
    }
    let _ = sqlx::query("UPDATE projects SET name = $1, retention_days = $2, scrub_enabled = $3, scrub_fields = $4 WHERE id = $5")
        .bind(f.name.trim())
        .bind(f.retention_days)
        .bind(f.scrub_enabled.is_some() as i64)
        .bind(f.scrub_fields.trim())
        .bind(&p.id)
        .execute(&s.db)
        .await;
    crate::projects::invalidate_cache(&s).await;
    auth::audit(&s, &user.label(), "update_project", &p.id, "ui").await;
    back(&format!("/ui/projects/{slug}"), "Saved")
}

#[derive(Deserialize)]
struct KeyForm {
    revoke: Option<String>,
}

async fn project_keys(State(s): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, Form(f): Form<KeyForm>) -> Response {
    let user = require_user!(s, headers);
    if !user.is_admin() || !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(Some(p)) = crate::projects::find(&s, &slug).await else { return back("/ui", "error:Project not found") };
    match f.revoke {
        Some(k) => {
            let _ = sqlx::query("UPDATE ingest_keys SET revoked_at = $1 WHERE key = $2 AND project_id = $3").bind(now_ms()).bind(&k).bind(&p.id).execute(&s.db).await;
            crate::projects::invalidate_cache(&s).await;
            back(&format!("/ui/projects/{slug}"), "Key revoked")
        }
        None => {
            let _ = crate::projects::create_ingest_key(&s, &p.id).await;
            back(&format!("/ui/projects/{slug}"), "Key created")
        }
    }
}

fn render_frames(frames: &serde_json::Value) -> Markup {
    html! {
        @for f in frames.as_array().cloned().unwrap_or_default() {
            @let app = f["in_app"].as_bool().unwrap_or(true);
            div class=(if app { "frame app" } else { "frame" }) {
                code { (f["function"].as_str().unwrap_or("?")) }
                span.muted { " " (f["filename"].as_str().or(f["module"].as_str()).or(f["abs_path"].as_str()).unwrap_or("")) @if let Some(l) = f["lineno"].as_u64() { ":" (l) } }
                @if let Some(c) = f["context_line"].as_str() { pre { (c.trim()) } }
            }
        }
    }
}

async fn issue_page(State(s): State<AppState>, headers: HeaderMap, Path((slug, id)): Path<(String, String)>, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    let issue = match query::get_issue(&s, &user, &id).await {
        Ok(i) => i,
        Err(e) => return back(&format!("/ui/projects/{slug}"), &format!("error:{}", e.1)),
    };
    let ev = &issue["latest_event"];
    page(issue["title"].as_str().unwrap_or("Issue"), Some(&user), flash_of(&q).as_deref(), html! {
        p { a href=(format!("/ui/projects/{slug}")) { "← " (slug) } }
        h1 { code { (issue["short_id"].as_str().unwrap_or("")) } " " (issue["title"].as_str().unwrap_or("")) }
        p.muted { (issue["culprit"].as_str().unwrap_or("")) }
        div.row {
            span.badge { (issue["status"].as_str().unwrap_or("")) }
            span { "Seen " (issue["times_seen"]) " times · first " (issue["first_seen"].as_str().unwrap_or("")) " (" (issue["first_release"].as_str().unwrap_or("-")) ") · last " (issue["last_seen"].as_str().unwrap_or("")) " (" (issue["last_release"].as_str().unwrap_or("-")) ")" }
        }
        @if user.can_write(issue["project_id"].as_str().unwrap_or("")) {
            div.row style="margin-top:12px" {
                form.inline method="post" { input type="hidden" name="status" value="resolved"; button type="submit" { "Resolve" } }
                form.inline method="post" { input type="hidden" name="status" value="ignored"; button.ghost type="submit" { "Ignore" } }
                form.inline method="post" { input type="hidden" name="status" value="open"; button.ghost type="submit" { "Reopen" } }
            }
        }
        @if !ev.is_null() {
            h2 { "Latest event" }
            div.card {
                p.muted { (ev["timestamp"].as_str().unwrap_or("")) " · " (ev["release"].as_str().unwrap_or("")) " · " (ev["environment"].as_str().unwrap_or("")) " · " (ev["device"]["os_name"].as_str().unwrap_or("")) " " (ev["device"]["os_version"].as_str().unwrap_or("")) }
                @for exc in ev["exceptions"].as_array().cloned().unwrap_or_default() {
                    p { strong { (exc["type"].as_str().unwrap_or("")) } ": " (exc["value"].as_str().unwrap_or("")) }
                    (render_frames(&exc["stacktrace"]))
                }
                @if ev["exceptions"].as_array().map(|a| a.is_empty()).unwrap_or(true) { pre { (ev["message"].as_str().unwrap_or("")) } }
            }
            h2 { "Breadcrumbs" }
            div.card { pre { (serde_json::to_string_pretty(&ev["breadcrumbs"]).unwrap_or_default()) } }
        }
        h2 { "Releases" }
        div.card { table { @for r in issue["by_release"].as_array().cloned().unwrap_or_default() { tr { td { (r["value"].as_str().unwrap_or("")) } td { (r["events"]) } } } } }
    })
}

fn sentiment_badge(s: &serde_json::Value) -> Markup {
    match s.as_str() {
        Some(x) => html! { span class=(format!("badge s-{x}")) { (x) } },
        None => html! { span.badge.muted { "untagged" } },
    }
}

async fn feedback_page(State(s): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    let Ok(Some(p)) = crate::projects::find(&s, &slug).await else { return back("/ui", "error:Project not found") };
    let status = q.get("status").cloned().filter(|v| !v.is_empty()).unwrap_or_else(|| "open".into());
    let sentiment = q.get("sentiment").cloned().filter(|v| !v.is_empty());
    let offset: i64 = q.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
    let params = crate::feedback::ListParams {
        project: p.id.clone(),
        status: Some(status.clone()),
        sentiment: sentiment.clone(),
        since: Some("365d".into()),
        limit: Some(50),
        offset: Some(offset),
        ..Default::default()
    };
    let (items, next) = match crate::feedback::list(&s, &user, params).await {
        Ok(v) => (v["feedback"].as_array().cloned().unwrap_or_default(), v["next_offset"].as_i64()),
        Err(e) => return back(&format!("/ui/projects/{slug}"), &format!("error:{}", e.1)),
    };
    page(&format!("Feedback · {}", p.name), Some(&user), flash_of(&q).as_deref(), html! {
        p { a href=(format!("/ui/projects/{slug}")) { "← " (p.name) } }
        h1 { "User feedback" }
        form.row method="get" {
            select name="status" {
                @for st in ["open", "all", "new", "triaged", "resolved", "ignored"] { option value=(st) selected[st == status] { (st) } }
            }
            select name="sentiment" {
                option value="" { "any sentiment" }
                @for st in ["negative", "mixed", "neutral", "positive", "untagged"] { option value=(st) selected[sentiment.as_deref() == Some(st)] { (st) } }
            }
            button.ghost type="submit" { "Filter" }
        }
        div.card {
            @if items.is_empty() { p.muted { "No feedback matches. Feedback arrives from the SDK widgets, crash prompts or captureFeedback()." } }
            @else { table {
                tr { th { "" } th { "Feedback" } th { "Sentiment" } th { "Status" } th { "Release" } th { "Received" } }
                @for f in &items {
                    @let id = f["id"].as_str().unwrap_or("");
                    @let link = format!("/ui/projects/{slug}/feedback/{id}");
                    tr {
                        td { @if f["attachments"].as_array().map(|a| !a.is_empty()).unwrap_or(false) { a href=(link) { img.thumb loading="lazy" alt="screenshot" src=(format!("/api/v1/feedback/{id}/attachments/0")); } } }
                        td {
                            a href=(link) { code { (f["short_id"].as_str().unwrap_or("")) } } " " (crate::util::truncate(f["message"].as_str().unwrap_or(""), 160))
                            @let topics: Vec<String> = f["topics"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect();
                            @if !topics.is_empty() || f["issue_short_id"].is_string() {
                                br; span.muted { (topics.join(", ")) @if let Some(i) = f["issue_short_id"].as_str() { " · about " (i) } }
                            }
                        }
                        td { (sentiment_badge(&f["sentiment"])) }
                        td { span.badge { (f["status"].as_str().unwrap_or("")) } }
                        td { (f["release"].as_str().unwrap_or("")) }
                        td.muted { (f["created_at"].as_str().unwrap_or("")) }
                    }
                }
            } }
        }
        @if let Some(n) = next {
            p { a href=(format!("/ui/projects/{slug}/feedback?status={status}&sentiment={}&offset={n}", sentiment.clone().unwrap_or_default())) { "Older →" } }
        }
        p.muted { "Agents read and triage feedback with the MCP tools list_feedback, get_feedback, update_feedback and feedback_summary." }
    })
}

async fn feedback_detail(State(s): State<AppState>, headers: HeaderMap, Path((slug, id)): Path<(String, String)>, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    let f = match crate::feedback::get(&s, &user, &id).await {
        Ok(f) => f,
        Err(e) => return back(&format!("/ui/projects/{slug}/feedback"), &format!("error:{}", e.1)),
    };
    let fid = f["id"].as_str().unwrap_or("").to_string();
    let can_write = user.can_write(f["project_id"].as_str().unwrap_or(""));
    let status = f["status"].as_str().unwrap_or("").to_string();
    let device = &f["device"];
    let device_line: Vec<String> = ["model", "os_name", "os_version", "browser", "app_version"].iter().filter_map(|k| device[*k].as_str().filter(|v| !v.is_empty()).map(String::from)).collect();
    page(&format!("Feedback {}", f["short_id"].as_str().unwrap_or("")), Some(&user), flash_of(&q).as_deref(), html! {
        p { a href=(format!("/ui/projects/{slug}/feedback")) { "← Feedback" } }
        h1 { code { (f["short_id"].as_str().unwrap_or("")) } " " (sentiment_badge(&f["sentiment"])) " " span.badge { (status) } }
        div.card {
            p.msg { (f["message"].as_str().unwrap_or("")) }
            @if let Some(sum) = f["summary"].as_str() { p.muted { "Summary: " (sum) } }
            @let topics: Vec<String> = f["topics"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect();
            @if !topics.is_empty() { p { @for t in &topics { span.badge { (t) } " " } } }
        }
        @if can_write {
            form.row method="post" {
                select name="status" { @for st in crate::feedback::STATUSES { option value=(st) selected[*st == status] { (st) } } }
                button type="submit" { "Update status" }
            }
        }
        @for a in f["attachments"].as_array().cloned().unwrap_or_default() {
            a href=(format!("/api/v1/feedback/{fid}/attachments/{}", a["index"])) target="_blank" {
                img.shot alt="screenshot" src=(format!("/api/v1/feedback/{fid}/attachments/{}", a["index"]));
            }
        }
        h2 { "Context" }
        div.card { table {
            @if let Some(e) = f["contact_email"].as_str() { tr { th { "Contact" } td { (f["contact_name"].as_str().unwrap_or("")) " " a href=(format!("mailto:{e}")) { (e) } } } }
            @if let Some(i) = f["issue"].as_object() {
                tr { th { "Issue" } td { a href=(format!("/ui/projects/{slug}/issues/{}", i.get("id").and_then(|v| v.as_str()).unwrap_or(""))) { code { (i.get("short_id").and_then(|v| v.as_str()).unwrap_or("")) } " " (i.get("title").and_then(|v| v.as_str()).unwrap_or("")) } } }
            }
            tr { th { "Source" } td { (f["source"].as_str().unwrap_or("")) } }
            tr { th { "Received" } td { (f["created_at"].as_str().unwrap_or("")) } }
            tr { th { "Release" } td { (f["release"].as_str().unwrap_or("")) " · " (f["environment"].as_str().unwrap_or("")) } }
            tr { th { "Where" } td { (f["url"].as_str().unwrap_or("")) } }
            tr { th { "Device" } td { (device_line.join(" · ")) " " (f["country"].as_str().unwrap_or("")) } }
            @if let Some(u) = f["user_id"].as_str() { tr { th { "User" } td { code { (u) } } } }
            @if let Some(sid) = f["session_id"].as_str() { tr { th { "Session" } td { code { (sid) } } } }
            @if f["flags"].as_object().map(|o| !o.is_empty()).unwrap_or(false) { tr { th { "Flags" } td { code { (f["flags"].to_string()) } } } }
            @if f["tags"].as_object().map(|o| !o.is_empty()).unwrap_or(false) { tr { th { "Tags" } td { code { (f["tags"].to_string()) } } } }
        } }
    })
}

#[derive(Deserialize)]
struct FeedbackForm {
    status: String,
}

async fn feedback_action(State(s): State<AppState>, headers: HeaderMap, Path((slug, id)): Path<(String, String)>, Form(f): Form<FeedbackForm>) -> Response {
    let user = require_user!(s, headers);
    if !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let to = format!("/ui/projects/{slug}/feedback/{id}");
    match crate::feedback::update(&s, &user, &id, crate::feedback::Update { status: Some(f.status), ..Default::default() }).await {
        Ok(_) => back(&to, "Updated"),
        Err(e) => back(&to, &format!("error:{}", e.1)),
    }
}

#[derive(Deserialize)]
struct IssueForm {
    status: String,
}

async fn issue_action(State(s): State<AppState>, headers: HeaderMap, Path((slug, id)): Path<(String, String)>, Form(f): Form<IssueForm>) -> Response {
    let user = require_user!(s, headers);
    if !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let to = format!("/ui/projects/{slug}/issues/{id}");
    match query::update_issue(&s, &user, &id, query::IssueUpdate { status: Some(f.status), ..Default::default() }).await {
        Ok(_) => back(&to, "Updated"),
        Err(e) => back(&to, &format!("error:{}", e.1)),
    }
}

async fn tokens_page(State(s): State<AppState>, headers: HeaderMap, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    if !user.is_admin() {
        return back("/ui", "error:Only admins can manage API tokens");
    }
    let rows = sqlx::query("SELECT t.id, t.name, t.token_prefix, t.scope, t.created_at, t.last_used_at, t.expires_at, p.slug FROM api_tokens t LEFT JOIN projects p ON p.id = t.project_id WHERE t.revoked_at IS NULL ORDER BY t.created_at DESC")
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
    let projects = crate::projects::list(&s).await.unwrap_or_default();
    page("API tokens", Some(&user), None, html! {
        h1 { "API tokens" }
        @if let Some(t) = q.get("new_token") {
            div.flash { "New token (copy it now, it won't be shown again): " br; code { (t) } }
        }
        @if let Some(f) = q.get("flash") { div.flash { (f) } }
        p.muted { "Tokens authenticate the REST API (" code { "/api/v1" } ") and MCP (" code { "/mcp" } "). Read tokens can only query; write tokens can also triage issues, upload debug files and create alert rules." }
        div.card { table {
            tr { th { "Name" } th { "Token" } th { "Project" } th { "Scope" } th { "Last used" } th {} }
            @for r in &rows {
                tr {
                    td { (r.s("name")) }
                    td { code { (r.s("token_prefix")) "…" } }
                    td { (r.os("slug").unwrap_or_else(|| "all projects".into())) }
                    td { (r.s("scope")) }
                    td.muted { (r.oi("last_used_at").map(iso_time).unwrap_or_else(|| "never".into())) }
                    td { form.inline method="post" { input type="hidden" name="revoke" value=(r.s("id")); button.danger type="submit" { "Revoke" } } }
                }
            }
        } }
        h2 { "New token" }
        div.card { form.row method="post" {
            input name="name" placeholder="Name, e.g. claude-code" required;
            select name="project" { option value="" { "All projects" } @for p in &projects { option value=(p.id) { (p.name) } } }
            select name="scope" { option value="read" { "read" } option value="write" { "write" } }
            input type="number" name="expires_in_days" placeholder="Expires in days (optional)" min="1";
            button type="submit" { "Create token" }
        } }
    })
}

#[derive(Deserialize)]
struct TokenForm {
    revoke: Option<String>,
    name: Option<String>,
    project: Option<String>,
    scope: Option<String>,
    expires_in_days: Option<String>,
}

async fn tokens_action(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<TokenForm>) -> Response {
    let user = require_user!(s, headers);
    if !user.is_admin() || !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(id) = f.revoke {
        let _ = sqlx::query("UPDATE api_tokens SET revoked_at = $1 WHERE id = $2").bind(now_ms()).bind(&id).execute(&s.db).await;
        auth::audit(&s, &user.label(), "revoke_token", &id, "ui").await;
        return back("/ui/tokens", "Token revoked");
    }
    let expires = f.expires_in_days.and_then(|d| d.parse::<i64>().ok()).map(|d| now_ms() + d * 86_400_000);
    let project = f.project.filter(|p| !p.is_empty());
    match auth::create_api_token(&s, f.name.as_deref().unwrap_or("token"), project.as_deref(), f.scope.as_deref() == Some("write"), expires, &user.label()).await {
        Ok(t) => {
            auth::audit(&s, &user.label(), "create_token", &t.id, "ui").await;
            Redirect::to(&format!("/ui/tokens?new_token={}", t.token)).into_response()
        }
        Err(e) => back("/ui/tokens", &format!("error:{e}")),
    }
}

async fn users_page(State(s): State<AppState>, headers: HeaderMap, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    if !user.is_admin() {
        return back("/ui", "error:Only admins can manage users");
    }
    let rows = sqlx::query("SELECT id, email, name, role, disabled, created_at FROM users ORDER BY created_at").fetch_all(&s.db).await.unwrap_or_default();
    let invites = sqlx::query("SELECT email, role, expires_at FROM invites WHERE accepted_at IS NULL AND expires_at > $1 ORDER BY created_at DESC")
        .bind(now_ms())
        .fetch_all(&s.db)
        .await
        .unwrap_or_default();
    page("Users", Some(&user), None, html! {
        h1 { "Users" }
        @if let Some(link) = q.get("invite_link") {
            div.flash { "Invite created. " @if q.get("emailed").map(|e| e == "true").unwrap_or(false) { "We emailed it, and here is the link too: " } @else { "Send this link to the person (email is not configured): " } br; code { (link) } }
        }
        @if let Some(f) = q.get("flash") { @if let Some(e) = f.strip_prefix("error:") { div.flash.err { (e) } } @else { div.flash { (f) } } }
        p.muted { "Roles: viewer (read), member (read + triage), admin (projects, tokens, users), owner (everything)." }
        div.card { table {
            tr { th { "Email" } th { "Name" } th { "Role" } th { "Status" } th {} }
            @for r in &rows {
                tr {
                    td { (r.s("email")) }
                    td { (r.s("name")) }
                    td { form.inline method="post" {
                        input type="hidden" name="user" value=(r.s("id"));
                        select name="role" { @for role in ["viewer", "member", "admin", "owner"] { option value=(role) selected[r.s("role") == role] { (role) } } }
                        " " button.ghost type="submit" name="action" value="role" { "Change" }
                    } }
                    td { @if r.i("disabled") != 0 { "disabled" } @else { "active" } }
                    td { form.inline method="post" {
                        input type="hidden" name="user" value=(r.s("id"));
                        @if r.i("disabled") != 0 { button.ghost type="submit" name="action" value="enable" { "Enable" } }
                        @else { button.danger type="submit" name="action" value="disable" { "Disable" } }
                    } }
                }
            }
        } }
        @if !invites.is_empty() {
            h2 { "Pending invites" }
            div.card { table { @for i in &invites { tr { td { (i.s("email")) } td { (i.s("role")) } td.muted { "expires " (iso_time(i.i("expires_at"))) } } } } }
        }
        h2 { "Invite someone" }
        div.card { form.row method="post" {
            input type="email" name="email" placeholder="Email" required;
            select name="role" { option value="viewer" { "viewer" } option value="member" selected { "member" } option value="admin" { "admin" } }
            button type="submit" name="action" value="invite" { "Invite" }
        } }
    })
}

#[derive(Deserialize)]
struct UserForm {
    action: String,
    user: Option<String>,
    role: Option<String>,
    email: Option<String>,
}

async fn users_action(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<UserForm>) -> Response {
    let user = require_user!(s, headers);
    if !user.is_admin() || !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match f.action.as_str() {
        "invite" => match crate::api::invite_inner(&s, &user, crate::api::InviteBody { email: f.email.unwrap_or_default(), role: f.role.unwrap_or_else(|| "member".into()) }).await {
            Ok(v) => Redirect::to(&format!(
                "/ui/users?invite_link={}&emailed={}",
                url::form_urlencoded::byte_serialize(v["invite_link"].as_str().unwrap_or("").as_bytes()).collect::<String>(),
                v["emailed"]
            ))
            .into_response(),
            Err(e) => back("/ui/users", &format!("error:{}", e.1)),
        },
        action @ ("role" | "disable" | "enable") => {
            let Some(id) = f.user else { return back("/ui/users", "error:missing user") };
            let row = sqlx::query("SELECT role FROM users WHERE id = $1").bind(&id).fetch_optional(&s.db).await.ok().flatten();
            let Some(row) = row else { return back("/ui/users", "error:user not found") };
            let current = row.s("role");
            let new_role = if action == "role" { f.role.clone().unwrap_or(current.clone()) } else { current.clone() };
            let my_role = match &user { Principal::User { role, .. } => *role, _ => Role::Viewer };
            if Role::parse(&new_role).map(|r| r > my_role).unwrap_or(true) {
                return back("/ui/users", "error:You cannot grant a higher role than your own");
            }
            let disabling = action == "disable";
            if (current == "owner" && (new_role != "owner" || disabling))
                && sqlx::query("SELECT COUNT(*) AS n FROM users WHERE role = 'owner' AND disabled = 0").fetch_one(&s.db).await.map(|r| r.i("n")).unwrap_or(0) <= 1
            {
                return back("/ui/users", "error:Cannot demote or disable the last owner");
            }
            let disabled: Option<i64> = match action { "disable" => Some(1), "enable" => Some(0), _ => None };
            let _ = sqlx::query("UPDATE users SET role = $1, disabled = COALESCE($2, disabled) WHERE id = $3").bind(&new_role).bind(disabled).bind(&id).execute(&s.db).await;
            if disabling {
                let _ = sqlx::query("DELETE FROM web_sessions WHERE user_id = $1").bind(&id).execute(&s.db).await;
            }
            auth::audit(&s, &user.label(), "update_user", &id, action).await;
            back("/ui/users", "Updated")
        }
        _ => back("/ui/users", "error:unknown action"),
    }
}

async fn alerts_page(State(s): State<AppState>, headers: HeaderMap, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    let rows = sqlx::query(&format!("SELECT {} FROM alert_rules ORDER BY created_at", crate::alerts::RULE_COLS)).fetch_all(&s.db).await.unwrap_or_default();
    let rules: Vec<_> = rows.iter().map(crate::alerts::rule_from_row).collect();
    let projects = crate::projects::list(&s).await.unwrap_or_default();
    let name_of = |id: &Option<String>| id.as_ref().and_then(|i| projects.iter().find(|p| &p.id == i).map(|p| p.name.clone())).unwrap_or_else(|| "All projects".into());
    page("Alerts", Some(&user), flash_of(&q).as_deref(), html! {
        h1 { "Alert rules" }
        p.muted { "Webhooks receive JSON (signed with HMAC-SHA256 in " code { "X-Vaka-Signature" } " when a secret is set). Email is sent through Resend." @if s.config.resend_api_key.is_none() { " Email is not configured: set VAKA_RESEND_API_KEY." } }
        div.card { table {
            tr { th { "Name" } th { "Project" } th { "Trigger" } th { "Channel" } th { "Target" } th {} }
            @for r in &rules {
                tr {
                    td { (r.name) @if !r.enabled { " " span.badge { "disabled" } } }
                    td { (name_of(&r.project_id)) }
                    td { (r.trigger) @if r.trigger == "spike" { span.muted { " ≥" (r.threshold) "/h" } } }
                    td { (r.channel) }
                    td { code { (r.target) } }
                    td { @if user.can_write(r.project_id.as_deref().unwrap_or("")) || user.is_admin() {
                        form.inline method="post" { input type="hidden" name="id" value=(r.id); button.ghost type="submit" name="action" value="test" { "Test" } }
                        " "
                        form.inline method="post" { input type="hidden" name="id" value=(r.id); button.danger type="submit" name="action" value="delete" { "Delete" } }
                    } }
                }
            }
        } }
        h2 { "New rule" }
        div.card { form.row method="post" {
            input type="hidden" name="action" value="create";
            select name="project" { option value="" { "All projects" } @for p in &projects { option value=(p.slug) { (p.name) } } }
            select name="trigger" { @for t in crate::alerts::TRIGGERS { option value=(t) { (t) } } }
            select name="channel" { @for c in crate::alerts::CHANNELS { option value=(c) { (c) } } }
            input name="target" placeholder="https://… or email" required style="width:240px";
            input name="secret" placeholder="Webhook secret (optional)";
            input type="number" name="threshold" placeholder="Spike: events/hour" min="1";
            button type="submit" { "Create" }
        } }
    })
}

#[derive(Deserialize)]
struct AlertForm {
    action: String,
    id: Option<String>,
    project: Option<String>,
    trigger: Option<String>,
    channel: Option<String>,
    target: Option<String>,
    secret: Option<String>,
    threshold: Option<String>,
}

async fn alerts_action(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<AlertForm>) -> Response {
    let user = require_user!(s, headers);
    if !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match f.action.as_str() {
        "create" => {
            let body = crate::api::RuleBody {
                project: f.project.filter(|p| !p.is_empty()),
                name: None,
                trigger: f.trigger,
                channel: f.channel,
                target: f.target,
                secret: f.secret.filter(|s| !s.is_empty()),
                threshold: f.threshold.and_then(|t| t.parse().ok()),
                enabled: Some(true),
                ..Default::default()
            };
            match crate::api::create_rule_inner(&s, &user, body).await {
                Ok(_) => back("/ui/alerts", "Rule created"),
                Err(e) => back("/ui/alerts", &format!("error:{}", e.1)),
            }
        }
        "test" => match crate::alerts::test_rule(&s, f.id.as_deref().unwrap_or("")).await {
            Ok(d) => back("/ui/alerts", &format!("Test sent: {d}")),
            Err(e) => back("/ui/alerts", &format!("error:Test failed: {e}")),
        },
        "delete" => {
            let id = f.id.unwrap_or_default();
            let row = sqlx::query("SELECT project_id FROM alert_rules WHERE id = $1").bind(&id).fetch_optional(&s.db).await.ok().flatten();
            let allowed = row.map(|r| r.os("project_id").filter(|p| !p.is_empty()).map(|p| user.can_write(&p)).unwrap_or(user.is_admin())).unwrap_or(false);
            if !allowed {
                return back("/ui/alerts", "error:Not allowed");
            }
            let _ = sqlx::query("DELETE FROM alert_rules WHERE id = $1").bind(&id).execute(&s.db).await;
            back("/ui/alerts", "Rule deleted")
        }
        _ => back("/ui/alerts", "error:unknown action"),
    }
}

async fn invite_page(State(s): State<AppState>, Path(token): Path<String>, axum::extract::Query(q): Q) -> Response {
    let row = sqlx::query("SELECT email, role FROM invites WHERE token_hash = $1 AND accepted_at IS NULL AND expires_at > $2")
        .bind(sha256_hex(&token))
        .bind(now_ms())
        .fetch_optional(&s.db)
        .await
        .ok()
        .flatten();
    let Some(row) = row else { return back("/ui/login", "error:This invite link is invalid or has expired") };
    page("Accept invite", None, flash_of(&q).as_deref(), html! {
        div.card style="max-width:420px;margin:60px auto" {
            h1 { "Join Vaka" }
            p { "You're invited as " strong { (row.s("role")) } " with " strong { (row.s("email")) } "." }
            form method="post" {
                p { input name="name" placeholder="Your name" required style="width:100%"; }
                p { input type="password" name="password" placeholder="Choose a password (min 8 characters)" minlength="8" required style="width:100%"; }
                button type="submit" { "Create account" }
            }
        }
    })
}

#[derive(Deserialize)]
struct AcceptForm {
    name: String,
    password: String,
}

async fn accept_invite(State(s): State<AppState>, Path(token): Path<String>, Form(f): Form<AcceptForm>) -> Response {
    let to = format!("/ui/invite/{token}");
    if f.password.len() < 8 {
        return back(&to, "error:Password must be at least 8 characters");
    }
    let row = sqlx::query("SELECT email, role FROM invites WHERE token_hash = $1 AND accepted_at IS NULL AND expires_at > $2")
        .bind(sha256_hex(&token))
        .bind(now_ms())
        .fetch_optional(&s.db)
        .await
        .ok()
        .flatten();
    let Some(row) = row else { return back("/ui/login", "error:This invite link is invalid or has expired") };
    let role = Role::parse(&row.s("role")).unwrap_or(Role::Viewer);
    let user_id = match auth::create_user(&s, &row.s("email"), f.name.trim(), &f.password, role).await {
        Ok(id) => id,
        Err(_) => return back("/ui/login", "error:An account with this email already exists; log in instead"),
    };
    let _ = sqlx::query("UPDATE invites SET accepted_at = $1 WHERE token_hash = $2").bind(now_ms()).bind(sha256_hex(&token)).execute(&s.db).await;
    let Ok(session) = auth::create_session(&s, &user_id).await else { return back("/ui/login", "Account created, please log in") };
    ([(header::SET_COOKIE, auth::session_cookie(&s, &session, 30 * 86400))], Redirect::to("/ui")).into_response()
}

async fn account_page(State(s): State<AppState>, headers: HeaderMap, axum::extract::Query(q): Q) -> Response {
    let user = require_user!(s, headers);
    let (name, role) = match &user { Principal::User { name, role, .. } => (name.clone(), role.as_str()), _ => (String::new(), "") };
    page("Account", Some(&user), flash_of(&q).as_deref(), html! {
        h1 { "Account" }
        p { (name) " · " span.badge { (role) } }
        div.card style="max-width:420px" { form method="post" {
            h2 { "Change password" }
            p { input type="password" name="current" placeholder="Current password" required style="width:100%"; }
            p { input type="password" name="new" placeholder="New password (min 8 characters)" minlength="8" required style="width:100%"; }
            button type="submit" { "Change password" }
        } }
    })
}

#[derive(Deserialize)]
struct PasswordForm {
    current: String,
    new: String,
}

async fn change_password(State(s): State<AppState>, headers: HeaderMap, Form(f): Form<PasswordForm>) -> Response {
    let user = require_user!(s, headers);
    if !same_origin(&s, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Principal::User { id, .. } = &user else { return back("/ui/account", "error:not a user") };
    let row = sqlx::query("SELECT password_hash FROM users WHERE id = $1").bind(id).fetch_one(&s.db).await;
    let Ok(row) = row else { return back("/ui/account", "error:user not found") };
    if !auth::verify_password(&f.current, &row.s("password_hash")) {
        return back("/ui/account", "error:Current password is wrong");
    }
    if f.new.len() < 8 {
        return back("/ui/account", "error:New password must be at least 8 characters");
    }
    let Ok(hash) = auth::hash_password(&f.new) else { return back("/ui/account", "error:could not hash password") };
    let _ = sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2").bind(hash).bind(id).execute(&s.db).await;
    back("/ui/account", "Password changed")
}

/// Email an invite link through Resend when configured. Returns whether it was sent.
pub async fn send_invite_email(s: &AppState, to: &str, link: &str) -> bool {
    send_email(s, to, "You're invited to Vaka", &format!("You have been invited to Vaka.\n\nAccept the invite (valid for 7 days):\n{link}\n")).await
}

/// Send a plain-text email through Resend when configured. Returns whether it was sent.
pub async fn send_email(s: &AppState, to: &str, subject: &str, text: &str) -> bool {
    let Some(key) = &s.config.resend_api_key else { return false };
    let res = s
        .http
        .post(format!("{}/emails", s.config.resend_api_url))
        .bearer_auth(key)
        .json(&serde_json::json!({
            "from": s.config.email_from,
            "to": [to],
            "subject": subject,
            "text": text,
        }))
        .send()
        .await;
    match res {
        Ok(r) if r.status().is_success() => true,
        Ok(r) => {
            tracing::warn!(status = %r.status(), "email delivery failed");
            false
        }
        Err(e) => {
            tracing::warn!("email delivery failed: {e}");
            false
        }
    }
}
