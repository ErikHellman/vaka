//! Encryption at rest for stored credentials (alert rule secrets such as GitHub tokens).
//!
//! When `VAKA_SECRET_KEY` is set, secrets are sealed with ChaCha20-Poly1305 under a key derived
//! from it (SHA-256) and stored as `enc:v1:<base64(nonce || ciphertext)>`. Values without that
//! prefix are plaintext from before the key was set and still work; [`seal_existing`] converts
//! them at startup. Without a key, secrets are stored as given.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use sha2::{Digest, Sha256};

const PREFIX: &str = "enc:v1:";

fn cipher_from(raw: &str) -> Option<ChaCha20Poly1305> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let key = Sha256::digest(raw.as_bytes());
    ChaCha20Poly1305::new_from_slice(&key).ok()
}

fn cipher() -> Option<&'static ChaCha20Poly1305> {
    static C: once_cell::sync::Lazy<Option<ChaCha20Poly1305>> = once_cell::sync::Lazy::new(|| {
        std::env::var("VAKA_SECRET_KEY")
            .ok()
            .and_then(|k| cipher_from(&k))
    });
    C.as_ref()
}

fn seal_with(c: Option<&ChaCha20Poly1305>, plain: &str) -> String {
    match c {
        Some(c) if !plain.is_empty() && !plain.starts_with(PREFIX) => {
            let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
            let ct = c
                .encrypt(&nonce, plain.as_bytes())
                .expect("chacha20poly1305 encrypt");
            let mut out = nonce.to_vec();
            out.extend_from_slice(&ct);
            format!("{PREFIX}{}", B64.encode(out))
        }
        _ => plain.to_string(),
    }
}

fn open_with(c: Option<&ChaCha20Poly1305>, stored: &str) -> anyhow::Result<String> {
    let Some(b64) = stored.strip_prefix(PREFIX) else {
        return Ok(stored.to_string());
    };
    let c = c.ok_or_else(|| {
        anyhow::anyhow!("an alert secret is encrypted but VAKA_SECRET_KEY is not set")
    })?;
    let bytes = B64.decode(b64)?;
    if bytes.len() < 12 {
        anyhow::bail!("encrypted secret is truncated");
    }
    let (nonce, ct) = bytes.split_at(12);
    let plain = c
        .decrypt(&Nonce::from(<[u8; 12]>::try_from(nonce)?), ct)
        .map_err(|_| {
            anyhow::anyhow!("an alert secret can't be decrypted: VAKA_SECRET_KEY changed?")
        })?;
    Ok(String::from_utf8(plain)?)
}

/// Seal a secret for storage (unchanged when no key is configured).
pub fn seal(plain: &str) -> String {
    seal_with(cipher(), plain)
}

/// Read a stored secret. Plaintext values pass through.
pub fn open(stored: &str) -> anyhow::Result<String> {
    open_with(cipher(), stored)
}

/// Encrypt plaintext alert secrets left from before `VAKA_SECRET_KEY` was set.
pub async fn seal_existing(db: &sqlx::AnyPool) -> anyhow::Result<()> {
    if cipher().is_none() {
        return Ok(());
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, secret FROM alert_rules WHERE secret <> '' AND secret NOT LIKE 'enc:v1:%'",
    )
    .fetch_all(db)
    .await?;
    for (id, secret) in &rows {
        sqlx::query("UPDATE alert_rules SET secret = $1 WHERE id = $2")
            .bind(seal(secret))
            .bind(id)
            .execute(db)
            .await?;
    }
    if !rows.is_empty() {
        tracing::info!(
            "encrypted {} stored alert secret(s) with VAKA_SECRET_KEY",
            rows.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_plaintext_compat() {
        let c = cipher_from("test-key");
        let sealed = seal_with(c.as_ref(), "ghp_secret");
        assert!(sealed.starts_with(PREFIX));
        assert_ne!(
            seal_with(c.as_ref(), "ghp_secret"),
            sealed,
            "fresh nonce each time"
        );
        assert_eq!(open_with(c.as_ref(), &sealed).unwrap(), "ghp_secret");
        assert_eq!(open_with(c.as_ref(), "plain").unwrap(), "plain");
        assert_eq!(seal_with(c.as_ref(), ""), "");
        assert_eq!(seal_with(None, "x"), "x");
        assert!(open_with(cipher_from("other").as_ref(), &sealed).is_err());
        assert!(open_with(None, &sealed).is_err());
    }
}
