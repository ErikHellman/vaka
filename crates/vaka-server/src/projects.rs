//! Project lookups shared by ingest, API and MCP.

use crate::db::RowExt;
use crate::state::{AppState, ProjectInfo};
use crate::util::now_ms;
use sqlx::any::AnyRow;

const KEY_CACHE_TTL_MS: i64 = 60_000;

pub fn from_row(r: &AnyRow) -> ProjectInfo {
    ProjectInfo {
        id: r.s("id"),
        slug: r.s("slug"),
        name: r.s("name"),
        platform: r.s("platform"),
        retention_days: r.i("retention_days"),
        scrub_enabled: r.i("scrub_enabled") != 0,
        scrub_fields: r.s("scrub_fields"),
    }
}

pub const COLS: &str = "id, slug, name, platform, retention_days, scrub_enabled, scrub_fields";

pub async fn by_ingest_key(state: &AppState, key: &str) -> anyhow::Result<Option<ProjectInfo>> {
    let now = now_ms();
    if let Some((p, at)) = state.key_cache.read().await.get(key) {
        if now - at < KEY_CACHE_TTL_MS {
            return Ok(Some(p.clone()));
        }
    }
    let row = sqlx::query(&format!(
        "SELECT p.id, p.slug, p.name, p.platform, p.retention_days, p.scrub_enabled, p.scrub_fields \
         FROM ingest_keys k JOIN projects p ON p.id = k.project_id \
         WHERE k.key = $1 AND k.revoked_at IS NULL"
    ))
    .bind(key)
    .fetch_optional(&state.db)
    .await?;
    let p = row.as_ref().map(from_row);
    if let Some(p) = &p {
        state.key_cache.write().await.insert(key.to_string(), (p.clone(), now));
    }
    Ok(p)
}

/// Find a project by id or slug.
pub async fn find(state: &AppState, id_or_slug: &str) -> anyhow::Result<Option<ProjectInfo>> {
    let row = sqlx::query(&format!(
        "SELECT {COLS} FROM projects WHERE id = $1 OR slug = $1"
    ))
    .bind(id_or_slug)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.as_ref().map(from_row))
}

pub async fn list(state: &AppState) -> anyhow::Result<Vec<ProjectInfo>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLS} FROM projects ORDER BY name"
    ))
    .fetch_all(&state.db)
    .await?;
    Ok(rows.iter().map(from_row).collect())
}

pub fn slugify(name: &str) -> String {
    let mut s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "project".into() } else { s }
}

pub async fn create(state: &AppState, name: &str, platform: &str, slug: Option<&str>) -> anyhow::Result<(ProjectInfo, String)> {
    let id = crate::util::new_id();
    let mut slug = slug.map(slugify).unwrap_or_else(|| slugify(name));
    // Make the slug unique.
    let base = slug.clone();
    let mut n = 2;
    while find(state, &slug).await?.is_some() {
        slug = format!("{base}-{n}");
        n += 1;
    }
    let now = now_ms();
    sqlx::query("INSERT INTO projects (id, slug, name, platform, retention_days, scrub_enabled, scrub_fields, created_at) VALUES ($1, $2, $3, $4, $5, 1, '', $6)")
        .bind(&id)
        .bind(&slug)
        .bind(name)
        .bind(platform)
        .bind(state.config.default_retention_days)
        .bind(now)
        .execute(&state.db)
        .await?;
    let key = create_ingest_key(state, &id).await?;
    let p = find(state, &id).await?.expect("project just created");
    Ok((p, key))
}

pub async fn create_ingest_key(state: &AppState, project_id: &str) -> anyhow::Result<String> {
    let key = format!("vk_{}", crate::util::random_token(16));
    sqlx::query("INSERT INTO ingest_keys (key, project_id, created_at) VALUES ($1, $2, $3)")
        .bind(&key)
        .bind(project_id)
        .bind(now_ms())
        .execute(&state.db)
        .await?;
    Ok(key)
}

pub async fn ingest_keys(state: &AppState, project_id: &str) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query("SELECT key FROM ingest_keys WHERE project_id = $1 AND revoked_at IS NULL ORDER BY created_at")
        .bind(project_id)
        .fetch_all(&state.db)
        .await?;
    Ok(rows.iter().map(|r| r.s("key")).collect())
}

pub async fn invalidate_cache(state: &AppState) {
    state.key_cache.write().await.clear();
}
