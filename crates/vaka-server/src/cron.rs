//! Five-field cron expressions (minute hour day-of-month month day-of-week) evaluated in an
//! IANA time zone, for heartbeat monitor schedules.
//!
//! Supports `*`, numbers, ranges (`1-5`), steps (`*/15`, `0-30/10`), lists (`1,15`), month and
//! weekday names (`jan`, `mon`), day-of-week 7 as Sunday, and the macros `@hourly`, `@daily`,
//! `@midnight`, `@weekly`, `@monthly`, `@yearly` and `@annually`. As in Vixie cron, when both
//! day-of-month and day-of-week are restricted a day matches if either does.

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;

#[derive(Clone, Debug, PartialEq)]
pub struct Cron {
    minutes: u64,
    hours: u32,
    dom: u32,
    months: u16,
    dow: u8,
    dom_any: bool,
    dow_any: bool,
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn parse_value(s: &str, names: &[&str], offset: u32) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    let l = s.to_ascii_lowercase();
    names.iter().position(|n| *n == l).map(|i| i as u32 + offset)
}

/// Parse one field into a bit set of allowed values in `min..=max`.
fn field(spec: &str, min: u32, max: u32, names: &[&str], name_offset: u32, what: &str) -> Result<(u64, bool), String> {
    let mut bits = 0u64;
    let any = spec == "*" || spec == "?";
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().map_err(|_| format!("bad step `{s}` in {what}"))?),
            None => (part, 1),
        };
        if step == 0 {
            return Err(format!("step 0 in {what}"));
        }
        let (lo, hi) = if range == "*" || range == "?" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let a = parse_value(a, names, name_offset).ok_or_else(|| format!("bad value `{a}` in {what}"))?;
            let b = parse_value(b, names, name_offset).ok_or_else(|| format!("bad value `{b}` in {what}"))?;
            (a, b)
        } else {
            let a = parse_value(range, names, name_offset).ok_or_else(|| format!("bad value `{range}` in {what}"))?;
            // `5/15` means from 5 to the end in steps of 15.
            (a, if part.contains('/') { max } else { a })
        };
        if lo < min || hi > max || lo > hi {
            return Err(format!("{what} must be within {min}-{max}"));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, any))
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let expr = expr.trim();
        let expanded = match expr.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *".to_string(),
            "@daily" | "@midnight" => "0 0 * * *".to_string(),
            "@weekly" => "0 0 * * 0".to_string(),
            "@monthly" => "0 0 1 * *".to_string(),
            "@yearly" | "@annually" => "0 0 1 1 *".to_string(),
            _ => expr.to_string(),
        };
        let parts: Vec<&str> = expanded.split_whitespace().collect();
        if parts.len() != 5 {
            return Err("a cron expression has 5 fields: minute hour day-of-month month day-of-week".into());
        }
        let (minutes, _) = field(parts[0], 0, 59, &[], 0, "minute")?;
        let (hours, _) = field(parts[1], 0, 23, &[], 0, "hour")?;
        let (dom, dom_any) = field(parts[2], 1, 31, &[], 0, "day of month")?;
        let (months, _) = field(parts[3], 1, 12, &MONTHS, 1, "month")?;
        let (mut dow, dow_any) = field(parts[4], 0, 7, &DAYS, 0, "day of week")?;
        if dow & (1 << 7) != 0 {
            dow = (dow | 1) & 0x7f;
        }
        Ok(Cron { minutes, hours: hours as u32, dom: dom as u32, months: months as u16, dow: dow as u8, dom_any, dow_any })
    }

    fn day_matches(&self, d: Date) -> bool {
        if self.months & (1 << d.month()) == 0 {
            return false;
        }
        let dom_ok = self.dom & (1 << d.day()) != 0;
        let dow_ok = self.dow & (1 << d.weekday().to_sunday_zero_offset()) != 0;
        match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (false, true) => dom_ok,
            (true, false) => dow_ok,
            (false, false) => dom_ok || dow_ok,
        }
    }

    /// The first scheduled time strictly after `after_ms`, in milliseconds since the epoch.
    pub fn next_after(&self, after_ms: i64, tz: &TimeZone) -> Option<i64> {
        let start = jiff::Timestamp::from_millisecond(after_ms).ok()?.to_zoned(tz.clone()).datetime();
        // Next whole minute after `start`.
        let mut dt = start.with().second(0).subsec_nanosecond(0).build().ok()?.checked_add(jiff::Span::new().minutes(1)).ok()?;
        // Five years covers any satisfiable expression (Feb 29 on a given weekday is the worst).
        let limit = start.checked_add(jiff::Span::new().days(366 * 5)).ok()?;
        while dt < limit {
            if !self.day_matches(dt.date()) {
                dt = DateTime::from(dt.date().tomorrow().ok()?);
                continue;
            }
            if self.hours & (1 << dt.hour()) == 0 {
                dt = dt.with().minute(0).build().ok()?.checked_add(jiff::Span::new().hours(1)).ok()?;
                continue;
            }
            if self.minutes & (1 << dt.minute()) == 0 {
                dt = dt.checked_add(jiff::Span::new().minutes(1)).ok()?;
                continue;
            }
            let ts = dt.to_zoned(tz.clone()).ok()?.timestamp().as_millisecond();
            if ts > after_ms {
                return Some(ts);
            }
            dt = dt.checked_add(jiff::Span::new().minutes(1)).ok()?;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str, tz: &str) -> i64 {
        let dt: DateTime = s.parse().unwrap();
        dt.to_zoned(TimeZone::get(tz).unwrap()).unwrap().timestamp().as_millisecond()
    }

    #[test]
    fn parses_and_rejects() {
        assert!(Cron::parse("0 3 * * *").is_ok());
        assert!(Cron::parse("*/15 9-17 * * mon-fri").is_ok());
        assert!(Cron::parse("@daily").is_ok());
        assert!(Cron::parse("0 3 * *").is_err());
        assert!(Cron::parse("60 * * * *").is_err());
        assert!(Cron::parse("*/0 * * * *").is_err());
        assert!(Cron::parse("0 0 * * 8").is_err());
    }

    #[test]
    fn next_runs() {
        let utc = TimeZone::UTC;
        let c = Cron::parse("0 3 * * *").unwrap();
        assert_eq!(c.next_after(ms("2026-11-14T02:59:59", "UTC"), &utc), Some(ms("2026-11-14T03:00:00", "UTC")));
        assert_eq!(c.next_after(ms("2026-11-14T03:00:00", "UTC"), &utc), Some(ms("2026-11-15T03:00:00", "UTC")));
        let c = Cron::parse("*/15 * * * *").unwrap();
        assert_eq!(c.next_after(ms("2026-11-14T10:07:00", "UTC"), &utc), Some(ms("2026-11-14T10:15:00", "UTC")));
        // Weekdays only: Saturday 2026-11-14 -> Monday 2026-11-16.
        let c = Cron::parse("30 9 * * 1-5").unwrap();
        assert_eq!(c.next_after(ms("2026-11-14T12:00:00", "UTC"), &utc), Some(ms("2026-11-16T09:30:00", "UTC")));
        // Sunday as 7.
        let c = Cron::parse("0 0 * * 7").unwrap();
        assert_eq!(c.next_after(ms("2026-11-14T12:00:00", "UTC"), &utc), Some(ms("2026-11-15T00:00:00", "UTC")));
        // Day-of-month OR day-of-week when both are set.
        let c = Cron::parse("0 0 1 * mon").unwrap();
        assert_eq!(c.next_after(ms("2026-11-14T12:00:00", "UTC"), &utc), Some(ms("2026-11-16T00:00:00", "UTC")));
        // Leap day.
        let c = Cron::parse("0 0 29 2 *").unwrap();
        assert_eq!(c.next_after(ms("2026-03-01T00:00:00", "UTC"), &utc), Some(ms("2028-02-29T00:00:00", "UTC")));
    }

    #[test]
    fn time_zones() {
        let tz = TimeZone::get("Europe/Stockholm").unwrap();
        let c = Cron::parse("0 3 * * *").unwrap();
        assert_eq!(c.next_after(ms("2026-11-14T00:00:00", "Europe/Stockholm"), &tz), Some(ms("2026-11-14T03:00:00", "Europe/Stockholm")));
        // 02:30 does not exist on the spring-forward day; it runs once, just after the gap.
        let c = Cron::parse("30 2 * * *").unwrap();
        let next = c.next_after(ms("2026-03-29T00:00:00", "Europe/Stockholm"), &tz).unwrap();
        assert_eq!(next, ms("2026-03-29T03:30:00", "Europe/Stockholm"));
    }
}
