//! GitHub alert channels: `github_dispatch` (repository_dispatch, to start a workflow such as a
//! coding agent) and `github_issue` (open an issue with the fix context).

use crate::state::AppState;
use serde_json::{json, Value};
use std::time::Duration;

/// "owner/repo" from "owner/repo" or "https://github.com/owner/repo(.git)".
pub fn repo_slug(target: &str) -> Option<String> {
    let t = target.trim().trim_end_matches('/').trim_end_matches(".git");
    let t = t.strip_prefix("https://github.com/").or_else(|| t.strip_prefix("http://github.com/")).unwrap_or(t);
    let mut parts = t.split('/');
    let (o, r) = (parts.next()?, parts.next()?);
    if o.is_empty() || r.is_empty() || parts.next().is_some() || t.contains(':') {
        return None;
    }
    Some(format!("{o}/{r}"))
}

fn client(state: &AppState, method: reqwest::Method, path: &str, token: &str) -> reqwest::RequestBuilder {
    state
        .http
        .request(method, format!("{}{path}", state.config.github_api_url))
        .bearer_auth(token)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .header("user-agent", "Vaka")
        .timeout(Duration::from_secs(15))
}

async fn check(resp: reqwest::Response) -> anyhow::Result<Value> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("GitHub returned {status}: {}", crate::util::truncate(&text, 300));
    }
    Ok(serde_json::from_str(&text).unwrap_or(json!({"status": status.as_u16()})))
}

/// The slim payload sent as `client_payload` (GitHub allows at most 10 top-level keys).
pub fn client_payload(trigger: &str, payload: &Value) -> Value {
    let issue = &payload["issue"];
    let fix = &payload["fix_context"];
    let frames: Vec<Value> = fix["exceptions"][0]["in_app_frames"]
        .as_array()
        .into_iter()
        .flatten()
        .take(5)
        .map(|f| json!({"function": f["function"], "path": f["source"]["path"].as_str().or(f["filename"].as_str()), "line": f["lineno"], "url": f["source"]["url"]}))
        .collect();
    json!({
        "trigger": trigger,
        "project": payload["project"]["slug"],
        "issue": {
            "id": issue["id"], "short_id": issue["short_id"], "title": issue["title"], "culprit": issue["culprit"], "level": issue["level"],
            "times_seen": issue["times_seen"], "first_release": issue["first_release"], "last_release": issue["last_release"], "owners": issue["owners"],
        },
        "url": payload["url"],
        "api_url": payload["api_url"],
        "fix": {
            "repository": fix["repository"], "ref": fix["ref"], "frames": frames,
            "branch": fix["suggested"]["branch"], "commit_trailer": fix["suggested"]["commit_trailer"], "pr_title": fix["suggested"]["pr_title"],
            "suspect_commit": fix["suspect_commits"][0]["sha"],
        },
        "release": payload["release"],
        "sent_at": payload["sent_at"],
    })
}

pub async fn dispatch(state: &AppState, target: &str, token: &str, trigger: &str, payload: &Value) -> anyhow::Result<String> {
    if token.is_empty() {
        anyhow::bail!("github_dispatch needs a GitHub token in the rule's secret (contents: write)");
    }
    let repo = repo_slug(target).ok_or_else(|| anyhow::anyhow!("target must be owner/repo"))?;
    let body = json!({"event_type": format!("vaka.{trigger}"), "client_payload": client_payload(trigger, payload)});
    check(client(state, reqwest::Method::POST, &format!("/repos/{repo}/dispatches"), token).json(&body).send().await?).await?;
    Ok(format!("repository_dispatch vaka.{trigger} sent to {repo}"))
}

/// Markdown issue body from the fix context.
pub fn issue_body(payload: &Value, mention: Option<&str>) -> String {
    let issue = &payload["issue"];
    let fix = &payload["fix_context"];
    let short = issue["short_id"].as_str().unwrap_or("");
    let mut out = String::new();
    if let Some(m) = mention.filter(|m| !m.trim().is_empty()) {
        out.push_str(&format!("{}\n\n", m.trim()));
    }
    out.push_str(&format!(
        "**{}** in `{}`: seen {} times, first in release `{}`, last in `{}`. [Open in Vaka]({})\n\n",
        short,
        payload["project"]["slug"].as_str().unwrap_or(""),
        issue["times_seen"],
        issue["first_release"].as_str().unwrap_or("-"),
        issue["last_release"].as_str().unwrap_or("-"),
        payload["url"].as_str().unwrap_or("")
    ));
    if let Some(e) = fix["exceptions"].get(0) {
        out.push_str(&format!("```\n{}: {}\n```\n\n", e["type"].as_str().unwrap_or(""), e["value"].as_str().unwrap_or("")));
        let frames = e["in_app_frames"].as_array().cloned().unwrap_or_default();
        if !frames.is_empty() {
            out.push_str("### In-app frames (crash site first)\n\n");
            for f in frames.iter().take(10) {
                let loc = format!("{}:{}", f["source"]["path"].as_str().or(f["filename"].as_str()).unwrap_or("?"), f["lineno"].as_u64().map(|l| l.to_string()).unwrap_or_default());
                match f["source"]["url"].as_str() {
                    Some(u) => out.push_str(&format!("- `{}` at [{loc}]({u})\n", f["function"].as_str().unwrap_or("?"))),
                    None => out.push_str(&format!("- `{}` at `{loc}`\n", f["function"].as_str().unwrap_or("?"))),
                }
            }
            out.push('\n');
        }
    }
    if let Some(cs) = fix["suspect_commits"].as_array().filter(|c| !c.is_empty()) {
        out.push_str("### Suspect commits\n\n");
        for c in cs {
            out.push_str(&format!("- {} {} ({})\n", c["sha"].as_str().unwrap_or(""), c["message"].as_str().unwrap_or(""), c["author"].as_str().unwrap_or("")));
        }
        out.push('\n');
    }
    if let Some(o) = issue["owners"].as_array().filter(|o| !o.is_empty()) {
        out.push_str(&format!("Owners: {}\n\n", o.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if let Some(b) = fix["breadcrumbs"].as_array().filter(|b| !b.is_empty()) {
        out.push_str("<details><summary>Last breadcrumbs</summary>\n\n```\n");
        for c in b.iter().rev().take(10).rev() {
            out.push_str(&format!("{} {}\n", c["category"].as_str().unwrap_or(""), c["message"].as_str().unwrap_or("")));
        }
        out.push_str("```\n</details>\n\n");
    }
    out.push_str(&format!(
        "Fix context for agents: `get_fix_context` (MCP) or `GET {}/fix-context`. Put `Fixes {short}` in the fix commit so Vaka resolves the issue when the release ships.\n",
        payload["api_url"].as_str().unwrap_or("")
    ));
    crate::util::truncate(&out, 60_000)
}

pub async fn issue(state: &AppState, target: &str, token: &str, trigger: &str, issue_id: &str, payload: &Value, filters: &Value) -> anyhow::Result<String> {
    if token.is_empty() {
        anyhow::bail!("github_issue needs a GitHub token in the rule's secret (issues: write)");
    }
    let repo = repo_slug(target).ok_or_else(|| anyhow::anyhow!("target must be owner/repo"))?;
    let short = payload["issue"]["short_id"].as_str().unwrap_or("");
    // One GitHub issue per Vaka issue: later alerts become comments.
    let prefix = format!("https://github.com/{repo}/issues/");
    let existing = if issue_id.is_empty() {
        None
    } else {
        crate::fixctx::links_for(state, issue_id).await?.into_iter().find_map(|l| l["url"].as_str().filter(|u| u.starts_with(&prefix)).map(String::from))
    };
    if let Some(url) = existing {
        let number = url.trim_start_matches(&prefix).split(['/', '#']).next().unwrap_or("").to_string();
        let what = match trigger {
            "regression" => format!("{short} regressed in release `{}`.", payload["issue"]["last_release"].as_str().unwrap_or("-")),
            "spike" => format!("{short} is spiking: {} events in the last hour.", payload["details"]["last_hour"]),
            other => format!("Vaka `{other}` for {short}."),
        };
        let body = format!("{what} Seen {} times. [Open in Vaka]({})", payload["issue"]["times_seen"], payload["url"].as_str().unwrap_or(""));
        check(client(state, reqwest::Method::POST, &format!("/repos/{repo}/issues/{number}/comments"), token).json(&json!({"body": body})).send().await?).await?;
        return Ok(format!("commented on {url}"));
    }
    let title = if short.is_empty() { "Vaka test alert".to_string() } else { format!("[Vaka] {short}: {}", crate::util::truncate(payload["issue"]["title"].as_str().unwrap_or(""), 200)) };
    let body = json!({"title": title, "body": issue_body(payload, filters["mention"].as_str()), "labels": ["vaka"]});
    let v = check(client(state, reqwest::Method::POST, &format!("/repos/{repo}/issues"), token).json(&body).send().await?).await?;
    let url = v["html_url"].as_str().unwrap_or("").to_string();
    if !issue_id.is_empty() && url.starts_with("http") {
        crate::fixctx::insert_link(state, issue_id, "issue", &url, &title, "vaka").await?;
    }
    Ok(format!("opened {}", if url.is_empty() { repo } else { url }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs_and_payloads() {
        assert_eq!(repo_slug("acme/shop").as_deref(), Some("acme/shop"));
        assert_eq!(repo_slug("https://github.com/acme/shop.git").as_deref(), Some("acme/shop"));
        assert!(repo_slug("https://gitlab.com/a/b").is_none());
        assert!(repo_slug("acme").is_none());
        let p = json!({"project": {"slug": "web"}, "issue": {"short_id": "WEB-3", "title": "TypeError", "times_seen": 4},
            "fix_context": {"exceptions": [{"type": "TypeError", "value": "x", "in_app_frames": [{"function": "f", "lineno": 3, "source": {"path": "src/a.ts", "url": "https://github.com/a/b/blob/c/src/a.ts#L3"}}]}],
            "suggested": {"branch": "vaka/web-3-typeerror", "commit_trailer": "Fixes WEB-3"}, "suspect_commits": []}});
        let cp = client_payload("new_issue", &p);
        assert!(cp.as_object().unwrap().len() <= 10);
        assert_eq!(cp["fix"]["frames"][0]["path"], "src/a.ts");
        assert_eq!(cp["fix"]["branch"], "vaka/web-3-typeerror");
        let body = issue_body(&p, Some("@claude please fix"));
        assert!(body.starts_with("@claude please fix"));
        assert!(body.contains("[src/a.ts:3](https://github.com/a/b/blob/c/src/a.ts#L3)"), "{body}");
        assert!(body.contains("Fixes WEB-3"));
    }

    #[test]
    fn filters() {
        let issue = json!({"level": "error", "last_release": "2.1.0", "owners": ["a@x.se"]});
        assert!(crate::alerts::filters_match(&json!({}), &issue, None));
        assert!(crate::alerts::filters_match(&json!({"release": "2.*", "owner": "A@x.se"}), &issue, None));
        assert!(!crate::alerts::filters_match(&json!({"min_level": "fatal"}), &issue, None));
        let ev = json!({"environment": "staging", "handled": true, "tags": {"area": "checkout"}, "level": "error"});
        assert!(!crate::alerts::filters_match(&json!({"environment": "production"}), &issue, Some(&ev)));
        assert!(!crate::alerts::filters_match(&json!({"unhandled_only": true}), &issue, Some(&ev)));
        assert!(crate::alerts::filters_match(&json!({"tags": {"area": "check*"}}), &issue, Some(&ev)));
        assert!(crate::alerts::validate_filters(&json!({"bogus": 1})).is_err());
        assert!(crate::alerts::validate_filters(&json!({"min_level": "fatal", "tags": {"a": "b"}, "unhandled_only": true})).is_ok());
    }
}
