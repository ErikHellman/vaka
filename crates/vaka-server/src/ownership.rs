//! Code ownership rules (who owns an issue) and suspect commits (which commit in a release
//! probably introduced it). Both work on symbolicated stack frames.
//!
//! Ownership rules are CODEOWNERS-like lines, last matching rule wins:
//!
//! ```text
//! # kind:pattern            owners (emails or #team names)
//! path:src/payments/**      alice@example.com
//! src/cart/*.ts             bob@example.com          (no kind = path)
//! module:com.example.cart.* carol@example.com
//! type:TimeoutError         #platform-team
//! tag:area=checkout         dana@example.com
//! ```

use crate::auth::{ApiError, Principal};
use crate::db::RowExt;
use crate::query::{project_for, QResult};
use crate::state::AppState;
use crate::util::{iso_time, now_ms};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Matcher {
    Path(String),
    Module(String),
    Type(String),
    Tag(String, String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub matcher: Matcher,
    pub owners: Vec<String>,
    pub line: usize,
    pub text: String,
}

/// Parse rules; returns the rules and a list of human-readable problems (bad lines are skipped).
pub fn parse_rules(text: &str) -> (Vec<Rule>, Vec<String>) {
    let mut rules = Vec::new();
    let mut problems = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        // `#team` owners start with '#', so only treat '#' as a comment at the start or after whitespace.
        let line = if raw.trim_start().starts_with('#') { "" } else { strip_comment(raw.trim()) };
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let pattern = parts.next().unwrap_or("");
        let owners: Vec<String> = parts.map(String::from).collect();
        if owners.is_empty() {
            problems.push(format!("line {}: `{pattern}` has no owners", i + 1));
            continue;
        }
        let matcher = match pattern.split_once(':') {
            Some(("path", p)) => Matcher::Path(p.to_string()),
            Some(("module", m)) => Matcher::Module(m.to_string()),
            Some(("type", t)) => Matcher::Type(t.to_string()),
            Some(("tag", kv)) => match kv.split_once('=') {
                Some((k, v)) => Matcher::Tag(k.to_string(), v.to_string()),
                None => {
                    problems.push(format!("line {}: tag rules look like tag:key=value", i + 1));
                    continue;
                }
            },
            Some((kind, _)) if !kind.contains('/') && !kind.contains('*') && !kind.contains('.') => {
                problems.push(format!("line {}: unknown rule kind `{kind}` (use path, module, type or tag)", i + 1));
                continue;
            }
            _ => Matcher::Path(pattern.to_string()),
        };
        rules.push(Rule { matcher, owners, line: i + 1, text: line.to_string() });
    }
    (rules, problems)
}

fn strip_comment(s: &str) -> &str {
    // A comment starts with " #" followed by whitespace or end; "#team" owners are kept.
    let b = s.as_bytes();
    for i in 1..b.len() {
        if b[i] == b'#' && b[i - 1].is_ascii_whitespace() && (i + 1 == b.len() || b[i + 1].is_ascii_whitespace()) {
            return s[..i].trim_end();
        }
    }
    s
}

/// Glob match: `**` matches anything, `*` anything except `/`, `?` one character.
pub fn glob(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = if p.get(2) == Some(&b'/') { &p[3..] } else { &p[2..] };
                (0..=t.len()).any(|i| go(rest, &t[i..])) || go(&p[2..], t)
            }
            Some(b'*') => (0..=t.len()).take_while(|&i| i == 0 || t[i - 1] != b'/').any(|i| go(&p[1..], &t[i..])),
            Some(b'?') => !t.is_empty() && t[0] != b'/' && go(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// A path pattern matches a frame path anchored at the start (`/src/x`) or at any directory boundary.
fn path_matches(pattern: &str, path: &str) -> bool {
    if let Some(anchored) = pattern.strip_prefix('/') {
        return glob(anchored, path) || glob(&format!("{anchored}/**"), path);
    }
    let pat = pattern.trim_end_matches('/');
    let dir_pat = format!("{pat}/**");
    let mut starts = vec![0];
    starts.extend(path.match_indices('/').map(|(i, _)| i + 1));
    starts.into_iter().any(|s| glob(pat, &path[s..]) || (pattern.ends_with('/') || !pat.contains('*')) && glob(&dir_pat, &path[s..]))
}

/// Clean a frame path for matching: drop URL scheme/host, `webpack://`, `~/`, `./` and query strings.
pub fn clean_path(p: &str) -> String {
    let mut s = p.trim();
    for prefix in ["webpack:///", "webpack://", "app:///", "file://"] {
        if let Some(r) = s.strip_prefix(prefix) {
            s = r;
        }
    }
    let s = if let Some(i) = s.find("://") {
        let after = &s[i + 3..];
        after.find('/').map(|j| &after[j + 1..]).unwrap_or("")
    } else {
        s
    };
    let s = s.split(['?', '#']).next().unwrap_or("");
    let s = s.trim_start_matches("~/").trim_start_matches("./").trim_start_matches('/');
    s.replace('\\', "/")
}

/// Candidate repository paths for a frame, most specific first.
fn frame_paths(f: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for k in ["abs_path", "filename"] {
        if let Some(p) = f[k].as_str().filter(|p| !p.is_empty()) {
            let c = clean_path(p);
            if !c.is_empty() && !out.contains(&c) {
                out.push(c);
            }
        }
    }
    // JVM: module com.example.cart.CartViewModel + CartViewModel.kt -> com/example/cart/CartViewModel.kt
    if let (Some(m), Some(file)) = (f["module"].as_str(), f["filename"].as_str()) {
        if let Some((pkg, _)) = m.rsplit_once('.') {
            let p = format!("{}/{}", pkg.replace('.', "/"), file.rsplit('/').next().unwrap_or(file));
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

fn in_app_frames(exceptions: &Value) -> Vec<Value> {
    let mut frames = Vec::new();
    for e in exceptions.as_array().into_iter().flatten() {
        let st = if e["stacktrace"].is_array() { &e["stacktrace"] } else { &e["frames"] };
        for f in st.as_array().into_iter().flatten() {
            if f["in_app"].as_bool().unwrap_or(false) {
                frames.push(f.clone());
            }
        }
    }
    frames
}

/// Evaluate rules against an event. Frames are checked crash site first; the first frame that any
/// path/module rule matches decides (last matching rule wins within it). Type and tag rules apply
/// when no frame rule matched.
pub fn owners_for(rules: &[Rule], exceptions: &Value, tags: &Value) -> Option<(Vec<String>, String)> {
    for f in in_app_frames(exceptions) {
        let paths = frame_paths(&f);
        let module = f["module"].as_str().unwrap_or("");
        let hit = rules.iter().rev().find(|r| match &r.matcher {
            Matcher::Path(p) => paths.iter().any(|fp| path_matches(p, fp)),
            Matcher::Module(m) => !module.is_empty() && (glob(m, module) || glob(&m.replace('.', "/"), &module.replace('.', "/"))),
            _ => false,
        });
        if let Some(r) = hit {
            return Some((r.owners.clone(), format!("line {}: {}", r.line, r.text)));
        }
    }
    let ty = exceptions.get(0).and_then(|e| e["type"].as_str()).unwrap_or("");
    rules
        .iter()
        .rev()
        .find(|r| match &r.matcher {
            Matcher::Type(t) => !ty.is_empty() && (glob(t, ty) || ty.rsplit(['.', ':']).next() == Some(t.as_str())),
            Matcher::Tag(k, v) => tags.get(k).and_then(|x| x.as_str()).map(|x| glob(v, x)).unwrap_or(false),
            _ => false,
        })
        .map(|r| (r.owners.clone(), format!("line {}: {}", r.line, r.text)))
}

pub async fn rules_for(state: &AppState, project_id: &str) -> anyhow::Result<String> {
    Ok(sqlx::query("SELECT rules FROM ownership_rules WHERE project_id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.s("rules"))
        .unwrap_or_default())
}

/// On a new issue: record its owners and assign it to the first owner who is a Vaka user.
pub async fn assign_new_issue(state: &AppState, project_id: &str, issue_id: &str, exceptions: &Value, tags: &Value) -> anyhow::Result<()> {
    let text = rules_for(state, project_id).await?;
    if text.trim().is_empty() {
        return Ok(());
    }
    let (rules, _) = parse_rules(&text);
    let Some((owners, _)) = owners_for(&rules, exceptions, tags) else { return Ok(()) };
    sqlx::query("UPDATE issues SET owners = $1 WHERE id = $2").bind(owners.join(" ")).bind(issue_id).execute(&state.db).await?;
    for o in owners.iter().filter(|o| o.contains('@')) {
        if let Some(u) = sqlx::query("SELECT id FROM users WHERE LOWER(email) = $1 AND disabled = 0").bind(o.to_lowercase()).fetch_optional(&state.db).await? {
            sqlx::query("UPDATE issues SET assignee_id = $1 WHERE id = $2 AND assignee_id IS NULL").bind(u.s("id")).bind(issue_id).execute(&state.db).await?;
            break;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Suspect commits
// ---------------------------------------------------------------------------

/// Does a file changed in a commit correspond to a frame path? Exact suffix at a directory
/// boundary scores 1.0; same file name only (ambiguous) scores 0.3.
fn file_match(commit_file: &str, frame_path: &str) -> f64 {
    let cf = clean_path(commit_file);
    if cf.is_empty() || frame_path.is_empty() {
        return 0.0;
    }
    let suffix = |long: &str, short: &str| long == short || (long.ends_with(short) && long.as_bytes()[long.len() - short.len() - 1] == b'/');
    if frame_path.contains('/') && (suffix(&cf, frame_path) || suffix(frame_path, &cf)) {
        return 1.0;
    }
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    if base(&cf) == base(frame_path) {
        return 0.3;
    }
    0.0
}

/// Rank commits of the issue's first release (and the release before it, if it has commits) by how
/// many in-app frames, weighted toward the crash site, touch files they changed.
pub async fn suspect_commits(state: &AppState, project_id: &str, first_release: &str, exceptions: &Value) -> anyhow::Result<Value> {
    if first_release.is_empty() {
        return Ok(json!([]));
    }
    let rows = sqlx::query(
        "SELECT c.release, c.sha, c.message, c.author_name, c.author_email, c.committed_at, c.files, r.repository \
         FROM release_commits c LEFT JOIN releases r ON r.project_id = c.project_id AND r.version = c.release \
         WHERE c.project_id = $1 AND c.release = $2 ORDER BY c.position LIMIT 2000",
    )
    .bind(project_id)
    .bind(first_release)
    .fetch_all(&state.db)
    .await?;
    if rows.is_empty() {
        return Ok(json!([]));
    }
    let frames: Vec<(usize, Value, Vec<String>)> = in_app_frames(exceptions).into_iter().take(15).enumerate().map(|(i, f)| {
        let p = frame_paths(&f);
        (i, f, p)
    }).collect();
    let mut scored: Vec<(f64, Value)> = Vec::new();
    for r in &rows {
        let files_text = r.s("files");
        let files: Vec<&str> = files_text.lines().filter(|l| !l.is_empty()).collect();
        let mut score = 0.0;
        let mut reasons = Vec::new();
        for (i, f, paths) in &frames {
            let best = files.iter().flat_map(|cf| paths.iter().map(move |fp| (file_match(cf, fp), *cf))).fold((0.0, ""), |a, b| if b.0 > a.0 { b } else { a });
            if best.0 > 0.0 {
                score += best.0 / (1.0 + *i as f64);
                if reasons.len() < 3 {
                    reasons.push(format!(
                        "changed {} (frame {}: {}{})",
                        best.1,
                        i,
                        f["function"].as_str().unwrap_or("?"),
                        f["lineno"].as_u64().map(|l| format!(" line {l}")).unwrap_or_default()
                    ));
                }
            }
        }
        if score > 0.0 {
            let sha = r.s("sha");
            let repo = r.s("repository");
            let url = if repo.contains("github.com") || repo.contains("gitlab") {
                Some(format!("{}/commit/{sha}", repo.trim_end_matches('/').trim_end_matches(".git")))
            } else {
                None
            };
            scored.push((
                score,
                json!({
                    "sha": sha, "message": r.s("message").lines().next().unwrap_or(""), "author": r.s("author_name"), "author_email": r.s("author_email"),
                    "committed_at": crate::util::iso_time(r.i("committed_at")), "release": r.s("release"), "url": url,
                    "score": (score * 100.0).round() / 100.0, "reasons": reasons,
                }),
            ));
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(json!(scored.into_iter().take(3).map(|(_, v)| v).collect::<Vec<_>>()))
}

#[derive(Debug, serde::Deserialize)]
pub struct CommitIn {
    pub sha: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub author_email: String,
    /// RFC 3339, unix seconds or ms.
    #[serde(default)]
    pub timestamp: Option<Value>,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SetCommits {
    #[serde(default)]
    pub repository: Option<String>,
    pub commits: Vec<CommitIn>,
}

/// Replace the commits recorded for a release.
pub async fn set_release_commits(state: &AppState, project_id: &str, release: &str, body: SetCommits) -> anyhow::Result<usize> {
    let now = now_ms();
    sqlx::query("INSERT INTO releases (project_id, version, created_at) VALUES ($1, $2, $3) ON CONFLICT (project_id, version) DO NOTHING")
        .bind(project_id)
        .bind(release)
        .bind(now)
        .execute(&state.db)
        .await?;
    let mut tx = state.db.begin().await?;
    if let Some(repo) = body.repository.as_deref() {
        sqlx::query("UPDATE releases SET repository = $1 WHERE project_id = $2 AND version = $3").bind(repo).bind(project_id).bind(release).execute(&mut *tx).await?;
    }
    sqlx::query("DELETE FROM release_commits WHERE project_id = $1 AND release = $2").bind(project_id).bind(release).execute(&mut *tx).await?;
    let mut n = 0;
    for (pos, c) in body.commits.iter().take(1000).enumerate() {
        if c.sha.trim().is_empty() {
            continue;
        }
        let ts = c.timestamp.as_ref().and_then(crate::util::parse_timestamp).unwrap_or(0);
        let files: Vec<&str> = c.files.iter().take(1000).map(|f| f.as_str()).collect();
        sqlx::query(
            "INSERT INTO release_commits (project_id, release, sha, message, author_name, author_email, committed_at, files, position) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (project_id, release, sha) DO NOTHING",
        )
        .bind(project_id)
        .bind(release)
        .bind(c.sha.trim())
        .bind(crate::util::truncate(&c.message, 2000))
        .bind(crate::util::truncate(&c.author_name, 200))
        .bind(crate::util::truncate(&c.author_email, 200))
        .bind(ts)
        .bind(files.join("\n"))
        .bind(pos as i64)
        .execute(&mut *tx)
        .await?;
        n += 1;
    }
    tx.commit().await?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// Query layer (shared by REST and MCP)
// ---------------------------------------------------------------------------

fn rule_json(r: &Rule) -> Value {
    let (kind, pattern) = match &r.matcher {
        Matcher::Path(p) => ("path", p.clone()),
        Matcher::Module(m) => ("module", m.clone()),
        Matcher::Type(t) => ("type", t.clone()),
        Matcher::Tag(k, v) => ("tag", format!("{k}={v}")),
    };
    json!({"line": r.line, "kind": kind, "pattern": pattern, "owners": r.owners})
}

pub async fn get_ownership(state: &AppState, p: &Principal, project: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let row = sqlx::query("SELECT rules, updated_at, updated_by FROM ownership_rules WHERE project_id = $1").bind(&proj.id).fetch_optional(&state.db).await?;
    let text = row.as_ref().map(|r| r.s("rules")).unwrap_or_default();
    let (rules, problems) = parse_rules(&text);
    Ok(json!({
        "project": proj.slug,
        "rules": text,
        "parsed": rules.iter().map(rule_json).collect::<Vec<_>>(),
        "problems": problems,
        "updated_at": row.as_ref().map(|r| iso_time(r.i("updated_at"))),
        "updated_by": row.as_ref().map(|r| r.s("updated_by")),
    }))
}

/// Replace a project's ownership rules. Rejects the whole text if any line is invalid.
pub async fn set_ownership(state: &AppState, p: &Principal, project: &str, text: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    if text.len() > 64 * 1024 {
        return Err(ApiError::bad("rules are limited to 64 KB"));
    }
    let (rules, problems) = parse_rules(text);
    if !problems.is_empty() {
        return Err(ApiError::bad(format!("invalid ownership rules: {}", problems.join("; "))));
    }
    sqlx::query(
        "INSERT INTO ownership_rules (project_id, rules, updated_at, updated_by) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (project_id) DO UPDATE SET rules = excluded.rules, updated_at = excluded.updated_at, updated_by = excluded.updated_by",
    )
    .bind(&proj.id)
    .bind(text)
    .bind(now_ms())
    .bind(p.label())
    .execute(&state.db)
    .await?;
    Ok(json!({"project": proj.slug, "rules": rules.len(), "parsed": rules.iter().map(rule_json).collect::<Vec<_>>(),
        "note": "Rules apply to issues created from now on; existing issues keep their owners."}))
}

pub async fn get_release_commits(state: &AppState, p: &Principal, project: &str, release: &str) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    let repo = sqlx::query("SELECT repository FROM releases WHERE project_id = $1 AND version = $2")
        .bind(&proj.id)
        .bind(release)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("release"))?
        .s("repository");
    let rows = sqlx::query(
        "SELECT sha, message, author_name, author_email, committed_at, files FROM release_commits \
         WHERE project_id = $1 AND release = $2 ORDER BY position LIMIT 1000",
    )
    .bind(&proj.id)
    .bind(release)
    .fetch_all(&state.db)
    .await?;
    let commits: Vec<Value> = rows
        .iter()
        .map(|r| {
            let files = r.s("files");
            json!({"sha": r.s("sha"), "message": r.s("message"), "author_name": r.s("author_name"), "author_email": r.s("author_email"),
                "committed_at": iso_time(r.i("committed_at")), "files": files.lines().filter(|l| !l.is_empty()).collect::<Vec<_>>()})
        })
        .collect();
    Ok(json!({"project": proj.slug, "release": release, "repository": repo, "commits": commits}))
}

pub async fn put_release_commits(state: &AppState, p: &Principal, project: &str, release: &str, body: SetCommits) -> QResult<Value> {
    let proj = project_for(state, p, project).await?;
    if !p.can_write(&proj.id) {
        return Err(ApiError::forbidden());
    }
    if release.trim().is_empty() || release.len() > 200 {
        return Err(ApiError::bad("invalid release"));
    }
    let n = set_release_commits(state, &proj.id, release, body).await?;
    Ok(json!({"project": proj.slug, "release": release, "commits": n}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        assert!(glob("src/**", "src/a/b.ts"));
        assert!(glob("src/*.ts", "src/a.ts"));
        assert!(!glob("src/*.ts", "src/a/b.ts"));
        assert!(glob("**/cart.ts", "src/x/cart.ts"));
        assert!(glob("com.example.cart.*", "com.example.cart.CartViewModel"));
    }

    #[test]
    fn paths() {
        assert_eq!(clean_path("https://shop.example.com/assets/app.js?v=1"), "assets/app.js");
        assert_eq!(clean_path("webpack:///./src/cart.ts"), "src/cart.ts");
        assert!(path_matches("src/payments/", "home/ci/repo/src/payments/stripe.ts"));
        assert!(path_matches("src/payments", "src/payments/stripe.ts"));
        assert!(path_matches("/src/**", "src/a.ts"));
        assert!(!path_matches("/src/**", "lib/src/a.ts"));
        assert!(path_matches("*.rs", "crates/x/src/main.rs"));
    }

    #[test]
    fn rules_and_owners() {
        let (rules, problems) = parse_rules(
            "# owners\nsrc/**  dev@x.se\npath:src/payments/** pay@x.se #payments\nmodule:com.example.cart.* cart@x.se\ntype:TimeoutError ops@x.se # comment\ntag:area=checkout co@x.se\nbogus:thing a@b\nlonely\n",
        );
        assert_eq!(rules.len(), 5, "{rules:?}");
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(rules[1].owners, vec!["pay@x.se", "#payments"]);
        let ex = json!([{"type": "Error", "stacktrace": [{"function": "lib", "abs_path": "https://x.se/node_modules/a.js", "in_app": false}, {"function": "charge", "abs_path": "/build/src/payments/stripe.ts", "in_app": true}]}]);
        let (owners, why) = owners_for(&rules, &ex, &json!({})).unwrap();
        assert_eq!(owners, vec!["pay@x.se", "#payments"], "{why}");
        let jvm = json!([{"type": "java.lang.IllegalStateException", "stacktrace": [{"module": "com.example.cart.CartViewModel", "filename": "CartViewModel.kt", "in_app": true}]}]);
        assert_eq!(owners_for(&rules, &jvm, &json!({})).unwrap().0, vec!["cart@x.se"]);
        let timeout = json!([{"type": "TimeoutError", "stacktrace": []}]);
        assert_eq!(owners_for(&rules, &timeout, &json!({})).unwrap().0, vec!["ops@x.se"]);
        assert_eq!(owners_for(&rules, &json!([{"type": "X"}]), &json!({"area": "checkout"})).unwrap().0, vec!["co@x.se"]);
        assert!(owners_for(&rules, &json!([{"type": "X"}]), &json!({})).is_none());
    }

    #[test]
    fn commit_files() {
        assert_eq!(file_match("crates/app/src/main.rs", "home/ci/vaka/crates/app/src/main.rs"), 1.0);
        assert_eq!(file_match("src/app.ts", "src/app.ts"), 1.0);
        assert_eq!(file_match("app/src/main/java/com/x/Cart.kt", "com/x/Cart.kt"), 1.0);
        assert_eq!(file_match("other/app.ts", "src/app.ts"), 0.3);
        assert_eq!(file_match("src/other.ts", "src/app.ts"), 0.0);
        assert_eq!(file_match("src/myapp.ts", "app.ts"), 0.0);
    }
}
