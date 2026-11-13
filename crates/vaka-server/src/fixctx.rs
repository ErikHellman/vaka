//! Source and commit linking: code mappings (stack path -> repository path), source links on
//! frames, issue links (PRs, tickets), fix-by-commit (`Fixes WEB-12`) and the compact "fix
//! context" bundle that coding agents use to go from an issue to a pull request.

use crate::auth::{ApiError, Principal};
use crate::db::RowExt;
use crate::ownership::{clean_path, frame_paths};
use crate::query::{project_for, QResult};
use crate::state::AppState;
use crate::util::{iso_time, new_id, now_ms, truncate};
use serde_json::{json, Value};

pub const LINK_KINDS: &[&str] = &["pull_request", "issue", "commit", "other"];

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CodeMapping {
    /// Prefix of the (cleaned) frame path, e.g. `build/src/` or `com/example/` ("" matches all).
    #[serde(default)]
    pub stack_root: String,
    /// What the prefix is replaced with to get a repository path, e.g. `src/` or `app/src/main/java/com/example/`.
    #[serde(default)]
    pub source_root: String,
    /// Repository web URL, e.g. https://github.com/acme/shop.
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub default_branch: String,
}

fn norm_root(s: &str) -> String {
    let c = clean_path(s);
    if c.is_empty() || c.ends_with('/') {
        c
    } else {
        format!("{c}/")
    }
}

/// Map a cleaned frame path through the first matching mapping (longest stack root first).
pub fn map_path<'a>(mappings: &'a [CodeMapping], path: &str) -> Option<(String, &'a CodeMapping)> {
    let mut sorted: Vec<&CodeMapping> = mappings.iter().collect();
    sorted.sort_by_key(|m| std::cmp::Reverse(norm_root(&m.stack_root).len()));
    for m in sorted {
        let root = norm_root(&m.stack_root);
        let rest = if root.is_empty() {
            Some(path)
        } else if let Some(r) = path.strip_prefix(root.as_str()) {
            Some(r)
        } else {
            // Also match at a directory boundary: /home/ci/build/src/x -> src/x with stack_root "src/".
            path.match_indices('/').map(|(i, _)| &path[i + 1..]).find_map(|tail| tail.strip_prefix(root.as_str()))
        };
        if let Some(rest) = rest {
            return Some((format!("{}{rest}", norm_root(&m.source_root)), m));
        }
    }
    None
}

/// Web URL of a file at a ref and line for GitHub, GitLab, Bitbucket (others get the GitHub form).
pub fn source_url(repository: &str, git_ref: &str, path: &str, line: Option<u64>) -> String {
    let repo = repository.trim_end_matches('/').trim_end_matches(".git");
    let r = if git_ref.is_empty() { "HEAD" } else { git_ref };
    if repo.contains("gitlab") {
        format!("{repo}/-/blob/{r}/{path}{}", line.map(|l| format!("#L{l}")).unwrap_or_default())
    } else if repo.contains("bitbucket") {
        format!("{repo}/src/{r}/{path}{}", line.map(|l| format!("#lines-{l}")).unwrap_or_default())
    } else {
        format!("{repo}/blob/{r}/{path}{}", line.map(|l| format!("#L{l}")).unwrap_or_default())
    }
}

pub async fn mappings_for(state: &AppState, project_id: &str) -> anyhow::Result<Vec<CodeMapping>> {
    Ok(sqlx::query("SELECT stack_root, source_root, repository, default_branch FROM code_mappings WHERE project_id = $1 ORDER BY position")
        .bind(project_id)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| CodeMapping { stack_root: r.s("stack_root"), source_root: r.s("source_root"), repository: r.s("repository"), default_branch: r.s("default_branch") })
        .collect())
}

/// Repository and git ref for a release: the release's own repository/head commit, else the
/// newest release in the project that has a repository.
pub async fn release_repo(state: &AppState, project_id: &str, release: &str) -> anyhow::Result<(String, String)> {
    let row = sqlx::query("SELECT repository, head_commit FROM releases WHERE project_id = $1 AND version = $2")
        .bind(project_id)
        .bind(release)
        .fetch_optional(&state.db)
        .await?;
    let (mut repo, head) = row.map(|r| (r.s("repository"), r.s("head_commit"))).unwrap_or_default();
    if repo.is_empty() {
        repo = sqlx::query("SELECT repository FROM releases WHERE project_id = $1 AND repository != '' ORDER BY created_at DESC LIMIT 1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await?
            .map(|r| r.s("repository"))
            .unwrap_or_default();
    }
    Ok((repo, head))
}

/// Source location for one frame, or None when no repository is known.
pub fn frame_source(f: &Value, mappings: &[CodeMapping], release_repo: &str, head: &str) -> Option<Value> {
    let paths = frame_paths(f);
    let line = f["lineno"].as_u64();
    // Most specific mapping first, trying every candidate path of the frame against it.
    let mut sorted: Vec<&CodeMapping> = mappings.iter().collect();
    sorted.sort_by_key(|m| std::cmp::Reverse(norm_root(&m.stack_root).len()));
    for (path, m) in sorted.iter().flat_map(|m| paths.iter().filter_map(move |p| map_path(std::slice::from_ref(*m), p))) {
        {
            let repo = if m.repository.is_empty() { release_repo } else { m.repository.as_str() };
            if repo.is_empty() {
                continue;
            }
            // Pin to the release's head commit when it is the same repository.
            let git_ref = if !head.is_empty() && (m.repository.is_empty() || same_repo(&m.repository, release_repo)) { head.to_string() } else { m.default_branch.clone() };
            return Some(json!({"repository": repo, "path": path, "line": line, "ref": git_ref, "url": source_url(repo, &git_ref, &path, line), "mapped": true}));
        }
    }
    if release_repo.is_empty() || !mappings.is_empty() {
        return None;
    }
    // No mappings: best guess from the symbolicated/relative file name.
    let guess = f["filename"].as_str().map(clean_path).filter(|p| p.contains('/') || f["symbolicated"] == json!(true)).or_else(|| paths.first().cloned())?;
    Some(json!({"repository": release_repo, "path": guess, "line": line, "ref": head, "url": source_url(release_repo, head, &guess, line), "mapped": false}))
}

fn same_repo(a: &str, b: &str) -> bool {
    let n = |s: &str| s.trim_end_matches('/').trim_end_matches(".git").to_lowercase();
    n(a) == n(b)
}

/// Add `source` to every in-app frame of an event (in place).
pub async fn annotate_event(state: &AppState, project_id: &str, ev: &mut Value) {
    if !ev["exceptions"].is_array() {
        return;
    }
    let release = ev["release"].as_str().unwrap_or("").to_string();
    let (mappings, (repo, head)) = match (mappings_for(state, project_id).await, release_repo(state, project_id, &release).await) {
        (Ok(m), Ok(r)) => (m, r),
        _ => return,
    };
    if mappings.is_empty() && repo.is_empty() {
        return;
    }
    for e in ev["exceptions"].as_array_mut().into_iter().flatten() {
        let key = if e["stacktrace"].is_array() { "stacktrace" } else { "frames" };
        for f in e[key].as_array_mut().into_iter().flatten() {
            if f["in_app"].as_bool() != Some(true) {
                continue;
            }
            if let Some(src) = frame_source(f, &mappings, &repo, &head) {
                f["source"] = src;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Code mappings API
// ---------------------------------------------------------------------------

pub async fn get_code_mappings(state: &AppState, p: &Principal, project: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    Ok(json!({"project": proj.slug, "mappings": mappings_for(state, &proj.id).await?}))
}

pub async fn set_code_mappings(state: &AppState, p: &Principal, project: &str, mappings: Vec<CodeMapping>) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    if mappings.len() > 50 {
        return Err(ApiError::bad("at most 50 code mappings"));
    }
    for m in &mappings {
        if !m.repository.is_empty() && !m.repository.starts_with("https://") && !m.repository.starts_with("http://") {
            return Err(ApiError::bad(format!("repository must be a web URL like https://github.com/acme/shop, got `{}`", m.repository)));
        }
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM code_mappings WHERE project_id = $1").bind(&proj.id).execute(&mut *tx).await?;
    for (i, m) in mappings.iter().enumerate() {
        sqlx::query(
            "INSERT INTO code_mappings (id, project_id, stack_root, source_root, repository, default_branch, position, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(new_id())
        .bind(&proj.id)
        .bind(truncate(m.stack_root.trim(), 500))
        .bind(truncate(m.source_root.trim(), 500))
        .bind(truncate(m.repository.trim(), 500))
        .bind(truncate(m.default_branch.trim(), 200))
        .bind(i as i64)
        .bind(now_ms())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    crate::auth::audit(state, &p.label(), "set_code_mappings", &proj.id, &format!("{} mappings", mappings.len())).await;
    Ok(json!({"project": proj.slug, "mappings": mappings}))
}

// ---------------------------------------------------------------------------
// Issue links
// ---------------------------------------------------------------------------

pub async fn links_for(state: &AppState, issue_id: &str) -> anyhow::Result<Vec<Value>> {
    Ok(sqlx::query("SELECT id, kind, url, title, created_by, created_at FROM issue_links WHERE issue_id = $1 ORDER BY created_at")
        .bind(issue_id)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| json!({"id": r.s("id"), "kind": r.s("kind"), "url": r.s("url"), "title": r.s("title"), "created_by": r.s("created_by"), "created_at": iso_time(r.i("created_at"))}))
        .collect())
}

pub async fn insert_link(state: &AppState, issue_id: &str, kind: &str, url: &str, title: &str, by: &str) -> anyhow::Result<String> {
    let existing = sqlx::query("SELECT id FROM issue_links WHERE issue_id = $1 AND url = $2").bind(issue_id).bind(url).fetch_optional(&state.db).await?;
    if let Some(r) = existing {
        return Ok(r.s("id"));
    }
    let id = new_id();
    sqlx::query("INSERT INTO issue_links (id, issue_id, kind, url, title, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(&id)
        .bind(issue_id)
        .bind(kind)
        .bind(truncate(url, 2000))
        .bind(truncate(title, 500))
        .bind(by)
        .bind(now_ms())
        .execute(&state.db)
        .await?;
    Ok(id)
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct LinkIn {
    pub url: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

fn guess_kind(url: &str) -> &'static str {
    if url.contains("/pull/") || url.contains("/merge_requests/") || url.contains("/pull-requests/") {
        "pull_request"
    } else if url.contains("/commit/") || url.contains("/commits/") {
        "commit"
    } else if url.contains("/issues/") {
        "issue"
    } else {
        "other"
    }
}

pub async fn add_link(state: &AppState, p: &Principal, issue: &str, l: LinkIn) -> QResult<Value> {
    let issue = crate::query::issue_for(state, p, issue).await?;
    if !p.can_write(issue["project_id"].as_str().unwrap_or("")) {
        return Err(ApiError::forbidden());
    }
    let url = l.url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(ApiError::bad("url must start with https://"));
    }
    let kind = l.kind.clone().filter(|k| !k.is_empty()).unwrap_or_else(|| guess_kind(url).to_string());
    if !LINK_KINDS.contains(&kind.as_str()) {
        return Err(ApiError::bad(format!("kind must be one of {}", LINK_KINDS.join(", "))));
    }
    let issue_id = issue["id"].as_str().unwrap_or("");
    let id = insert_link(state, issue_id, &kind, url, l.title.as_deref().unwrap_or(""), &p.label()).await?;
    Ok(json!({"issue": issue["short_id"], "id": id, "links": links_for(state, issue_id).await?}))
}

pub async fn delete_link(state: &AppState, p: &Principal, issue: &str, link: &str) -> QResult<Value> {
    let issue = crate::query::issue_for(state, p, issue).await?;
    if !p.can_write(issue["project_id"].as_str().unwrap_or("")) {
        return Err(ApiError::forbidden());
    }
    let issue_id = issue["id"].as_str().unwrap_or("");
    let n = sqlx::query("DELETE FROM issue_links WHERE issue_id = $1 AND id = $2").bind(issue_id).bind(link).execute(&state.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::not_found("link"));
    }
    Ok(json!({"issue": issue["short_id"], "links": links_for(state, issue_id).await?}))
}

// ---------------------------------------------------------------------------
// Fix-by-commit
// ---------------------------------------------------------------------------

static FIX_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"(?i)\b(?:fix(?:e[sd])?|close[sd]?|resolve[sd]?)\b:?\s+((?:[A-Za-z0-9][A-Za-z0-9_-]*-\d+)(?:\s*(?:,|and)\s*[A-Za-z0-9][A-Za-z0-9_-]*-\d+)*)").unwrap()
});

/// Short ids (upper-cased) referenced by `Fixes X-1`, `closes X-1, X-2`, `Resolved: X-3` in a message.
pub fn fixed_short_ids(message: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in FIX_RE.captures_iter(message) {
        for id in c[1].split(|ch: char| ch == ',' || ch.is_whitespace()).map(str::trim).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("and")) {
            let id = id.to_uppercase();
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Resolve issues referenced as fixed by a release's commits. Returns the resolved short ids.
pub async fn resolve_from_commits(state: &AppState, project_id: &str, slug: &str, release: &str, commits: &[(String, String, String)]) -> anyhow::Result<Vec<String>> {
    let prefix = format!("{}-", slug.to_uppercase());
    let mut resolved = Vec::new();
    for (sha, message, repo) in commits {
        for short in fixed_short_ids(message) {
            let Some(n) = short.strip_prefix(&prefix).and_then(|n| n.parse::<i64>().ok()) else { continue };
            let Some(row) = sqlx::query("SELECT id, status FROM issues WHERE project_id = $1 AND short_id = $2").bind(project_id).bind(n).fetch_optional(&state.db).await? else {
                continue;
            };
            let issue_id = row.s("id");
            let now = now_ms();
            if row.s("status") != "resolved" {
                sqlx::query("UPDATE issues SET status = 'resolved', resolved_at = $1, resolved_in_release = $2, ignored_until = NULL WHERE id = $3")
                    .bind(now)
                    .bind(release)
                    .bind(&issue_id)
                    .execute(&state.db)
                    .await?;
                sqlx::query("INSERT INTO issue_comments (id, issue_id, author, body, created_at) VALUES ($1, $2, $3, $4, $5)")
                    .bind(new_id())
                    .bind(&issue_id)
                    .bind("vaka")
                    .bind(format!("Resolved in release {release} by commit {} ({}).", &sha[..sha.len().min(12)], message.lines().next().unwrap_or("")))
                    .bind(now)
                    .execute(&state.db)
                    .await?;
                crate::alerts::notify_issue_by_project(state, project_id, &issue_id, "issue_resolved", Some(json!({"actor": "vaka", "resolved_in_release": release, "commit": sha})));
                if !resolved.contains(&short) {
                    resolved.push(short.clone());
                }
            }
            if repo.starts_with("http") {
                let url = format!("{}/commit/{sha}", repo.trim_end_matches('/').trim_end_matches(".git"));
                insert_link(state, &issue_id, "commit", &url, message.lines().next().unwrap_or(""), "vaka").await?;
            }
        }
    }
    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Fix context
// ---------------------------------------------------------------------------

pub fn slugify(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= max {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

fn slim_frame(f: &Value, index: usize) -> Value {
    let mut v = json!({
        "index": index,
        "function": f["function"], "module": f["module"], "filename": f["filename"], "abs_path": f["abs_path"],
        "lineno": f["lineno"], "colno": f["colno"], "source": f.get("source").cloned().unwrap_or(Value::Null),
    });
    for k in ["pre_context", "context_line", "post_context"] {
        if !f[k].is_null() {
            v[k] = f[k].clone();
        }
    }
    v
}

/// Everything an agent needs to fix an issue, in one response.
pub async fn fix_context(state: &AppState, p: &Principal, id: &str) -> QResult<Value> {
    let issue = crate::query::get_issue(state, p, id).await?;
    let ev = &issue["latest_event"];
    let short = issue["short_id"].as_str().unwrap_or("").to_string();
    let mut exceptions = Vec::new();
    let mut first_source = Value::Null;
    for e in ev["exceptions"].as_array().into_iter().flatten() {
        let st = if e["stacktrace"].is_array() { &e["stacktrace"] } else { &e["frames"] };
        let all: Vec<&Value> = st.as_array().map(|a| a.iter().collect()).unwrap_or_default();
        let in_app: Vec<Value> = all.iter().enumerate().filter(|(_, f)| f["in_app"].as_bool() == Some(true)).take(20).map(|(i, f)| slim_frame(f, i)).collect();
        if first_source.is_null() {
            if let Some(s) = in_app.iter().find(|f| !f["source"].is_null()) {
                first_source = s["source"].clone();
            }
        }
        exceptions.push(json!({"type": e["type"], "value": e["value"], "frames_total": all.len(), "in_app_frames": in_app,
            "top_frames": if in_app.is_empty() { all.iter().take(10).enumerate().map(|(i, f)| slim_frame(f, i)).collect::<Vec<_>>() } else { vec![] }}));
    }
    let crumbs: Vec<Value> = ev["breadcrumbs"].as_array().map(|b| b.iter().rev().take(20).rev().cloned().collect()).unwrap_or_default();
    let title = issue["title"].as_str().unwrap_or("");
    let links = issue["links"].clone();
    let open_prs: Vec<Value> = links.as_array().into_iter().flatten().filter(|l| l["kind"] == "pull_request").cloned().collect();
    let mut next = vec![
        "Read the in-app frames' source (source.url or source.path at source.line) and the breadcrumbs to find the root cause.".to_string(),
        format!("Put `Fixes {short}` in the commit message; when the release's commits are sent to Vaka (`vaka releases set-commits`) the issue is resolved in that release."),
        format!("After opening a PR, record it with link_issue (issue {short}, url) so other agents don't duplicate the work."),
    ];
    if !open_prs.is_empty() {
        next.insert(0, format!("A pull request is already linked ({}); check it before starting new work.", open_prs[0]["url"].as_str().unwrap_or("")));
    }
    Ok(json!({
        "issue": {
            "id": issue["id"], "short_id": short, "title": title, "culprit": issue["culprit"], "level": issue["level"], "status": issue["status"],
            "platform": issue["platform"], "first_seen": issue["first_seen"], "last_seen": issue["last_seen"], "times_seen": issue["times_seen"],
            "first_release": issue["first_release"], "last_release": issue["last_release"], "assignee": issue["assignee"], "owners": issue["owners"],
            "url": issue["url"], "stats": issue["stats"],
        },
        "event": {"event_id": ev["event_id"], "timestamp": ev["timestamp"], "release": ev["release"], "environment": ev["environment"], "message": ev["message"],
            "handled": ev["handled"], "url": ev["url"], "device": ev["device"], "tags": ev["tags"], "contexts": ev["contexts"]},
        "exceptions": exceptions,
        "breadcrumbs": crumbs,
        "repository": first_source.get("repository").cloned().unwrap_or(Value::Null),
        "ref": first_source.get("ref").cloned().unwrap_or(Value::Null),
        "suspect_commits": issue["suspect_commits"],
        "links": links,
        "comments": issue["comments"].as_array().map(|c| c.iter().rev().take(5).rev().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "timeline_before_crash": issue.get("timeline_before_crash").cloned().unwrap_or(Value::Null),
        "suggested": {
            "branch": format!("vaka/{}-{}", short.to_lowercase(), slugify(title, 40)),
            "commit_trailer": format!("Fixes {short}"),
            "pr_title": format!("Fix {short}: {}", truncate(title, 80)),
        },
        "next_steps": next,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_and_urls() {
        let m = vec![
            CodeMapping { stack_root: "".into(), source_root: "web/".into(), repository: "".into(), default_branch: "main".into() },
            CodeMapping { stack_root: "com/example/".into(), source_root: "app/src/main/java/com/example/".into(), repository: "https://github.com/a/android".into(), default_branch: "dev".into() },
        ];
        assert_eq!(map_path(&m, "com/example/Cart.kt").unwrap().0, "app/src/main/java/com/example/Cart.kt");
        assert_eq!(map_path(&m, "src/cart.ts").unwrap().0, "web/src/cart.ts");
        let m2 = vec![CodeMapping { stack_root: "src".into(), source_root: "packages/web/src".into(), repository: String::new(), default_branch: String::new() }];
        assert_eq!(map_path(&m2, "home/ci/build/src/a/b.ts").unwrap().0, "packages/web/src/a/b.ts");
        assert!(map_path(&m2, "lib/x.ts").is_none());
        assert_eq!(source_url("https://github.com/a/b.git", "abc", "src/x.ts", Some(4)), "https://github.com/a/b/blob/abc/src/x.ts#L4");
        assert_eq!(source_url("https://gitlab.com/a/b", "", "x.ts", None), "https://gitlab.com/a/b/-/blob/HEAD/x.ts");
        assert_eq!(source_url("https://bitbucket.org/a/b", "main", "x.ts", Some(2)), "https://bitbucket.org/a/b/src/main/x.ts#lines-2");
        let f = json!({"filename": "src/checkout/total.ts", "abs_path": "webpack:///./src/checkout/total.ts", "lineno": 4, "in_app": true});
        let s = frame_source(&f, &[], "https://github.com/acme/shop", "deadbee").unwrap();
        assert_eq!(s["url"], "https://github.com/acme/shop/blob/deadbee/src/checkout/total.ts#L4");
        assert_eq!(s["mapped"], false);
        let s = frame_source(&f, &m2, "https://github.com/acme/shop", "deadbee").unwrap();
        assert_eq!(s["path"], "packages/web/src/checkout/total.ts");
        assert_eq!(s["ref"], "deadbee");
        let jvm = json!({"module": "com.example.Cart", "filename": "Cart.kt", "lineno": 9});
        let s = frame_source(&jvm, &m, "https://github.com/a/web", "abc").unwrap();
        assert_eq!(s["url"], "https://github.com/a/android/blob/dev/app/src/main/java/com/example/Cart.kt#L9");
    }

    #[test]
    fn fix_references() {
        assert_eq!(fixed_short_ids("Guard against null cart\n\nFixes WEB-12"), vec!["WEB-12"]);
        assert_eq!(fixed_short_ids("fix: closes web-3, WEB-4 and WEB-5; resolved: APP-1"), vec!["WEB-3", "WEB-4", "WEB-5", "APP-1"]);
        assert!(fixed_short_ids("Prefix fixture for WEB-2").is_empty());
        assert!(fixed_short_ids("fixes the thing").is_empty());
        assert_eq!(slugify("TypeError: x is undefined!", 40), "typeerror-x-is-undefined");
        assert_eq!(guess_kind("https://github.com/a/b/pull/3"), "pull_request");
    }
}
