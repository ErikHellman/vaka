//! Issue grouping: turn an error event into a stable fingerprint.

use crate::models::{ExceptionData, Frame};
use once_cell::sync::Lazy;
use regex::Regex;
use sha2::{Digest, Sha256};

const FRAMES_IN_FINGERPRINT: usize = 5;

static NUMBERS: Lazy<Regex> = Lazy::new(|| Regex::new(r"0x[0-9a-fA-F]+|\b\d+\b").unwrap());
static HEXISH: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b[0-9a-fA-F]{8,}\b").unwrap());
static QUOTED: Lazy<Regex> = Lazy::new(|| Regex::new(r#"'[^']*'|"[^"]*""#).unwrap());

/// Remove volatile parts (numbers, ids, quoted values) from a message.
pub fn normalize_message(msg: &str) -> String {
    let s = QUOTED.replace_all(msg, "<str>");
    let s = HEXISH.replace_all(&s, "<hex>");
    let s = NUMBERS.replace_all(&s, "<n>");
    s.chars().take(200).collect()
}

fn is_in_app(f: &Frame) -> bool {
    f.in_app.unwrap_or(true)
}

fn frame_key(f: &Frame) -> String {
    let func = f.function.clone().unwrap_or_default();
    // Strip line numbers and hashed chunk names; keep the logical location.
    let loc = f
        .module
        .clone()
        .or_else(|| f.filename.clone())
        .or_else(|| f.abs_path.as_ref().map(|p| strip_url(p)))
        .unwrap_or_default();
    let loc = HEXISH.replace_all(&loc, "<hex>").to_string();
    format!("{loc}::{func}")
}

fn strip_url(p: &str) -> String {
    match url::Url::parse(p) {
        Ok(u) => u.path().to_string(),
        Err(_) => p.to_string(),
    }
}

/// Frames to use for grouping: in-app frames if there are any, else all.
pub fn grouping_frames(frames: &[Frame]) -> Vec<&Frame> {
    let in_app: Vec<&Frame> = frames.iter().filter(|f| is_in_app(f) && f.function.is_some()).collect();
    if !in_app.is_empty() {
        return in_app.into_iter().take(FRAMES_IN_FINGERPRINT).collect();
    }
    frames.iter().filter(|f| f.function.is_some()).take(FRAMES_IN_FINGERPRINT).collect()
}

pub fn fingerprint(
    platform: &str,
    exceptions: &[ExceptionData],
    message: Option<&str>,
    custom: Option<&[String]>,
) -> String {
    let mut h = Sha256::new();
    if let Some(parts) = custom.filter(|p| !p.is_empty()) {
        h.update(b"custom\0");
        for p in parts {
            h.update(p.as_bytes());
            h.update(b"\0");
        }
        return hex::encode(&h.finalize()[..16]);
    }
    h.update(platform.as_bytes());
    h.update(b"\0");
    match exceptions.first() {
        Some(exc) => {
            h.update(exc.ty.as_bytes());
            h.update(b"\0");
            let frames = grouping_frames(&exc.stacktrace);
            if frames.is_empty() {
                h.update(normalize_message(&exc.value).as_bytes());
            } else {
                for f in frames {
                    h.update(frame_key(f).as_bytes());
                    h.update(b"\0");
                }
            }
        }
        None => {
            h.update(b"message\0");
            h.update(normalize_message(message.unwrap_or("")).as_bytes());
        }
    }
    hex::encode(&h.finalize()[..16])
}

/// Human-readable title for an issue.
pub fn title(exceptions: &[ExceptionData], message: Option<&str>) -> String {
    match exceptions.first() {
        Some(e) if !e.value.is_empty() && !e.ty.is_empty() => {
            crate::util::truncate(&format!("{}: {}", e.ty, e.value.lines().next().unwrap_or("")), 200)
        }
        Some(e) if !e.ty.is_empty() => e.ty.clone(),
        Some(e) => crate::util::truncate(&e.value, 200),
        None => crate::util::truncate(message.unwrap_or("Unknown error"), 200),
    }
}

/// Where the error happened: the top in-app frame, formatted for humans.
pub fn culprit(exceptions: &[ExceptionData]) -> String {
    let Some(exc) = exceptions.first() else { return String::new() };
    let frame = exc
        .stacktrace
        .iter()
        .find(|f| is_in_app(f) && f.function.is_some())
        .or_else(|| exc.stacktrace.first());
    match frame {
        Some(f) => {
            let func = f.function.clone().unwrap_or_else(|| "?".into());
            let loc = f
                .filename
                .clone()
                .or_else(|| f.module.clone())
                .or_else(|| f.abs_path.as_ref().map(|p| strip_url(p)))
                .unwrap_or_default();
            match (loc.is_empty(), f.lineno) {
                (true, _) => func,
                (false, Some(l)) => format!("{func} ({loc}:{l})"),
                (false, None) => format!("{func} ({loc})"),
            }
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exc(ty: &str, value: &str, funcs: &[(&str, u32)]) -> ExceptionData {
        ExceptionData {
            ty: ty.into(),
            value: value.into(),
            module: None,
            stacktrace: funcs
                .iter()
                .map(|(f, l)| Frame {
                    function: Some(f.to_string()),
                    filename: Some("app.js".into()),
                    lineno: Some(*l),
                    ..Default::default()
                })
                .collect(),
        }
    }

    #[test]
    fn same_stack_different_lines_and_values_group_together() {
        let a = fingerprint("js", &[exc("TypeError", "x is 1", &[("foo", 10), ("bar", 20)])], None, None);
        let b = fingerprint("js", &[exc("TypeError", "x is 2", &[("foo", 11), ("bar", 25)])], None, None);
        assert_eq!(a, b);
    }

    #[test]
    fn different_functions_split() {
        let a = fingerprint("js", &[exc("TypeError", "", &[("foo", 1)])], None, None);
        let b = fingerprint("js", &[exc("TypeError", "", &[("baz", 1)])], None, None);
        assert_ne!(a, b);
    }

    #[test]
    fn message_only_normalizes_numbers() {
        let a = fingerprint("rust", &[], Some("timeout after 30s for user 1234"), None);
        let b = fingerprint("rust", &[], Some("timeout after 31s for user 99"), None);
        // "30s" is not a standalone number, so these differ only by user id.
        assert_eq!(normalize_message("user 1234 id 'abc'"), "user <n> id <str>");
        assert_ne!(a, String::new());
        assert_ne!(b, String::new());
    }

    #[test]
    fn culprit_formats_location() {
        let e = exc("E", "v", &[("handler", 42)]);
        assert_eq!(culprit(&[e]), "handler (app.js:42)");
    }
}
