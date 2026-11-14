//! Periodic housekeeping: daily salts, retention enforcement, session cleanup,
//! and GDPR erasure.

use crate::db::RowExt;
use crate::state::AppState;
use crate::util::{now_ms, random_token};
use std::time::Duration;

pub async fn salt_for_day(state: &AppState, day: &str) -> anyhow::Result<String> {
    if let Some(r) = sqlx::query("SELECT salt FROM daily_salts WHERE day = $1").bind(day).fetch_optional(&state.db).await? {
        return Ok(r.s("salt"));
    }
    sqlx::query("INSERT INTO daily_salts (day, salt) VALUES ($1, $2) ON CONFLICT (day) DO NOTHING")
        .bind(day)
        .bind(random_token(32))
        .execute(&state.db)
        .await?;
    // Old salts are deleted so yesterday's visitor hashes can never be recomputed.
    sqlx::query("DELETE FROM daily_salts WHERE day < $1").bind(day).execute(&state.db).await?;
    Ok(sqlx::query("SELECT salt FROM daily_salts WHERE day = $1").bind(day).fetch_one(&state.db).await?.s("salt"))
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            if let Err(e) = enforce_retention(&state).await {
                tracing::warn!("retention job failed: {e:#}");
            }
            let _ = sqlx::query("DELETE FROM web_sessions WHERE expires_at < $1").bind(now_ms()).execute(&state.db).await;
            let _ = sqlx::query("DELETE FROM login_codes WHERE expires_at < $1").bind(now_ms()).execute(&state.db).await;
        }
    });
}

/// TTLs handle rows written with the current setting. When retention is lowered,
/// rows written earlier with a longer TTL are deleted here.
pub async fn enforce_retention(state: &AppState) -> anyhow::Result<()> {
    for p in crate::projects::list(state).await? {
        for (table, col) in [("errors", "timestamp"), ("events", "timestamp"), ("sessions", "started")] {
            state
                .ch
                .execute_with(
                    &format!(
                        "DELETE FROM {table} WHERE project_id = {{p:String}} AND retention_days > {{d:UInt16}} \
                         AND {col} < now64(3) - toIntervalDay({{d:UInt16}})"
                    ),
                    &[("p", p.id.clone()), ("d", p.retention_days.to_string())],
                )
                .await?;
        }
        crate::feedback::enforce_retention(state, &p).await?;
    }
    Ok(())
}

#[derive(serde::Serialize)]
pub struct ErasureResult {
    pub project_id: String,
    pub user_id: String,
    pub errors: u64,
    pub events: u64,
    pub sessions: u64,
    pub feedback: u64,
}

/// Delete everything stored for `user_id` in a project (GDPR right to erasure).
pub async fn erase_user(state: &AppState, project_id: &str, user_id: &str, actor: &str) -> anyhow::Result<ErasureResult> {
    let params = [("p", project_id.to_string()), ("u", user_id.to_string())];
    let mut counts = [0u64; 3];
    for (i, table) in ["errors", "events", "sessions"].iter().enumerate() {
        let rows = state
            .ch
            .query(&format!("SELECT count() AS n FROM {table} WHERE project_id = {{p:String}} AND user_id = {{u:String}}"), &params)
            .await?;
        counts[i] = rows.first().and_then(|r| r["n"].as_u64()).unwrap_or(0);
        state
            .ch
            .execute_with(
                &format!("DELETE FROM {table} WHERE project_id = {{p:String}} AND user_id = {{u:String}}"),
                &params,
            )
            .await?;
    }
    // Spans and OTLP logs carry user ids too.
    for table in ["spans", "logs"] {
        state.ch.execute_with(&format!("DELETE FROM {table} WHERE project_id = {{p:String}} AND user_id = {{u:String}}"), &params).await?;
    }
    // Feedback is matched by user id, and by contact email when the id is an email address.
    let feedback = crate::feedback::erase_user(state, project_id, user_id).await?;
    crate::auth::audit(state, actor, "erase_user", project_id, &format!("user_id={user_id} errors={} events={} sessions={} feedback={feedback}", counts[0], counts[1], counts[2])).await;
    Ok(ErasureResult {
        project_id: project_id.into(),
        user_id: user_id.into(),
        errors: counts[0],
        events: counts[1],
        sessions: counts[2],
        feedback,
    })
}
