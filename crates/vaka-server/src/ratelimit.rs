//! Small in-memory fixed-window rate limiter, shared by login, email codes, feedback and ingest.
//!
//! Memory is bounded: when the map passes `max_keys`, windows from earlier periods are dropped,
//! and if that is not enough the map is cleared (fail-open, so a flood of distinct keys can't
//! exhaust memory or lock real users out).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

pub struct Limiter {
    windows: Mutex<HashMap<String, (i64, u32)>>,
    window_ms: i64,
    max_keys: usize,
}

impl Limiter {
    pub fn new(window_ms: i64, max_keys: usize) -> Self {
        Limiter {
            windows: Mutex::new(HashMap::new()),
            window_ms,
            max_keys,
        }
    }

    fn window(&self, now: i64) -> i64 {
        now / self.window_ms
    }

    /// Hits counted for `key` in the current window.
    pub fn count(&self, key: &str, now: i64) -> u32 {
        let w = self.window(now);
        let m = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        m.get(key).filter(|e| e.0 == w).map(|e| e.1).unwrap_or(0)
    }

    /// Count one hit for `key`.
    pub fn hit(&self, key: &str, now: i64) {
        self.add(key, 1, now)
    }

    fn add(&self, key: &str, n: u32, now: i64) {
        let w = self.window(now);
        let mut m = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= self.max_keys && !m.contains_key(key) {
            m.retain(|_, e| e.0 == w);
            if m.len() >= self.max_keys {
                m.clear();
            }
        }
        let e = m.entry(key.to_string()).or_insert((w, 0));
        if e.0 != w {
            *e = (w, 0);
        }
        e.1 = e.1.saturating_add(n);
    }

    /// Count `n` hits at once (for example the items of a batch) if they fit within `limit`.
    pub fn take_n(&self, key: &str, n: u32, limit: u32, now: i64) -> bool {
        if limit == 0 {
            return true;
        }
        if self.count(key, now).saturating_add(n) > limit {
            return false;
        }
        self.add(key, n, now);
        true
    }

    /// Count a hit and return whether `key` is still within `limit` (0 = unlimited).
    pub fn take(&self, key: &str, limit: u32, now: i64) -> bool {
        if limit == 0 {
            return true;
        }
        if self.count(key, now) >= limit {
            return false;
        }
        self.hit(key, now);
        true
    }
}

/// The part of an address that identifies one client: the full IPv4 address, or the /64 of an
/// IPv6 address (one subscriber usually holds a whole /64).
pub fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_and_resets() {
        let l = Limiter::new(1000, 100);
        assert!(l.take("a", 2, 0));
        assert!(l.take("a", 2, 10));
        assert!(!l.take("a", 2, 20));
        assert!(l.take("a", 2, 1500)); // next window
    }

    #[test]
    fn memory_is_bounded() {
        let l = Limiter::new(1000, 50);
        for i in 0..10_000 {
            l.hit(&format!("k{i}"), 0);
        }
        assert!(l.windows.lock().unwrap().len() <= 50);
    }

    #[test]
    fn ipv6_keys_by_prefix() {
        assert_eq!(
            ip_key("2001:db8:1:2:aaaa::1".parse().unwrap()),
            ip_key("2001:db8:1:2:bbbb::9".parse().unwrap())
        );
        assert_ne!(
            ip_key("2001:db8:1:2::1".parse().unwrap()),
            ip_key("2001:db8:1:3::1".parse().unwrap())
        );
        assert_eq!(ip_key("::ffff:1.2.3.4".parse().unwrap()), "1.2.3.4");
    }
}

/// Shared limiters.
pub static LOGIN: once_cell::sync::Lazy<Limiter> =
    once_cell::sync::Lazy::new(|| Limiter::new(15 * 60_000, 100_000));
pub static FEEDBACK: once_cell::sync::Lazy<Limiter> =
    once_cell::sync::Lazy::new(|| Limiter::new(3_600_000, 100_000));
pub static INGEST: once_cell::sync::Lazy<Limiter> =
    once_cell::sync::Lazy::new(|| Limiter::new(60_000, 200_000));
pub static NEW_ISSUES: once_cell::sync::Lazy<Limiter> =
    once_cell::sync::Lazy::new(|| Limiter::new(3_600_000, 10_000));
