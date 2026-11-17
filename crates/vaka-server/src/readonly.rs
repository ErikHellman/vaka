//! Read-only maintenance mode for the metadata database.
//!
//! Used for the SQLite -> Postgres cutover (`vaka-server migrate-db`): while it is on,
//! metadata writes (admin edits, issue state, tokens, alert rules, ...) are refused with
//! a clear 503, background jobs pause, and analytics ingest (events, sessions, spans,
//! logs) keeps flowing into ClickHouse. Error and feedback items need metadata writes
//! (issue grouping, feedback rows), so ingest answers those with a retryable 503 and
//! SDKs send them again once the server is back on the new database.
//!
//! Turning it on happens in two steps: first `requested` flips, so no new metadata work
//! is accepted; then, once the error events already queued have been processed and their
//! alerts dispatched, every database connection is switched to read-only (`PRAGMA
//! query_only` on SQLite, `default_transaction_read_only` on Postgres). That last step is
//! what makes the final copy safe: nothing can write to the old database after it.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering::SeqCst};
use std::time::Duration;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static ENFORCED: AtomicBool = AtomicBool::new(false);
/// Accepted work that still has to write metadata (queued error events, feedback being stored).
static PENDING: AtomicI64 = AtomicI64::new(0);
/// Serializes enable/disable so two admins toggling at once cannot interleave.
static TOGGLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub const MESSAGE: &str = "Vaka is in read-only maintenance mode (metadata database migration); changes are paused, try again shortly";

/// Read-only mode was asked for: refuse new metadata work, pause background jobs.
pub fn requested() -> bool {
    REQUESTED.load(SeqCst)
}

/// Database connections are read-only (set on connections as they are opened, too).
pub fn enforced() -> bool {
    ENFORCED.load(SeqCst)
}

/// A slot of in-flight metadata work. Taken before work is accepted; refused while
/// read-only mode is requested. Dropping it (or calling `release`) frees the slot.
pub struct Pending(i64);

impl Pending {
    /// Reserve `n` slots, or `None` when read-only mode is on.
    pub fn try_acquire(n: i64) -> Option<Pending> {
        if n == 0 {
            return Some(Pending(0));
        }
        // Increment first, then check: `enable` sets the flag first, then waits for zero,
        // so every piece of work is either refused here or counted before it waits.
        PENDING.fetch_add(n, SeqCst);
        if REQUESTED.load(SeqCst) {
            PENDING.fetch_sub(n, SeqCst);
            return None;
        }
        Some(Pending(n))
    }

    /// Hand one slot over to whoever finishes the work (see `finish_one`).
    pub fn transfer_one(&mut self) {
        if self.0 > 0 {
            self.0 -= 1;
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.0 > 0 {
            PENDING.fetch_sub(self.0, SeqCst);
        }
    }
}

/// The processor finished one error event handed over with `transfer_one`.
pub fn finish_one() {
    PENDING.fetch_sub(1, SeqCst);
}

pub fn pending() -> i64 {
    PENDING.load(SeqCst)
}

/// Turn read-only mode on: stop accepting metadata work, wait for queued work to finish
/// (at most `drain`), then make every database connection read-only. Returns whether the
/// queue drained in time (the guard is applied either way).
pub async fn enable(pool: &crate::db::Pool, drain: Duration) -> anyhow::Result<bool> {
    let _lock = TOGGLE.lock().await;
    REQUESTED.store(true, SeqCst);
    let deadline = tokio::time::Instant::now() + drain;
    let mut drained = true;
    while pending() > 0 {
        if tokio::time::Instant::now() >= deadline {
            drained = false;
            tracing::warn!(pending = pending(), "read-only: queued metadata work did not finish in time");
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Alerts for new issues are dispatched 2s after the event is processed.
    if drained && !ENFORCED.load(SeqCst) {
        tokio::time::sleep(crate::readonly::ALERT_GRACE).await;
    }
    ENFORCED.store(true, SeqCst);
    crate::db::apply_read_only(pool, true).await?;
    tracing::warn!("metadata database is now read-only");
    Ok(drained)
}

/// Turn read-only mode off.
pub async fn disable(pool: &crate::db::Pool) -> anyhow::Result<()> {
    let _lock = TOGGLE.lock().await;
    ENFORCED.store(false, SeqCst);
    crate::db::apply_read_only(pool, false).await?;
    REQUESTED.store(false, SeqCst);
    tracing::info!("metadata database is writable again");
    Ok(())
}

#[cfg(not(test))]
pub const ALERT_GRACE: Duration = Duration::from_millis(2500);
#[cfg(test)]
pub const ALERT_GRACE: Duration = Duration::from_millis(10);

/// Is this database error caused by read-only mode?
pub fn is_read_only_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("readonly database") || m.contains("read-only transaction") || m.contains("read only database")
}

pub fn status_json() -> serde_json::Value {
    serde_json::json!({"read_only": requested(), "enforced": enforced(), "pending": pending().max(0)})
}

#[cfg(test)]
pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guard_blocks_writes_and_drains_pending_work() {
        let _serial = TEST_LOCK.lock().await;
        let dir = std::env::temp_dir().join(format!("vaka-ro-{}", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}/vaka.db?mode=rwc", dir.display());
        let pool = crate::db::connect(&url).await.unwrap();
        sqlx::query("INSERT INTO daily_salts (day, salt) VALUES ('d1', 's')").execute(&pool).await.unwrap();

        // Work accepted before the switch holds it until it finishes.
        let mut slot = Pending::try_acquire(1).unwrap();
        slot.transfer_one();
        drop(slot);
        let p2 = pool.clone();
        let t = tokio::spawn(async move { enable(&p2, Duration::from_secs(5)).await.unwrap() });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(requested());
        assert!(!enforced(), "must wait for pending work");
        assert!(Pending::try_acquire(1).is_none(), "new work is refused");
        finish_one();
        assert!(t.await.unwrap(), "drained");
        assert!(enforced());

        // Every pooled connection refuses writes; reads still work.
        for _ in 0..8 {
            let err = sqlx::query("INSERT INTO daily_salts (day, salt) VALUES ('d2', 's')").execute(&pool).await.unwrap_err();
            assert!(is_read_only_error(&err.to_string()), "{err}");
        }
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM daily_salts").fetch_one(&pool).await.unwrap();
        assert_eq!(n, 1);

        disable(&pool).await.unwrap();
        assert!(!requested() && !enforced());
        sqlx::query("INSERT INTO daily_salts (day, salt) VALUES ('d2', 's')").execute(&pool).await.unwrap();
        assert!(Pending::try_acquire(1).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
