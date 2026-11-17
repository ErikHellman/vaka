//! Server-side symbolication for JS source maps, Android R8/ProGuard mappings,
//! and native debug files (iOS dSYM, Rust/ELF DWARF, Mach-O, PDB).

use crate::db::RowExt;
use crate::models::{DebugImage, ExceptionData, Frame};
use crate::state::AppState;
use proguard::{ProguardMapper, ProguardMapping, StackFrame};
use sourcemap::SourceMap;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use symbolic::common::{ByteView, DebugId, Language, Name, NameMangling};
use symbolic::debuginfo::Archive;
use symbolic::demangle::{Demangle, DemangleOptions};
use symbolic::symcache::{SymCache, SymCacheConverter};

self_cell::self_cell!(
    /// A ProGuard mapper together with the mapping bytes it borrows.
    pub struct OwnedProguard {
        owner: Vec<u8>,
        #[covariant]
        dependent: ProguardMapper,
    }
);

self_cell::self_cell!(
    /// A SymCache together with the buffer it borrows.
    pub struct OwnedSymCache {
        owner: Vec<u8>,
        #[covariant]
        dependent: SymCache,
    }
);

/// A small LRU cache bounded by the bytes of the cached artifacts.
struct Lru<V> {
    map: HashMap<String, (Arc<V>, usize, u64)>,
    bytes: usize,
    tick: u64,
    budget: usize,
}

impl<V> Lru<V> {
    fn new(budget: usize) -> Self {
        Lru {
            map: HashMap::new(),
            bytes: 0,
            tick: 0,
            budget,
        }
    }

    fn get(&mut self, key: &str) -> Option<Arc<V>> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(key).map(|e| {
            e.2 = tick;
            e.0.clone()
        })
    }

    fn insert(&mut self, key: String, value: Arc<V>, size: usize) {
        if size > self.budget {
            return; // Too big to keep; the caller still uses it once and then drops it.
        }
        if let Some((_, old, _)) = self.map.remove(&key) {
            self.bytes -= old;
        }
        while self.bytes + size > self.budget {
            let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.2)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some((_, n, _)) = self.map.remove(&oldest) {
                self.bytes -= n;
            }
        }
        self.tick += 1;
        self.bytes += size;
        self.map.insert(key, (value, size, self.tick));
    }
}

/// Parsed artifacts are expensive to build, so they are cached by artifact id in LRU caches
/// bounded by bytes (`VAKA_SYMBOL_CACHE_MB`, default 256, split between the three kinds).
/// Artifacts that fail to parse are remembered for a while so they aren't re-read per event.
pub struct SymbolCache {
    sourcemaps: Mutex<Lru<SourceMap>>,
    proguard: Mutex<Lru<OwnedProguard>>,
    symcaches: Mutex<Lru<OwnedSymCache>>,
    failed: Mutex<HashMap<String, i64>>,
}

impl Default for SymbolCache {
    fn default() -> Self {
        let mb: usize = std::env::var("VAKA_SYMBOL_CACHE_MB")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(256);
        let each = mb * 1024 * 1024 / 3;
        SymbolCache {
            sourcemaps: Mutex::new(Lru::new(each)),
            proguard: Mutex::new(Lru::new(each)),
            symcaches: Mutex::new(Lru::new(each)),
            failed: Mutex::new(HashMap::new()),
        }
    }
}

/// How long an artifact that failed to load is skipped.
const FAILED_TTL_MS: i64 = 10 * 60 * 1000;

#[derive(Clone, Debug)]
struct ArtifactRef {
    id: String,
    kind: String,
    name: String,
    debug_id: String,
    path: String,
}

/// Normalize debug ids (lowercase, canonical form) and derive them from ELF
/// build ids when only a code id is given.
pub fn normalize_images(images: &[DebugImage]) -> Vec<DebugImage> {
    images
        .iter()
        .map(|img| {
            let mut img = img.clone();
            let from_debug = img.debug_id.as_deref().and_then(|d| DebugId::from_str(d).ok());
            let id = from_debug.or_else(|| img.code_id.as_deref().and_then(debug_id_from_code_id));
            img.debug_id = id.map(|d| d.to_string().to_lowercase());
            img
        })
        .collect()
}

/// ELF build id -> debug id (first 16 bytes read as a little-endian GUID).
pub fn debug_id_from_code_id(code_id: &str) -> Option<DebugId> {
    let bytes = hex::decode(code_id.trim()).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let mut guid = [0u8; 16];
    let n = bytes.len().min(16);
    guid[..n].copy_from_slice(&bytes[..n]);
    DebugId::from_guid_age(&guid, 0).ok()
}

fn parse_addr(s: &str) -> Option<u64> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

fn url_path(p: &str) -> String {
    match url::Url::parse(p) {
        Ok(u) => u.path().to_string(),
        Err(_) => p.to_string(),
    }
}

/// Normalize an artifact or frame name for source map matching: URL path, no `~`, no `.map`.
pub fn sourcemap_key(name: &str) -> String {
    let p = url_path(name.trim());
    let p = p.trim_start_matches('~');
    let p = p.strip_suffix(".map").unwrap_or(p);
    if p.starts_with('/') { p.to_string() } else { format!("/{p}") }
}

async fn load_artifacts(state: &AppState, project_id: &str, release: &str, debug_ids: &[String]) -> Vec<ArtifactRef> {
    let mut out = Vec::new();
    if !release.is_empty() {
        if let Ok(rows) = sqlx::query("SELECT id, kind, name, debug_id, path FROM artifacts WHERE project_id = $1 AND release = $2 AND kind IN ('sourcemap', 'proguard') ORDER BY created_at DESC")
            .bind(project_id)
            .bind(release)
            .fetch_all(&state.db)
            .await
        {
            out.extend(rows.iter().map(|r| ArtifactRef { id: r.s("id"), kind: r.s("kind"), name: r.s("name"), debug_id: r.s("debug_id"), path: r.s("path") }));
        }
    }
    for d in debug_ids {
        if let Ok(Some(r)) = sqlx::query("SELECT id, kind, name, debug_id, path FROM artifacts WHERE project_id = $1 AND debug_id = $2 AND kind = 'native' ORDER BY created_at DESC LIMIT 1")
            .bind(project_id)
            .bind(d)
            .fetch_optional(&state.db)
            .await
        {
            out.push(ArtifactRef { id: r.s("id"), kind: r.s("kind"), name: r.s("name"), debug_id: r.s("debug_id"), path: r.s("path") });
        }
    }
    if state.s3.is_some() {
        // Files may only be in the bucket (new container, other instance): fetch missing ones.
        for a in &out {
            crate::s3::ensure_local(state, &a.path).await;
        }
    }
    out
}

/// Symbolicate all frames in place. Returns true if any frame was rewritten.
pub async fn symbolicate(
    state: &AppState,
    project_id: &str,
    release: &str,
    platform: &str,
    exceptions: &mut [ExceptionData],
    images: &[DebugImage],
) -> bool {
    let needs_native = exceptions.iter().any(|e| e.stacktrace.iter().any(|f| f.instruction_addr.is_some()));
    let debug_ids: Vec<String> = if needs_native {
        images.iter().filter_map(|i| i.debug_id.clone()).collect()
    } else {
        Vec::new()
    };
    let artifacts = load_artifacts(state, project_id, release, &debug_ids).await;
    if artifacts.is_empty() {
        return false;
    }
    let state2 = state.clone();
    let mut exc = exceptions.to_vec();
    let images = images.to_vec();
    let platform = platform.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let changed = symbolicate_sync(&state2.symbols, &artifacts, &platform, &mut exc, &images);
        (changed, exc)
    })
    .await;
    match result {
        Ok((changed, exc)) => {
            exceptions.clone_from_slice(&exc);
            changed
        }
        Err(e) => {
            tracing::error!("symbolication task failed: {e}");
            false
        }
    }
}

fn symbolicate_sync(
    cache: &SymbolCache,
    artifacts: &[ArtifactRef],
    platform: &str,
    exceptions: &mut [ExceptionData],
    images: &[DebugImage],
) -> bool {
    let mut changed = false;
    let proguard_owned = artifacts
        .iter()
        .find(|a| a.kind == "proguard")
        .and_then(|a| cache.proguard(a));
    let proguard = proguard_owned.as_ref().map(|p| p.borrow_dependent());
    for exc in exceptions.iter_mut() {
        if let Some(mapper) = proguard {
            if let Some(orig) = mapper.remap_class(&exc.ty) {
                exc.ty = orig.to_string();
                changed = true;
            }
        }
        let mut out: Vec<Frame> = Vec::with_capacity(exc.stacktrace.len());
        // JS: the original name at a call site names the *callee*, i.e. the previous frame.
        let mut js_names: Vec<Option<String>> = Vec::new();
        let mut js_frames: Vec<usize> = Vec::new();
        for (idx, frame) in exc.stacktrace.iter().enumerate() {
            while js_names.len() < out.len() {
                js_names.push(None);
            }
            if frame.symbolicated == Some(true) {
                out.push(frame.clone());
                continue;
            }
            if frame.instruction_addr.is_some() {
                if let Some(f) = symbolicate_native(cache, artifacts, images, frame, idx == 0) {
                    out.extend(f);
                    changed = true;
                    continue;
                }
            } else if let (Some(mapper), Some(_)) = (proguard, frame.module.as_ref()) {
                if let Some(f) = remap_jvm(mapper, frame) {
                    out.extend(f);
                    changed = true;
                    continue;
                }
            } else if frame.abs_path.is_some() && frame.lineno.is_some() && platform != "android" {
                if let Some((f, name)) = symbolicate_js(cache, artifacts, frame) {
                    js_frames.push(out.len());
                    js_names.push(name);
                    out.push(f);
                    changed = true;
                    continue;
                }
            }
            out.push(frame.clone());
        }
        while js_names.len() < out.len() {
            js_names.push(None);
        }
        for i in js_frames {
            if let Some(Some(name)) = js_names.get(i + 1) {
                out[i].function = Some(name.clone());
            }
        }
        exc.stacktrace = out;
    }
    changed
}

impl SymbolCache {
    fn recently_failed(&self, id: &str) -> bool {
        let now = crate::util::now_ms();
        self.failed
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|t| now - t < FAILED_TTL_MS)
    }

    fn mark_failed(&self, id: &str) {
        let now = crate::util::now_ms();
        let mut f = self.failed.lock().unwrap();
        if f.len() > 10_000 {
            f.retain(|_, t| now - *t < FAILED_TTL_MS);
        }
        f.insert(id.to_string(), now);
    }

    fn sourcemap(&self, a: &ArtifactRef) -> Option<Arc<SourceMap>> {
        if let Some(sm) = self.sourcemaps.lock().unwrap().get(&a.id) {
            return Some(sm);
        }
        if self.recently_failed(&a.id) {
            return None;
        }
        let parsed = std::fs::read(&a.path).ok().and_then(|bytes| {
            let sm = match SourceMap::from_slice(&bytes) {
                Ok(sm) => sm,
                Err(_) => sourcemap::SourceMapIndex::from_slice(&bytes)
                    .ok()?
                    .flatten()
                    .ok()?,
            };
            Some((Arc::new(sm), bytes.len()))
        });
        let Some((sm, size)) = parsed else {
            self.mark_failed(&a.id);
            return None;
        };
        self.sourcemaps
            .lock()
            .unwrap()
            .insert(a.id.clone(), sm.clone(), size);
        Some(sm)
    }

    fn proguard(&self, a: &ArtifactRef) -> Option<Arc<OwnedProguard>> {
        if let Some(m) = self.proguard.lock().unwrap().get(&a.id) {
            return Some(m);
        }
        if self.recently_failed(&a.id) {
            return None;
        }
        let loaded = std::fs::read(&a.path).ok().and_then(|bytes| {
            let size = bytes.len();
            OwnedProguard::try_new(bytes, |b| {
                let mapping = ProguardMapping::new(b);
                if mapping.is_valid() {
                    Ok(ProguardMapper::new(mapping))
                } else {
                    Err(())
                }
            })
            .ok()
            .map(|m| (Arc::new(m), size))
        });
        let Some((mapper, size)) = loaded else {
            self.mark_failed(&a.id);
            return None;
        };
        self.proguard
            .lock()
            .unwrap()
            .insert(a.id.clone(), mapper.clone(), size);
        Some(mapper)
    }

    fn symcache(&self, a: &ArtifactRef) -> Option<Arc<OwnedSymCache>> {
        if let Some(s) = self.symcaches.lock().unwrap().get(&a.id) {
            return Some(s);
        }
        if self.recently_failed(&a.id) {
            return None;
        }
        let loaded = build_symcache(&a.path, &a.debug_id).ok().and_then(|buf| {
            let size = buf.len();
            OwnedSymCache::try_new(buf, |b| SymCache::parse(b))
                .ok()
                .map(|s| (Arc::new(s), size))
        });
        let Some((sc, size)) = loaded else {
            self.mark_failed(&a.id);
            return None;
        };
        self.symcaches
            .lock()
            .unwrap()
            .insert(a.id.clone(), sc.clone(), size);
        Some(sc)
    }
}

/// Whether bytes look like a ProGuard/R8 mapping file (used to validate uploads).
pub fn is_valid_proguard(bytes: &[u8]) -> bool {
    ProguardMapping::new(bytes).is_valid()
}

/// Build a SymCache for the object with `debug_id` inside the debug file at `path`.
pub fn build_symcache(path: &str, debug_id: &str) -> anyhow::Result<Vec<u8>> {
    let view = ByteView::open(path)?;
    let archive = Archive::parse(&view)?;
    let wanted = DebugId::from_str(debug_id).ok();
    let mut converter = SymCacheConverter::new();
    let mut found = false;
    for object in archive.objects() {
        let object = object?;
        if wanted.is_none() || Some(object.debug_id()) == wanted {
            converter.process_object(&object)?;
            found = true;
            break;
        }
    }
    if !found {
        anyhow::bail!("no object with debug id {debug_id} in {path}");
    }
    let mut buf = Vec::new();
    converter.serialize(&mut std::io::Cursor::new(&mut buf))?;
    Ok(buf)
}

/// List debug ids (and architectures) contained in a debug file.
pub fn inspect_debug_file(path: &str) -> anyhow::Result<Vec<(String, String, bool)>> {
    let view = ByteView::open(path)?;
    let archive = Archive::parse(&view)?;
    let mut out = Vec::new();
    for object in archive.objects() {
        let object = object?;
        out.push((
            object.debug_id().to_string().to_lowercase(),
            object.arch().name().to_string(),
            object.has_debug_info(),
        ));
    }
    Ok(out)
}

fn demangle(name: &str, lang: Language) -> String {
    Name::new(name, NameMangling::Unknown, lang)
        .try_demangle(DemangleOptions::name_only())
        .to_string()
}

fn symbolicate_native(
    cache: &SymbolCache,
    artifacts: &[ArtifactRef],
    images: &[DebugImage],
    frame: &Frame,
    crashing: bool,
) -> Option<Vec<Frame>> {
    let addr = parse_addr(frame.instruction_addr.as_deref()?)?;
    // Find the image containing the address.
    let image = images
        .iter()
        .filter_map(|i| Some((i, parse_addr(i.image_addr.as_deref()?)?)))
        .filter(|(i, base)| addr >= *base && i.image_size.map(|s| addr < base + s).unwrap_or(true))
        .max_by_key(|(_, base)| *base)
        .or_else(|| {
            let base = parse_addr(frame.image_addr.as_deref()?)?;
            images.iter().find(|i| i.image_addr.as_deref().and_then(parse_addr) == Some(base)).map(|i| (i, base))
        })?;
    let debug_id = image.0.debug_id.as_deref()?;
    let artifact = artifacts
        .iter()
        .find(|a| a.kind == "native" && a.debug_id == debug_id)?;
    let sc_owned = cache.symcache(artifact)?;
    let sc = sc_owned.borrow_dependent();
    // Return addresses point after the call; look up the call instruction instead.
    let rel = (addr - image.1).saturating_sub(if crashing { 0 } else { 1 });
    let mut out = Vec::new();
    for loc in sc.lookup(rel) {
        let func = loc.function();
        let name = demangle(func.name(), func.language());
        let file = loc.file().map(|f| f.full_path());
        out.push(Frame {
            function: Some(name),
            filename: file.as_ref().map(|f| f.rsplit('/').next().unwrap_or(f).to_string()),
            abs_path: file,
            lineno: if loc.line() > 0 { Some(loc.line()) } else { None },
            instruction_addr: frame.instruction_addr.clone(),
            package: image.0.code_file.clone().or_else(|| frame.package.clone()),
            in_app: frame.in_app,
            symbolicated: Some(true),
            ..Default::default()
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

fn remap_jvm(mapper: &ProguardMapper<'_>, frame: &Frame) -> Option<Vec<Frame>> {
    let class = frame.module.as_deref()?;
    let method = frame.function.as_deref().unwrap_or("");
    let line = frame.lineno.unwrap_or(0) as usize;
    let sf = StackFrame::new(class, method, line);
    let mut out = Vec::new();
    for r in mapper.remap_frame(&sf) {
        out.push(Frame {
            function: Some(r.method().to_string()),
            module: Some(r.class().to_string()),
            filename: r.file().map(|f| f.to_string()).or_else(|| frame.filename.clone()),
            lineno: r.line().map(|l| l as u32).filter(|l| *l > 0),
            in_app: frame.in_app,
            symbolicated: Some(true),
            ..Default::default()
        });
    }
    if out.is_empty() {
        // Class-only remap (method unknown).
        let orig = mapper.remap_class(class)?;
        let mut f = frame.clone();
        f.module = Some(orig.to_string());
        f.symbolicated = Some(true);
        out.push(f);
    }
    Some(out)
}

fn symbolicate_js(cache: &SymbolCache, artifacts: &[ArtifactRef], frame: &Frame) -> Option<(Frame, Option<String>)> {
    let key = sourcemap_key(frame.abs_path.as_deref()?);
    let artifact = artifacts
        .iter()
        .filter(|a| a.kind == "sourcemap")
        .find(|a| sourcemap_key(&a.name) == key)
        .or_else(|| {
            // Fall back to matching on the file name only.
            let base = key.rsplit('/').next()?;
            artifacts.iter().filter(|a| a.kind == "sourcemap").find(|a| sourcemap_key(&a.name).rsplit('/').next() == Some(base))
        })?;
    let sm = cache.sourcemap(artifact)?;
    let line = frame.lineno?.checked_sub(1)?;
    let col = frame.colno.unwrap_or(1).saturating_sub(1);
    let token = sm.lookup_token(line, col)?;
    let source = token.get_source().map(|s| s.to_string());
    let src_line = token.get_src_line();
    let (context_line, pre, post) = sm
        .get_source_contents(token.get_src_id())
        .map(|contents| {
            let lines: Vec<&str> = contents.lines().collect();
            let i = src_line as usize;
            let get = |a: usize, b: usize| lines.get(a..b.min(lines.len())).unwrap_or(&[]).iter().map(|s| crate::util::truncate(s, 300)).collect::<Vec<_>>();
            (
                lines.get(i).map(|s| crate::util::truncate(s, 300)),
                get(i.saturating_sub(5), i),
                get(i + 1, i + 6),
            )
        })
        .unwrap_or((None, vec![], vec![]));
    let in_app = source.as_deref().map(|s| !s.contains("node_modules") && !s.contains("webpack/runtime")).or(frame.in_app);
    let name = token.get_name().map(|n| n.to_string());
    Some((Frame {
        function: frame.function.clone(),
        filename: source.as_ref().map(|s| s.trim_start_matches("webpack://").trim_start_matches("../").to_string()),
        abs_path: frame.abs_path.clone(),
        lineno: Some(src_line + 1),
        colno: Some(token.get_src_col() + 1),
        in_app,
        context_line,
        pre_context: pre,
        post_context: post,
        symbolicated: Some(true),
        ..Default::default()
    }, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sourcemap_keys_match() {
        assert_eq!(sourcemap_key("https://example.com/assets/app-1a2b.js"), "/assets/app-1a2b.js");
        assert_eq!(sourcemap_key("~/assets/app-1a2b.js.map"), "/assets/app-1a2b.js");
        assert_eq!(sourcemap_key("assets/app-1a2b.js"), "/assets/app-1a2b.js");
    }

    #[test]
    fn code_id_to_debug_id() {
        let d = debug_id_from_code_id("b5381a457906d279073822a5ceb24c4bfef94ddb").unwrap();
        assert_eq!(d.to_string().to_lowercase(), "451a38b5-0679-79d2-0738-22a5ceb24c4b");
    }

    #[test]
    fn js_symbolication_with_inline_map() {
        // Map for: line 1 of min.js maps to src/app.ts line 3, name "boom".
        let map = r#"{"version":3,"file":"min.js","sources":["src/app.ts"],"sourcesContent":["a\nb\nfunction boom() { throw new Error() }\nd"],"names":["boom"],"mappings":"AAEAA"}"#;
        let dir = std::env::temp_dir().join(format!("vaka-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("min.js.map");
        std::fs::write(&path, map).unwrap();
        let cache = SymbolCache::default();
        let artifacts = vec![ArtifactRef { id: "a".into(), kind: "sourcemap".into(), name: "/static/min.js".into(), debug_id: String::new(), path: path.to_string_lossy().into() }];
        let frame = Frame { function: Some("t".into()), abs_path: Some("https://x.io/static/min.js".into()), lineno: Some(1), colno: Some(1), ..Default::default() };
        let (f, name) = symbolicate_js(&cache, &artifacts, &frame).unwrap();
        assert_eq!(f.filename.as_deref(), Some("src/app.ts"));
        assert_eq!(f.lineno, Some(3));
        assert_eq!(name.as_deref(), Some("boom"));
        assert_eq!(f.context_line.as_deref(), Some("function boom() { throw new Error() }"));
    }

    #[test]
    fn lru_is_bounded_by_bytes() {
        let mut l: Lru<u8> = Lru::new(100);
        l.insert("a".into(), Arc::new(1), 40);
        l.insert("b".into(), Arc::new(2), 40);
        assert!(l.get("a").is_some()); // a is now newer than b
        l.insert("c".into(), Arc::new(3), 40);
        assert!(l.get("b").is_none());
        assert!(l.get("a").is_some() && l.get("c").is_some());
        assert!(l.bytes <= 100);
        l.insert("huge".into(), Arc::new(4), 1000);
        assert!(l.get("huge").is_none());
    }

    #[test]
    fn proguard_remap() {
        let mapping = "com.example.MainActivity -> a.a:\n    void onCreate(android.os.Bundle) -> a\n    1:1:void crash():42:42 -> b\n";
        let mapper = ProguardMapper::new(ProguardMapping::new(mapping.as_bytes()));
        let frame = Frame {
            function: Some("b".into()),
            module: Some("a.a".into()),
            lineno: Some(1),
            ..Default::default()
        };
        let out = remap_jvm(&mapper, &frame).unwrap();
        assert_eq!(out[0].module.as_deref(), Some("com.example.MainActivity"));
        assert_eq!(out[0].function.as_deref(), Some("crash"));
        assert_eq!(out[0].lineno, Some(42));
    }
}
