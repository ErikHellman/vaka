//! Metadata store: SQLite or Postgres through sqlx's `Any` driver.
//!
//! All SQL in Vaka is written to run unchanged on both backends: `$N` placeholders,
//! BIGINT for integers and booleans, TEXT for ids, unix-millisecond timestamps.

use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{AnyPool, Row};

pub type Pool = AnyPool;

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/001_init.sql")),
    (2, include_str!("../migrations/002_login_codes.sql")),
    (3, include_str!("../migrations/003_ownership_commits.sql")),
    (4, include_str!("../migrations/004_goals.sql")),
    (5, include_str!("../migrations/005_source_links.sql")),
    (6, include_str!("../migrations/006_webhooks.sql")),
    (7, include_str!("../migrations/007_cohorts.sql")),
    (8, include_str!("../migrations/008_flags.sql")),
    (9, include_str!("../migrations/009_digest_schedule.sql")),
    (10, include_str!("../migrations/010_feedback.sql")),
    (11, include_str!("../migrations/011_monitors.sql")),
];

pub async fn connect(url: &str) -> anyhow::Result<Pool> {
    let pool = open(url).await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// Newest schema version this build knows about.
pub fn latest_version() -> i64 {
    MIGRATIONS.last().map(|m| m.0).unwrap_or(0)
}

/// Open a pool without running migrations.
pub async fn open(url: &str) -> anyhow::Result<Pool> {
    sqlx::any::install_default_drivers();
    if let Some(path) = url.strip_prefix("sqlite://") {
        let file = path.split('?').next().unwrap_or(path);
        if let Some(parent) = std::path::Path::new(file).parent() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    let is_sqlite = url.starts_with("sqlite:");
    let pool = AnyPoolOptions::new()
        .max_connections(if is_sqlite { 4 } else { 16 })
        .after_connect(move |conn, _| {
            Box::pin(async move {
                if is_sqlite {
                    sqlx::query("PRAGMA journal_mode=WAL").execute(&mut *conn).await?;
                    sqlx::query("PRAGMA busy_timeout=5000").execute(&mut *conn).await?;
                    sqlx::query("PRAGMA foreign_keys=ON").execute(&mut *conn).await?;
                }
                if crate::readonly::enforced() {
                    set_read_only(conn, true).await?;
                }
                Ok(())
            })
        })
        .connect(url)
        .await?;
    Ok(pool)
}

async fn set_read_only(conn: &mut sqlx::AnyConnection, on: bool) -> Result<(), sqlx::Error> {
    let sql = match (conn.backend_name(), on) {
        ("SQLite", true) => "PRAGMA query_only = ON",
        ("SQLite", false) => "PRAGMA query_only = OFF",
        (_, true) => "SET SESSION default_transaction_read_only = on",
        (_, false) => "SET SESSION default_transaction_read_only = off",
    };
    sqlx::query(sql).execute(&mut *conn).await?;
    Ok(())
}

/// Switch every connection of the pool to read-only (or back). Holds all connections at
/// once so none is missed; connections opened later pick the mode up in `after_connect`.
pub async fn apply_read_only(pool: &Pool, on: bool) -> anyhow::Result<()> {
    let max = pool.options().get_max_connections() as usize;
    let mut held = Vec::with_capacity(max);
    for _ in 0..max {
        let mut conn = pool.acquire().await?;
        set_read_only(&mut conn, on).await?;
        held.push(conn);
    }
    Ok(())
}

async fn migrate(pool: &Pool) -> anyhow::Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS schema_migrations (version BIGINT PRIMARY KEY, applied_at BIGINT NOT NULL)")
        .execute(pool)
        .await?;
    for (version, sql) in MIGRATIONS {
        let applied: Option<AnyRow> = sqlx::query("SELECT version FROM schema_migrations WHERE version = $1")
            .bind(*version)
            .fetch_optional(pool)
            .await?;
        if applied.is_some() {
            continue;
        }
        let mut tx = pool.begin().await?;
        for stmt in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            sqlx::query(stmt).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO schema_migrations (version, applied_at) VALUES ($1, $2)")
            .bind(*version)
            .bind(crate::util::now_ms())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!(version, "applied metadata migration");
    }
    Ok(())
}

/// Helpers for reading optional columns from `AnyRow` without caring about backend quirks.
pub trait RowExt {
    fn s(&self, col: &str) -> String;
    fn os(&self, col: &str) -> Option<String>;
    fn i(&self, col: &str) -> i64;
    fn oi(&self, col: &str) -> Option<i64>;
}

impl RowExt for AnyRow {
    fn s(&self, col: &str) -> String {
        self.try_get::<Option<String>, _>(col).ok().flatten().unwrap_or_default()
    }
    fn os(&self, col: &str) -> Option<String> {
        self.try_get::<Option<String>, _>(col).ok().flatten()
    }
    fn i(&self, col: &str) -> i64 {
        self.oi(col).unwrap_or(0)
    }
    fn oi(&self, col: &str) -> Option<i64> {
        if let Ok(v) = self.try_get::<Option<i64>, _>(col) {
            return v;
        }
        if let Ok(v) = self.try_get::<Option<i32>, _>(col) {
            return v.map(i64::from);
        }
        None
    }
}
