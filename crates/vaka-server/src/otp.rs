//! One-time login codes sent by email for the admin UI.
//!
//! A user has at most one live code. Codes are 6 digits, valid for 10 minutes, allow
//! 5 guesses, and a new one can be requested once a minute. Only a hash is stored.

use crate::db::RowExt;
use crate::state::AppState;
use crate::util::{now_ms, sha256_hex};
use rand::Rng;

pub const CODE_TTL_MS: i64 = 10 * 60 * 1000;
pub const RESEND_COOLDOWN_MS: i64 = 60 * 1000;
pub const MAX_ATTEMPTS: i64 = 5;

pub fn generate_code() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

fn code_hash(user_id: &str, code: &str) -> String {
    sha256_hex(&format!("vaka-login-code:{user_id}:{code}"))
}

/// Keep only digits, so "123 456" or "123-456" from an email client still works.
pub fn normalize_code(input: &str) -> String {
    input.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Constant-time comparison of two hex digests.
fn hashes_equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Active (not disabled) user id for an email address.
async fn user_id_for(state: &AppState, email: &str) -> Option<String> {
    sqlx::query("SELECT id FROM users WHERE email = $1 AND disabled = 0")
        .bind(email.trim().to_lowercase())
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .map(|r| r.s("id"))
}

pub enum RequestOutcome {
    /// A code was emailed (or the address is unknown; callers must not reveal which).
    Sent,
    /// A code was sent less than a minute ago; the existing one is still valid.
    TooSoon,
    /// Email delivery failed.
    SendFailed,
}

/// Create a fresh code for `email` and send it. Unknown or disabled addresses get `Sent`
/// without any email, so the response does not reveal which accounts exist.
pub async fn request_code(state: &AppState, email: &str) -> RequestOutcome {
    let Some(user_id) = user_id_for(state, email).await else { return RequestOutcome::Sent };
    let now = now_ms();
    let recent = sqlx::query("SELECT created_at FROM login_codes WHERE user_id = $1")
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .map(|r| r.i("created_at"));
    if recent.is_some_and(|t| now - t < RESEND_COOLDOWN_MS) {
        return RequestOutcome::TooSoon;
    }
    let code = generate_code();
    let _ = sqlx::query("DELETE FROM login_codes WHERE user_id = $1").bind(&user_id).execute(&state.db).await;
    if sqlx::query("INSERT INTO login_codes (user_id, code_hash, created_at, expires_at, attempts) VALUES ($1, $2, $3, $4, 0)")
        .bind(&user_id)
        .bind(code_hash(&user_id, &code))
        .bind(now)
        .bind(now + CODE_TTL_MS)
        .execute(&state.db)
        .await
        .is_err()
    {
        return RequestOutcome::SendFailed;
    }
    let text = format!(
        "Your Vaka sign-in code is {code}\n\nIt expires in 10 minutes. Log in at {}/ui/login\n\nIf you didn't try to sign in, you can ignore this email.\n",
        state.config.public_url
    );
    if crate::admin::send_email(state, email.trim(), &format!("Vaka sign-in code: {code}"), &text).await {
        RequestOutcome::Sent
    } else {
        let _ = sqlx::query("DELETE FROM login_codes WHERE user_id = $1").bind(&user_id).execute(&state.db).await;
        RequestOutcome::SendFailed
    }
}

/// Check a code. Returns the user id on success and consumes the code.
pub async fn verify_code(state: &AppState, email: &str, code: &str) -> Option<String> {
    let code = normalize_code(code);
    if code.len() != 6 {
        return None;
    }
    let user_id = user_id_for(state, email).await?;
    let row = sqlx::query("SELECT code_hash, expires_at, attempts FROM login_codes WHERE user_id = $1")
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()?;
    if row.i("expires_at") < now_ms() || row.i("attempts") >= MAX_ATTEMPTS {
        return None;
    }
    // Count the attempt before comparing so parallel guesses can't exceed the limit.
    let counted = sqlx::query("UPDATE login_codes SET attempts = attempts + 1 WHERE user_id = $1 AND attempts < $2")
        .bind(&user_id)
        .bind(MAX_ATTEMPTS)
        .execute(&state.db)
        .await
        .map(|r| r.rows_affected() == 1)
        .unwrap_or(false);
    if !counted || !hashes_equal(&row.s("code_hash"), &code_hash(&user_id, &code)) {
        return None;
    }
    let consumed = sqlx::query("DELETE FROM login_codes WHERE user_id = $1 AND code_hash = $2")
        .bind(&user_id)
        .bind(row.s("code_hash"))
        .execute(&state.db)
        .await
        .map(|r| r.rows_affected() == 1)
        .unwrap_or(false);
    consumed.then_some(user_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_six_digits() {
        for _ in 0..200 {
            let c = generate_code();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|ch| ch.is_ascii_digit()));
        }
    }

    #[test]
    fn normalize_strips_separators() {
        assert_eq!(normalize_code(" 123 456 "), "123456");
        assert_eq!(normalize_code("123-456"), "123456");
    }

    #[test]
    fn hash_is_bound_to_user() {
        assert_ne!(code_hash("a", "123456"), code_hash("b", "123456"));
        assert!(hashes_equal(&code_hash("a", "123456"), &code_hash("a", "123456")));
        assert!(!hashes_equal(&code_hash("a", "123456"), &code_hash("a", "123457")));
    }
}
