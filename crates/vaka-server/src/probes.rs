//! Outbound checks for HTTP and TLS monitors.
//!
//! Every request goes through [`crate::netguard::Guard`]: public addresses only, plus the
//! operator's `VAKA_MONITOR_ALLOW_CIDRS`. Redirect targets are checked too. A check that can't
//! connect is only counted when Vaka itself can reach the internet (the canary URLs), so an
//! outage of Vaka's own network does not page everyone.

use crate::netguard::{monitor_client, Guard};
use once_cell::sync::Lazy;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT_S: i64 = 10;
pub const MAX_TIMEOUT_S: i64 = 60;
pub const DEFAULT_TLS_WARN_DAYS: i64 = 14;

/// What a check found.
#[derive(Debug, Default, Clone)]
pub struct Probe {
    /// ok, fail, timeout or inconclusive.
    pub result: String,
    pub duration_ms: i64,
    pub http_status: i64,
    pub error: String,
    /// When the server's certificate expires (ms), for https targets.
    pub tls_not_after: Option<i64>,
    pub trace_id: String,
}

impl Probe {
    fn new(result: &str, error: impl Into<String>) -> Probe {
        Probe { result: result.into(), error: error.into(), ..Default::default() }
    }
}

/// The HTTP check of a monitor, from its stored config.
#[derive(Debug, Clone)]
pub struct HttpCheck {
    pub url: String,
    pub method: reqwest::Method,
    pub timeout: Duration,
    /// Accepted status codes: exact codes or classes (`2` for 2xx).
    pub expect: Vec<(u16, bool)>,
    pub body_contains: Option<String>,
    pub headers: Vec<(String, String)>,
    pub follow_redirects: bool,
}

/// Parse `expect_status`: a code (200), a class ("2xx") or a list of them. Default 2xx and 3xx.
pub fn parse_expect(v: Option<&Value>) -> Result<Vec<(u16, bool)>, String> {
    let one = |v: &Value| -> Result<(u16, bool), String> {
        let bad = || "expect_status must be status codes like 200 or classes like \"2xx\"".to_string();
        match v {
            Value::Number(n) => n.as_u64().filter(|n| (100..600).contains(n)).map(|n| (n as u16, false)).ok_or_else(bad),
            Value::String(s) => {
                let s = s.trim().to_ascii_lowercase();
                if s.len() == 3 && s.ends_with("xx") {
                    s[..1].parse::<u16>().ok().filter(|d| (1..6).contains(d)).map(|d| (d, true)).ok_or_else(bad)
                } else {
                    s.parse::<u16>().ok().filter(|n| (100..600).contains(n)).map(|n| (n, false)).ok_or_else(bad)
                }
            }
            _ => Err(bad()),
        }
    };
    match v {
        None | Some(Value::Null) => Ok(vec![(2, true), (3, true)]),
        Some(Value::Array(a)) if !a.is_empty() && a.len() <= 20 => a.iter().map(one).collect(),
        Some(Value::Array(_)) => Err("expect_status needs 1 to 20 entries".into()),
        Some(v) => Ok(vec![one(v)?]),
    }
}

fn status_ok(expect: &[(u16, bool)], code: u16) -> bool {
    expect.iter().any(|(n, class)| if *class { code / 100 == *n } else { code == *n })
}

/// A new W3C trace id and a `traceparent` header value, so the target's own traces (if it
/// reports to Vaka) can be found from the check.
pub fn traceparent() -> (String, String) {
    use rand::RngCore;
    let mut t = [0u8; 16];
    let mut s = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut t);
    rand::thread_rng().fill_bytes(&mut s);
    let trace = hex::encode(t);
    let header = format!("00-{trace}-{}-01", hex::encode(s));
    (trace, header)
}

static GUARD: Lazy<Arc<Guard>> = Lazy::new(|| Arc::new(Guard::from_env()));

pub fn guard() -> Arc<Guard> {
    GUARD.clone()
}

static CLIENTS: Lazy<(reqwest::Client, reqwest::Client)> = Lazy::new(|| {
    let follow = monitor_client(guard(), true).expect("monitor http client");
    let direct = monitor_client(guard(), false).expect("monitor http client");
    (follow, direct)
});

/// Describe a reqwest error by its root cause ("connection refused", "certificate expired").
fn describe(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(e);
    while let Some(s) = src {
        let t = s.to_string();
        if !msg.contains(&t) {
            msg = format!("{msg}: {t}");
        }
        src = s.source();
    }
    // Drop the URL reqwest prefixes; the monitor already names it.
    msg.replace(&format!(" for url ({})", e.url().map(|u| u.as_str()).unwrap_or("")), "")
}

fn is_network_error(e: &reqwest::Error) -> bool {
    (e.is_connect() || e.is_timeout()) && !describe(e).to_ascii_lowercase().contains("certificate")
}

pub async fn run_http(check: &HttpCheck) -> Probe {
    let (trace, header) = traceparent();
    let client = if check.follow_redirects { &CLIENTS.0 } else { &CLIENTS.1 };
    if let Err(e) = guard().check_url(&check.url) {
        return Probe::new("fail", e);
    }
    let mut req = client.request(check.method.clone(), &check.url).timeout(check.timeout).header("traceparent", header);
    for (k, v) in &check.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let started = Instant::now();
    let resp = req.send().await;
    let mut p = match resp {
        Err(e) => {
            let result = if e.is_timeout() { "timeout" } else { "fail" };
            let mut p = Probe::new(result, describe(&e));
            if e.is_timeout() {
                p.error = format!("no response within {}s", check.timeout.as_secs());
            }
            if is_network_error(&e) && !network_up().await {
                p.result = "inconclusive".into();
                p.error = format!("{} (Vaka could not reach its canary URLs either, so this check is not counted)", p.error);
            }
            p.duration_ms = started.elapsed().as_millis() as i64;
            p
        }
        Ok(r) => {
            let code = r.status().as_u16();
            let not_after = r.extensions().get::<reqwest::tls::TlsInfo>().and_then(|t| t.peer_certificate()).and_then(cert_not_after);
            let mut p = Probe { http_status: code as i64, tls_not_after: not_after, ..Default::default() };
            if !status_ok(&check.expect, code) {
                p.result = "fail".into();
                p.error = format!("HTTP {code}{}", r.status().canonical_reason().map(|s| format!(" {s}")).unwrap_or_default());
            } else if let Some(needle) = &check.body_contains {
                // Read at most 1 MB looking for the text.
                match read_limited(r, 1 << 20, check.timeout.saturating_sub(started.elapsed())).await {
                    Ok(body) if body.contains(needle.as_str()) => p.result = "ok".into(),
                    Ok(_) => {
                        p.result = "fail".into();
                        p.error = format!("response does not contain {needle:?}");
                    }
                    Err(e) => {
                        p.result = "fail".into();
                        p.error = format!("reading the response failed: {e}");
                    }
                }
            } else {
                p.result = "ok".into();
            }
            p.duration_ms = started.elapsed().as_millis() as i64;
            p
        }
    };
    p.trace_id = trace;
    p
}

async fn read_limited(mut r: reqwest::Response, max: usize, timeout: Duration) -> anyhow::Result<String> {
    let mut buf = Vec::new();
    let read = async {
        while let Some(chunk) = r.chunk().await? {
            buf.extend_from_slice(&chunk);
            if buf.len() >= max {
                break;
            }
        }
        anyhow::Ok(())
    };
    tokio::time::timeout(timeout.max(Duration::from_millis(100)), read).await.map_err(|_| anyhow::anyhow!("timed out"))??;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// TLS check: connect to `https://host:port/` and read the certificate. Any HTTP answer is
/// fine; an invalid or expired certificate fails the handshake and the check.
pub async fn run_tls(host: &str, port: u16, timeout: Duration) -> Probe {
    let url = if port == 443 { format!("https://{host}/") } else { format!("https://{host}:{port}/") };
    let started = Instant::now();
    if let Err(e) = guard().check_url(&url) {
        return Probe::new("fail", e);
    }
    let res = CLIENTS.1.head(&url).timeout(timeout).send().await;
    let mut p = match res {
        Ok(r) => match r.extensions().get::<reqwest::tls::TlsInfo>().and_then(|t| t.peer_certificate()).and_then(cert_not_after) {
            Some(na) => Probe { result: "ok".into(), tls_not_after: Some(na), http_status: r.status().as_u16() as i64, ..Default::default() },
            None => Probe::new("fail", "the server sent no readable certificate"),
        },
        Err(e) => {
            let mut p = Probe::new(if e.is_timeout() { "timeout" } else { "fail" }, describe(&e));
            if is_network_error(&e) && !network_up().await {
                p.result = "inconclusive".into();
            }
            p
        }
    };
    p.duration_ms = started.elapsed().as_millis() as i64;
    p
}

// ---------------------------------------------------------------------------
// Canaries
// ---------------------------------------------------------------------------

const DEFAULT_CANARIES: &[&str] = &["https://www.google.com/generate_204", "https://cloudflare.com/cdn-cgi/trace"];

fn canaries() -> Vec<String> {
    match std::env::var("VAKA_MONITOR_CANARY_URLS") {
        Ok(v) if v.trim() == "off" => vec![],
        Ok(v) if !v.trim().is_empty() => v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        _ => DEFAULT_CANARIES.iter().map(|s| s.to_string()).collect(),
    }
}

/// (checked at, up) cached for 60 seconds so a burst of failures costs one round of canaries.
static NETWORK: Lazy<Mutex<Option<(Instant, bool)>>> = Lazy::new(|| Mutex::new(None));

/// Whether Vaka can reach the internet: any canary answering counts. Without canaries
/// (`VAKA_MONITOR_CANARY_URLS=off`) the network is assumed up.
pub async fn network_up() -> bool {
    if let Some((at, up)) = *NETWORK.lock().unwrap_or_else(|e| e.into_inner()) {
        if at.elapsed() < Duration::from_secs(60) {
            return up;
        }
    }
    let urls = canaries();
    if urls.is_empty() {
        return true;
    }
    let client = &CLIENTS.1;
    let checks = urls.iter().map(|u| client.head(u).timeout(Duration::from_secs(5)).send());
    let up = futures::future::join_all(checks).await.into_iter().any(|r| r.is_ok());
    *NETWORK.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), up));
    if !up {
        tracing::warn!("monitor canaries unreachable; connection failures are not counted until the network is back");
    }
    up
}

// ---------------------------------------------------------------------------
// Certificate expiry
// ---------------------------------------------------------------------------

/// Read one DER element: (tag, content, rest).
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let len = if first < 0x80 {
        first as usize
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let mut l = 0usize;
        for &b in &rest[..n] {
            l = (l << 8) | b as usize;
        }
        rest = &rest[n..];
        l
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

/// The `notAfter` of an X.509 certificate, in ms since the epoch.
pub fn cert_not_after(cert: &[u8]) -> Option<i64> {
    let (_, cert, _) = der(cert)?; // Certificate SEQUENCE
    let (_, tbs, _) = der(cert)?; // tbsCertificate SEQUENCE
    let (tag, _, mut rest) = der(tbs)?;
    if tag == 0xa0 {
        // [0] version; the serial number follows.
        rest = der(rest)?.2;
    }
    let rest = der(rest)?.2; // signature AlgorithmIdentifier
    let rest = der(rest)?.2; // issuer
    let (_, validity, _) = der(rest)?;
    let after = der(validity)?.2; // skip notBefore
    let (tag, t, _) = der(after)?;
    let s = std::str::from_utf8(t).ok()?;
    // UTCTime YYMMDDHHMMSSZ or GeneralizedTime YYYYMMDDHHMMSSZ.
    let full = match tag {
        0x17 if s.len() >= 13 => {
            let yy: i32 = s[..2].parse().ok()?;
            format!("{}{}", if yy >= 50 { 1900 + yy } else { 2000 + yy }, &s[2..])
        }
        0x18 if s.len() >= 15 => s.to_string(),
        _ => return None,
    };
    let n = |a: usize, b: usize| full.get(a..b)?.parse::<i16>().ok();
    let dt = jiff::civil::DateTime::new(n(0, 4)?, n(4, 6)? as i8, n(6, 8)? as i8, n(8, 10)? as i8, n(10, 12)? as i8, n(12, 14)? as i8, 0).ok()?;
    Some(dt.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp().as_millisecond())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expect_status() {
        let d = parse_expect(None).unwrap();
        assert!(status_ok(&d, 200) && status_ok(&d, 301) && !status_ok(&d, 404) && !status_ok(&d, 500));
        let e = parse_expect(Some(&json!([200, "4xx"]))).unwrap();
        assert!(status_ok(&e, 200) && status_ok(&e, 404) && !status_ok(&e, 201));
        assert!(parse_expect(Some(&json!("9xx"))).is_err());
        assert!(parse_expect(Some(&json!(42))).is_err());
        assert!(parse_expect(Some(&json!([]))).is_err());
        assert_eq!(parse_expect(Some(&json!("204"))).unwrap(), vec![(204, false)]);
    }

    #[test]
    fn traceparent_format() {
        let (t, h) = traceparent();
        assert_eq!(t.len(), 32);
        assert!(h.starts_with(&format!("00-{t}-")) && h.ends_with("-01") && h.len() == 55);
    }

    fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        if body.len() < 128 {
            v.push(body.len() as u8);
        } else {
            v.extend([0x82, (body.len() >> 8) as u8, body.len() as u8]);
        }
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn reads_not_after() {
        let build = |after: Vec<u8>, version: bool| {
            let validity = tlv(0x30, &[tlv(0x17, b"250101000000Z"), after].concat());
            let mut tbs = Vec::new();
            if version {
                tbs.extend(tlv(0xa0, &tlv(0x02, &[2])));
            }
            tbs.extend(tlv(0x02, &[1, 2, 3]));
            tbs.extend(tlv(0x30, &[0u8; 140])); // long-form length
            tbs.extend(tlv(0x30, b"issuer"));
            tbs.extend(validity);
            tbs.extend(tlv(0x30, b"subject"));
            tlv(0x30, &[tlv(0x30, &tbs), tlv(0x30, b"sigalg")].concat())
        };
        let utc = build(tlv(0x17, b"261231235959Z"), true);
        assert_eq!(cert_not_after(&utc), Some(1_798_761_599_000));
        let gen = build(tlv(0x18, b"20500101000000Z"), false);
        assert_eq!(cert_not_after(&gen), Some(2_524_608_000_000));
        assert_eq!(cert_not_after(b"\x30\x03\x01"), None);
    }
}
