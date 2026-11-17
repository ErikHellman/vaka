//! Country lookup from the client IP with a MaxMind-format `.mmdb` database.
//!
//! Vaka ships DB-IP's free "IP to Country Lite" database (CC BY 4.0) in the image, refreshed in
//! the repository by a monthly CI job. With `VAKA_GEOIP_UPDATE` on (the default) the server also
//! checks for a newer release once a week and keeps it in `$VAKA_DATA_DIR/geoip/`, so long-running
//! instances stay current between upgrades. A file set with `VAKA_GEOIP_DB` is used as is and
//! never replaced. `VAKA_GEOIP=false` turns the lookup off.

use crate::config::Config;
use crate::state::AppState;
use maxminddb::Reader;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub const FILE_NAME: &str = "dbip-country-lite.mmdb";
/// Where the Docker image puts the bundled database.
pub const IMAGE_PATH: &str = "/usr/share/vaka/dbip-country-lite.mmdb";
/// The bundled database in a source checkout (`cargo run` from the repository root).
const REPO_PATH: &str = "geoip/dbip-country-lite.mmdb";
/// `{month}` is replaced with `YYYY-MM`. DB-IP publishes one Lite release per month.
pub const DEFAULT_UPDATE_URL: &str =
    "https://download.db-ip.com/free/dbip-country-lite-{month}.mmdb.gz";
const UPDATE_INTERVAL: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_DOWNLOAD: usize = 64 * 1024 * 1024;

type Db = Arc<Reader<Vec<u8>>>;

pub struct GeoIp {
    reader: RwLock<Option<Db>>,
}

impl GeoIp {
    pub fn disabled() -> Self {
        Self {
            reader: RwLock::new(None),
        }
    }

    /// Open the configured database, or the newest of the downloaded and bundled copies.
    pub fn load(config: &Config) -> Self {
        if !config.geoip_enabled {
            tracing::info!("GeoIP: country lookup disabled (VAKA_GEOIP=false)");
            return Self::disabled();
        }
        let db = match &config.geoip_db {
            Some(p) => open(Path::new(p))
                .map_err(|e| tracing::warn!("GeoIP: could not open {p}: {e}"))
                .ok()
                .map(|r| (PathBuf::from(p), r)),
            None => candidates(config)
                .into_iter()
                .filter(|p| p.exists())
                .filter_map(|p| match open(&p) {
                    Ok(r) => Some((p, r)),
                    Err(e) => {
                        tracing::warn!("GeoIP: ignoring {}: {e}", p.display());
                        None
                    }
                })
                .max_by_key(|(_, r)| r.metadata.build_epoch),
        };
        match &db {
            Some((p, r)) => tracing::info!("GeoIP: country lookup from {} ({}, built {})", p.display(), r.metadata.database_type, built(r)),
            None => tracing::warn!(
                "GeoIP: no country database found, so events get no country. Set VAKA_GEOIP_DB or keep VAKA_GEOIP_UPDATE on to download one"
            ),
        }
        Self {
            reader: RwLock::new(db.map(|(_, r)| Arc::new(r))),
        }
    }

    fn build_epoch(&self) -> u64 {
        self.reader
            .read()
            .unwrap()
            .as_ref()
            .map_or(0, |r| r.metadata.build_epoch)
    }

    fn replace(&self, db: Reader<Vec<u8>>) {
        *self.reader.write().unwrap() = Some(Arc::new(db));
    }

    /// ISO 3166-1 alpha-2 code for the address, if the database knows it.
    pub fn country(&self, ip: IpAddr) -> Option<String> {
        let db = self.reader.read().unwrap().clone()?;
        let c = db
            .lookup(ip)
            .and_then(|r| r.decode::<maxminddb::geoip2::Country>())
            .ok()??;
        c.country.iso_code.map(str::to_string)
    }
}

fn open(path: &Path) -> Result<Reader<Vec<u8>>, String> {
    Reader::open_readfile(path).map_err(|e| e.to_string())
}

fn built(r: &Reader<Vec<u8>>) -> String {
    time::OffsetDateTime::from_unix_timestamp(r.metadata.build_epoch as i64)
        .map(|t| t.date().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn downloaded_path(config: &Config) -> PathBuf {
    Path::new(&config.data_dir).join("geoip").join(FILE_NAME)
}

fn candidates(config: &Config) -> Vec<PathBuf> {
    vec![
        downloaded_path(config),
        PathBuf::from(IMAGE_PATH),
        PathBuf::from(REPO_PATH),
    ]
}

/// `YYYY-MM` for this month and the one before: a new month's file appears a day or two in.
fn months(now: time::OffsetDateTime) -> [String; 2] {
    let d = now.date();
    let (py, pm) = if d.month() == time::Month::January {
        (d.year() - 1, 12)
    } else {
        (d.year(), u8::from(d.month()) - 1)
    };
    [
        format!("{}-{:02}", d.year(), u8::from(d.month())),
        format!("{py}-{pm:02}"),
    ]
}

/// Decompress (if gzipped) and validate a downloaded database. Returns the raw `.mmdb` bytes.
fn parse_download(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let raw = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .take(MAX_DOWNLOAD as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| format!("gunzip: {e}"))?;
        if out.len() > MAX_DOWNLOAD {
            return Err("database too large".into());
        }
        out
    } else {
        bytes.to_vec()
    };
    let r =
        Reader::from_source(raw.as_slice()).map_err(|e| format!("not a valid .mmdb file: {e}"))?;
    let probe: IpAddr = "8.8.8.8".parse().unwrap();
    r.lookup(probe)
        .and_then(|l| l.decode::<maxminddb::geoip2::Country>())
        .map_err(|e| format!("not a country database: {e}"))?;
    drop(r);
    Ok(raw)
}

async fn fetch(state: &AppState, url: &str) -> Result<Option<Vec<u8>>, String> {
    let resp = state
        .http
        .get(url)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    if resp
        .content_length()
        .is_some_and(|n| n as usize > MAX_DOWNLOAD)
    {
        return Err("download too large".into());
    }
    let body = resp.bytes().await.map_err(|e| e.to_string())?;
    if body.len() > MAX_DOWNLOAD {
        return Err("download too large".into());
    }
    Ok(Some(body.to_vec()))
}

/// Fetch the newest monthly release and swap it in when it is newer than the loaded one.
async fn check(state: &AppState) -> Result<(), String> {
    let template = &state.config.geoip_update_url;
    for month in months(time::OffsetDateTime::now_utc()) {
        let url = template.replace("{month}", &month);
        let Some(bytes) = fetch(state, &url).await? else {
            continue;
        };
        let raw = tokio::task::spawn_blocking(move || parse_download(&bytes))
            .await
            .map_err(|e| e.to_string())??;
        let epoch = Reader::from_source(raw.as_slice())
            .map_err(|e| e.to_string())?
            .metadata
            .build_epoch;
        if epoch <= state.geoip.build_epoch() {
            tracing::debug!("GeoIP: {url} is not newer than the loaded database");
            return Ok(());
        }
        let path = downloaded_path(&state.config);
        let tmp = path.with_extension("mmdb.tmp");
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .map_err(|e| e.to_string())?;
        tokio::fs::write(&tmp, &raw)
            .await
            .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| e.to_string())?;
        let db = Reader::from_source(raw).map_err(|e| e.to_string())?;
        tracing::info!(
            "GeoIP: updated country database to the {month} release (built {})",
            built(&db)
        );
        state.geoip.replace(db);
        return Ok(());
    }
    Err(format!("no release found at {template}"))
}

/// Weekly update check, unless turned off or a custom database is configured.
pub fn spawn_updater(state: AppState) {
    let c = &state.config;
    if !c.geoip_enabled || !c.geoip_update || c.geoip_db.is_some() {
        if c.geoip_enabled && c.geoip_update && c.geoip_db.is_some() {
            tracing::info!("GeoIP: VAKA_GEOIP_DB is set, so the weekly database update is off");
        }
        return;
    }
    tokio::spawn(async move {
        // Skip the first check when the downloaded copy is less than a week old (restarts).
        let age = tokio::fs::metadata(downloaded_path(&state.config))
            .await
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        let mut wait = match age {
            Some(a) if a < UPDATE_INTERVAL => UPDATE_INTERVAL - a,
            _ => Duration::from_secs(60),
        };
        loop {
            tokio::time::sleep(wait).await;
            if let Err(e) = check(&state).await {
                tracing::warn!("GeoIP: update check failed: {e}");
            }
            wait = UPDATE_INTERVAL;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_wrap_the_year() {
        let t = time::macros::datetime!(2026-01-02 0:00 UTC);
        assert_eq!(months(t), ["2026-01".to_string(), "2025-12".to_string()]);
        let t = time::macros::datetime!(2026-11-17 0:00 UTC);
        assert_eq!(months(t), ["2026-11".to_string(), "2026-10".to_string()]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_download(b"not a database").is_err());
        assert!(parse_download(&[0x1f, 0x8b, 0, 0]).is_err());
    }

    /// Runs against the database bundled in the repository.
    #[test]
    fn bundled_database_resolves_countries() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(REPO_PATH);
        let bytes = std::fs::read(&path).expect("bundled GeoIP database is missing");
        let db = Reader::from_source(parse_download(&bytes).unwrap()).unwrap();
        let g = GeoIp {
            reader: RwLock::new(Some(Arc::new(db))),
        };
        assert_eq!(g.country("8.8.8.8".parse().unwrap()).as_deref(), Some("US"));
        assert_eq!(g.country("127.0.0.1".parse().unwrap()), None);
        // Gzipped, as DB-IP serves it.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &bytes).unwrap();
        assert!(parse_download(&gz.finish().unwrap()).is_ok());
    }
}
