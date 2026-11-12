//! Optional S3-compatible storage for debug files (AWS S3, Cloudflare R2, MinIO, ...).
//!
//! The local artifact directory stays the working copy: uploads are written there and to the
//! bucket, and symbolication downloads files that are missing locally (a fresh container, another
//! instance, a wiped disk). Requests are signed with AWS Signature Version 4.

use crate::alerts::hmac_lite::hmac_sha256;
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub prefix: String,
    pub path_style: bool,
}

impl S3Config {
    pub fn from_env() -> Option<Self> {
        let var = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
        let bucket = var("VAKA_S3_BUCKET")?;
        let region = var("VAKA_S3_REGION").unwrap_or_else(|| "us-east-1".into());
        let custom = var("VAKA_S3_ENDPOINT");
        let endpoint = custom.clone().unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com")).trim_end_matches('/').to_string();
        let path_style = match var("VAKA_S3_PATH_STYLE").as_deref().map(str::to_lowercase).as_deref() {
            Some("true" | "1" | "yes") => true,
            Some("false" | "0" | "no") => false,
            _ => custom.is_some(),
        };
        let mut prefix = var("VAKA_S3_PREFIX").unwrap_or_else(|| "vaka/".into());
        if !prefix.is_empty() && !prefix.ends_with('/') {
            prefix.push('/');
        }
        Some(Self {
            endpoint,
            bucket,
            region,
            access_key: var("VAKA_S3_ACCESS_KEY_ID").or_else(|| var("AWS_ACCESS_KEY_ID")).unwrap_or_default(),
            secret_key: var("VAKA_S3_SECRET_ACCESS_KEY").or_else(|| var("AWS_SECRET_ACCESS_KEY")).unwrap_or_default(),
            prefix: prefix.trim_start_matches('/').to_string(),
            path_style,
        })
    }

    /// (url, host, canonical uri) for an object key ("" = the bucket itself).
    fn location(&self, key: &str) -> (String, String, String) {
        let (scheme, rest) = self.endpoint.split_once("://").unwrap_or(("https", &self.endpoint));
        let (host, base) = match rest.split_once('/') {
            Some((h, b)) => (h.to_string(), format!("/{}", b.trim_matches('/'))),
            None => (rest.to_string(), String::new()),
        };
        let base = if base == "/" { String::new() } else { base };
        let key = encode_path(key);
        if self.path_style {
            let uri = if key.is_empty() { format!("{base}/{}", self.bucket) } else { format!("{base}/{}/{key}", self.bucket) };
            (format!("{scheme}://{host}{uri}"), host, uri)
        } else {
            let host = format!("{}.{host}", self.bucket);
            let uri = format!("{base}/{key}");
            (format!("{scheme}://{host}{uri}"), host, uri)
        }
    }
}

fn encode_path(p: &str) -> String {
    p.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> [u8; 32] {
    let k = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac_sha256(&k, region.as_bytes());
    let k = hmac_sha256(&k, service.as_bytes());
    hmac_sha256(&k, b"aws4_request")
}

/// Authorization header for a request without a query string.
fn authorization(cfg: &S3Config, method: &str, host: &str, uri: &str, payload_hash: &str, amz_date: &str) -> String {
    let date = &amz_date[..8];
    let canonical = format!(
        "{method}\n{uri}\n\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload_hash}"
    );
    let scope = format!("{date}/{}/s3/aws4_request", cfg.region);
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let sig = hex::encode(hmac_sha256(&signing_key(&cfg.secret_key, date, &cfg.region, "s3"), to_sign.as_bytes()));
    format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={sig}", cfg.access_key)
}

fn amz_now() -> String {
    let now = time::OffsetDateTime::now_utc();
    now.format(time::macros::format_description!("[year][month][day]T[hour][minute][second]Z")).unwrap_or_default()
}

#[derive(Clone)]
pub struct S3 {
    pub cfg: S3Config,
    http: reqwest::Client,
}

impl S3 {
    pub fn new(cfg: S3Config) -> anyhow::Result<Self> {
        Ok(Self { cfg, http: reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()? })
    }

    fn request(&self, method: reqwest::Method, key: &str, body: Option<&[u8]>) -> reqwest::RequestBuilder {
        let (url, host, uri) = self.cfg.location(key);
        let hash = sha256_hex(body.unwrap_or(b""));
        let date = amz_now();
        let auth = authorization(&self.cfg, method.as_str(), &host, &uri, &hash, &date);
        let mut req = self
            .http
            .request(method, url)
            .header("x-amz-date", date)
            .header("x-amz-content-sha256", hash)
            .header("authorization", auth)
            .timeout(Duration::from_secs(600));
        if let Some(b) = body {
            req = req.body(b.to_vec());
        }
        req
    }

    pub fn key_for(&self, relative: &str) -> String {
        format!("{}{}", self.cfg.prefix, relative.trim_start_matches('/'))
    }

    pub async fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let resp = self.request(reqwest::Method::PUT, key, Some(bytes)).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            anyhow::bail!("S3 PUT {key}: {status} {}", crate::util::truncate(&resp.text().await.unwrap_or_default(), 300));
        }
        Ok(())
    }

    /// Download an object into `path` (via a temporary file). Ok(false) when it does not exist.
    pub async fn get_to_file(&self, key: &str, path: &str) -> anyhow::Result<bool> {
        use tokio::io::AsyncWriteExt;
        let mut resp = self.request(reqwest::Method::GET, key, None).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            anyhow::bail!("S3 GET {key}: {status} {}", crate::util::truncate(&resp.text().await.unwrap_or_default(), 300));
        }
        if let Some(dir) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let tmp = format!("{path}.part-{}", crate::util::new_id());
        let mut f = tokio::fs::File::create(&tmp).await?;
        while let Some(chunk) = resp.chunk().await? {
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        drop(f);
        tokio::fs::rename(&tmp, path).await?;
        Ok(true)
    }

    pub async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        let resp = self.request(reqwest::Method::HEAD, key, None).send().await?;
        match resp.status() {
            s if s.is_success() => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s => anyhow::bail!("S3 HEAD {key}: {s}"),
        }
    }

    pub async fn delete(&self, key: &str) -> anyhow::Result<()> {
        let resp = self.request(reqwest::Method::DELETE, key, None).send().await?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("S3 DELETE {key}: {}", resp.status());
        }
        Ok(())
    }

    /// Check the bucket at startup; create it when it does not exist (MinIO, dev setups).
    pub async fn ensure_bucket(&self) -> anyhow::Result<()> {
        let resp = self.request(reqwest::Method::HEAD, "", None).send().await?;
        if resp.status().is_success() {
            return Ok(());
        }
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let resp = self.request(reqwest::Method::PUT, "", Some(b"")).send().await?;
            if resp.status().is_success() {
                tracing::info!("created S3 bucket {}", self.cfg.bucket);
                return Ok(());
            }
            anyhow::bail!("bucket {} does not exist and could not be created: {}", self.cfg.bucket, resp.status());
        }
        anyhow::bail!("cannot access bucket {}: {}", self.cfg.bucket, resp.status())
    }
}

// ---------------------------------------------------------------------------
// Artifact helpers
// ---------------------------------------------------------------------------

use crate::state::AppState;

/// Object key for a local artifact path: the path relative to the artifact directory.
fn artifact_key(state: &AppState, s3: &S3, path: &str) -> Option<String> {
    let root = state.config.artifact_dir();
    let rel = path.strip_prefix(&root)?.trim_start_matches('/');
    (!rel.is_empty()).then(|| s3.key_for(&format!("artifacts/{rel}")))
}

/// After an upload has been accepted: copy the local file to the bucket.
pub async fn store_artifact(state: &AppState, path: &str) -> anyhow::Result<()> {
    let Some(s3) = &state.s3 else { return Ok(()) };
    let Some(key) = artifact_key(state, s3, path) else { return Ok(()) };
    let bytes = tokio::fs::read(path).await?;
    s3.put(&key, &bytes).await
}

/// Make sure an artifact is on local disk, downloading it from the bucket when needed.
pub async fn ensure_local(state: &AppState, path: &str) -> bool {
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        return true;
    }
    let Some(s3) = &state.s3 else { return false };
    let Some(key) = artifact_key(state, s3, path) else { return false };
    match s3.get_to_file(&key, path).await {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!("fetching debug file from S3 failed: {e:#}");
            false
        }
    }
}

pub async fn delete_artifact(state: &AppState, path: &str) {
    let Some(s3) = &state.s3 else { return };
    if let Some(key) = artifact_key(state, s3, path) {
        if let Err(e) = s3.delete(&key).await {
            tracing::warn!("deleting debug file from S3 failed: {e:#}");
        }
    }
}

/// At startup: check the bucket and copy local debug files that are not in it yet.
pub fn spawn_startup_sync(state: AppState) {
    let Some(s3) = state.s3.clone() else { return };
    tokio::spawn(async move {
        if let Err(e) = s3.ensure_bucket().await {
            tracing::error!("S3 storage: {e:#}");
            return;
        }
        let rows = match sqlx::query("SELECT DISTINCT path FROM artifacts").fetch_all(&state.db).await {
            Ok(r) => r,
            Err(e) => return tracing::warn!("S3 sync: {e}"),
        };
        let mut copied = 0;
        for r in rows {
            use crate::db::RowExt;
            let path = r.s("path");
            let Some(key) = artifact_key(&state, &s3, &path) else { continue };
            if !tokio::fs::try_exists(&path).await.unwrap_or(false) || s3.exists(&key).await.unwrap_or(true) {
                continue;
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => match s3.put(&key, &bytes).await {
                    Ok(()) => copied += 1,
                    Err(e) => tracing::warn!("S3 sync: {e:#}"),
                },
                Err(e) => tracing::warn!("S3 sync: reading {path}: {e}"),
            }
        }
        tracing::info!("S3 storage ready (bucket {}, {copied} existing debug files copied)", s3.cfg.bucket);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_signing_key_vector() {
        // From the AWS Signature Version 4 documentation.
        let k = signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20120215", "us-east-1", "iam");
        assert_eq!(hex::encode(k), "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d");
    }

    #[test]
    fn locations() {
        let mut c = S3Config {
            endpoint: "http://localhost:9000".into(),
            bucket: "b".into(),
            region: "us-east-1".into(),
            access_key: "a".into(),
            secret_key: "s".into(),
            prefix: "vaka/".into(),
            path_style: true,
        };
        assert_eq!(c.location("vaka/x y"), ("http://localhost:9000/b/vaka/x%20y".into(), "localhost:9000".into(), "/b/vaka/x%20y".into()));
        assert_eq!(c.location("").2, "/b");
        c.path_style = false;
        c.endpoint = "https://s3.eu-north-1.amazonaws.com".into();
        assert_eq!(c.location("k").0, "https://b.s3.eu-north-1.amazonaws.com/k");
    }
}
