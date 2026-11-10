use serde_json::{json, Value};

const NOT_IN_APP: &[&str] = &[
    "std::", "core::", "alloc::", "backtrace::", "vaka_sdk::", "tokio::", "futures", "rust_begin_unwind",
    "__rust", "_start", "__libc_start", "start_thread", "clone", "<core::", "<alloc::", "<std::", "<tokio::",
];

fn in_app(name: &str, include: &[String]) -> bool {
    // `<&dyn core::ops::Fn ...>` / `<T as std::...>` style names: look at the inner path.
    let name = name.trim_start_matches(['<', '&']).trim_start_matches("dyn ").trim_start_matches("mut ");
    if name.is_empty() || name == "?" || name == "main" || name.starts_with("fn(") || !name.contains("::") && name.starts_with('_') {
        return false;
    }
    if !include.is_empty() {
        return include.iter().any(|p| name.starts_with(p.as_str()) || name.starts_with(&format!("<{p}")));
    }
    !NOT_IN_APP.iter().any(|p| name.starts_with(p))
}

/// Capture the current stack, crash site first, skipping `skip` SDK frames.
pub fn capture_backtrace(skip: usize, include: &[String]) -> Vec<Value> {
    let bt = backtrace::Backtrace::new();
    let mut out = Vec::new();
    for frame in bt.frames() {
        let ip = frame.ip() as usize;
        let symbols = frame.symbols();
        if symbols.is_empty() {
            out.push(json!({"instruction_addr": format!("{ip:#x}"), "in_app": false}));
            continue;
        }
        for sym in symbols {
            let name = sym.name().map(|n| format!("{n:#}")).unwrap_or_default();
            let mut f = serde_json::Map::new();
            f.insert("instruction_addr".into(), json!(format!("{ip:#x}")));
            if !name.is_empty() {
                f.insert("function".into(), json!(name));
            }
            if let Some(file) = sym.filename() {
                let p = file.to_string_lossy().to_string();
                f.insert("filename".into(), json!(file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
                f.insert("abs_path".into(), json!(p));
            }
            if let Some(l) = sym.lineno() {
                f.insert("lineno".into(), json!(l));
            }
            if let Some(c) = sym.colno() {
                f.insert("colno".into(), json!(c));
            }
            f.insert("in_app".into(), json!(!name.is_empty() && in_app(&name, include)));
            out.push(Value::Object(f));
        }
    }
    // Drop frames from inside the backtrace crate and this SDK.
    let start = out
        .iter()
        .position(|f| {
            let n = f["function"].as_str().unwrap_or("");
            !(n.starts_with("backtrace::") || (n.starts_with("vaka_sdk::") && !n.starts_with("vaka_sdk::tests::")) || n.starts_with("<backtrace::"))
        })
        .unwrap_or(0);
    out.into_iter().skip(start + skip.saturating_sub(0)).take(100).collect()
}

fn is_panic_machinery(n: &str) -> bool {
    if n.contains("catch_unwind") || n.contains("::try") {
        return false;
    }
    n.starts_with("std::panicking::") || n.starts_with("core::panicking::") || n.contains("rust_begin_unwind")
        || n.starts_with("std::sys::backtrace::__rust_end_short_backtrace") || n.contains("panic_fmt")
        || n.starts_with("core::option::expect_failed") || n.starts_with("core::option::unwrap_failed")
        || n.starts_with("core::result::unwrap_failed") || n.starts_with("std::rt::begin_panic")
}

/// Remove the panic hook and panic machinery frames so frames[0] is where `panic!` was called.
pub fn trim_panic_frames(frames: Vec<Value>) -> Vec<Value> {
    let name = |f: &Value| f["function"].as_str().unwrap_or("").to_string();
    let Some(first) = frames.iter().position(|f| is_panic_machinery(&name(f))) else { return frames };
    let mut last = first;
    while last + 1 < frames.len() && is_panic_machinery(&name(&frames[last + 1])) {
        last += 1;
    }
    frames.into_iter().skip(last + 1).collect()
}

/// Loaded modules, so the server can symbolicate stripped binaries with uploaded debug files.
pub fn debug_images() -> Vec<Value> {
    use findshlibs::{SharedLibrary, SharedLibraryId, TargetSharedLibrary};
    let mut out = Vec::new();
    TargetSharedLibrary::each(|lib| {
        let mut img = serde_json::Map::new();
        img.insert("code_file".into(), json!(lib.name().to_string_lossy()));
        img.insert("image_addr".into(), json!(format!("{:#x}", lib.actual_load_addr().0)));
        img.insert("image_size".into(), json!(lib.len()));
        match lib.id() {
            Some(SharedLibraryId::GnuBuildId(bytes)) => {
                img.insert("type".into(), json!("elf"));
                img.insert("code_id".into(), json!(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()));
            }
            Some(SharedLibraryId::Uuid(bytes)) => {
                img.insert("type".into(), json!("macho"));
                let u = uuid::Uuid::from_bytes(bytes);
                img.insert("debug_id".into(), json!(u.to_string()));
            }
            Some(SharedLibraryId::PdbSignature(guid, age)) => {
                img.insert("type".into(), json!("pe"));
                let u = uuid::Uuid::from_bytes_le(guid);
                img.insert("debug_id".into(), json!(format!("{u}-{age:x}")));
            }
            _ => return,
        }
        out.push(Value::Object(img));
    });
    out
}

pub fn type_name_of_error(e: &(dyn std::error::Error + 'static)) -> String {
    // `Debug` usually starts with the type or variant name: "Os { code: 2, ... }" or "ParseIntError { .. }".
    let dbg = format!("{e:?}");
    let ty: String = dbg.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
    if ty.is_empty() { "Error".into() } else { ty }
}
