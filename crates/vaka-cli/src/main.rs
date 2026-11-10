//! `vaka`: command-line tool for Vaka.
//!
//! Configuration comes from flags or environment variables:
//! VAKA_URL (server), VAKA_TOKEN (API token, write scope for uploads), VAKA_PROJECT (slug).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use reqwest::blocking::{multipart, Client};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "vaka", version, about = "Vaka CLI: upload debug files and query crashes")]
struct Cli {
    /// Vaka server URL
    #[arg(long, env = "VAKA_URL", global = true, default_value = "http://localhost:8080")]
    url: String,
    /// API token (vt_...)
    #[arg(long, env = "VAKA_TOKEN", global = true, hide_env_values = true)]
    token: Option<String>,
    /// Project slug or id
    #[arg(long, short, env = "VAKA_PROJECT", global = true)]
    project: Option<String>,
    /// Print raw JSON responses
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Upload debug files
    Upload {
        #[command(subcommand)]
        what: Upload,
    },
    /// List projects
    Projects,
    /// List open issues
    Issues {
        #[arg(long, default_value = "open")]
        status: String,
        #[arg(long, default_value = "24h")]
        since: String,
        #[arg(long, default_value = "events")]
        sort: String,
    },
    /// Show an issue (id or short id like WEB-12)
    Issue { id: String },
    /// Resolve an issue
    Resolve {
        id: String,
        /// Release that contains the fix
        #[arg(long)]
        release: Option<String>,
    },
    /// Release health (crash-free sessions and users)
    Health {
        #[arg(long)]
        release: Option<String>,
    },
    /// Send a test error with an ingest key to verify an installation
    SendTestEvent {
        /// Ingest key (vk_...)
        #[arg(long, env = "VAKA_KEY")]
        key: String,
    },
}

#[derive(Subcommand)]
enum Upload {
    /// Upload JavaScript source maps (*.map) from a build directory
    Sourcemaps {
        /// Release, must match the `release` the SDK reports
        #[arg(long)]
        release: String,
        /// URL path prefix where the files are served, e.g. "/assets" or "~/static/js"
        #[arg(long, default_value = "/")]
        url_prefix: String,
        /// Build output directory (e.g. dist)
        dir: PathBuf,
    },
    /// Upload an Android R8/ProGuard mapping.txt
    Proguard {
        #[arg(long)]
        release: String,
        mapping: PathBuf,
    },
    /// Upload iOS/macOS dSYM bundles (or a folder containing them)
    Dsym {
        #[arg(long, default_value = "")]
        release: String,
        paths: Vec<PathBuf>,
    },
    /// Upload native debug files: ELF binaries with debug info, Mach-O, PDB
    DebugFiles {
        #[arg(long, default_value = "")]
        release: String,
        paths: Vec<PathBuf>,
    },
}

struct Ctx {
    url: String,
    token: Option<String>,
    project: Option<String>,
    json: bool,
    http: Client,
}

impl Ctx {
    fn token(&self) -> Result<&str> {
        self.token.as_deref().context("an API token is required: pass --token or set VAKA_TOKEN")
    }
    fn project(&self) -> Result<&str> {
        self.project.as_deref().context("a project is required: pass --project or set VAKA_PROJECT")
    }
    fn get(&self, path: &str) -> Result<Value> {
        let r = self.http.get(format!("{}{path}", self.url)).bearer_auth(self.token()?).send()?;
        parse(r)
    }
    fn send_json(&self, method: reqwest::Method, path: &str, body: Value) -> Result<Value> {
        let r = self.http.request(method, format!("{}{path}", self.url)).bearer_auth(self.token()?).json(&body).send()?;
        parse(r)
    }
    fn upload(&self, kind: &str, release: &str, name: &str, file: &Path) -> Result<Value> {
        let form = multipart::Form::new()
            .text("kind", kind.to_string())
            .text("release", release.to_string())
            .text("name", name.to_string())
            .file("file", file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        let r = self
            .http
            .post(format!("{}/api/v1/projects/{}/artifacts", self.url, self.project()?))
            .bearer_auth(self.token()?)
            .multipart(form)
            .send()?;
        parse(r)
    }
}

fn parse(r: reqwest::blocking::Response) -> Result<Value> {
    let status = r.status();
    let text = r.text()?;
    let v: Value = serde_json::from_str(&text).unwrap_or(json!({"raw": text}));
    if !status.is_success() {
        bail!("{status}: {}", v["error"].as_str().unwrap_or(&text));
    }
    Ok(v)
}

fn main() {
    let cli = Cli::parse();
    let ctx = Ctx {
        url: cli.url.trim_end_matches('/').to_string(),
        token: cli.token.clone(),
        project: cli.project.clone(),
        json: cli.json,
        http: Client::builder().timeout(Duration::from_secs(600)).build().expect("http client"),
    };
    if let Err(e) = run(&ctx, cli.cmd) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(ctx: &Ctx, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Upload { what } => upload(ctx, what),
        Cmd::Projects => {
            let v = ctx.get("/api/v1/projects")?;
            if ctx.json {
                return print_json(&v);
            }
            for p in v["projects"].as_array().into_iter().flatten() {
                println!("{:<24} {:<12} {}", p["slug"].as_str().unwrap_or(""), p["platform"].as_str().unwrap_or(""), p["name"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        Cmd::Issues { status, since, sort } => {
            let v = ctx.get(&format!("/api/v1/projects/{}/issues?status={status}&since={since}&sort={sort}", ctx.project()?))?;
            if ctx.json {
                return print_json(&v);
            }
            for i in v["issues"].as_array().into_iter().flatten() {
                println!(
                    "{:<14} {:>6} ev {:>5} users  {}\n{:<14} {}",
                    i["short_id"].as_str().unwrap_or(""),
                    i["events_in_window"],
                    i["users_in_window"],
                    i["title"].as_str().unwrap_or(""),
                    "",
                    i["culprit"].as_str().unwrap_or("")
                );
            }
            Ok(())
        }
        Cmd::Issue { id } => {
            let v = ctx.get(&format!("/api/v1/issues/{id}"))?;
            if ctx.json {
                return print_json(&v);
            }
            println!("{} {}  [{}]", v["short_id"].as_str().unwrap_or(""), v["title"].as_str().unwrap_or(""), v["status"].as_str().unwrap_or(""));
            println!("seen {} times, last in {} at {}", v["times_seen"], v["last_release"].as_str().unwrap_or("-"), v["last_seen"].as_str().unwrap_or(""));
            for exc in v["latest_event"]["exceptions"].as_array().into_iter().flatten() {
                println!("\n{}: {}", exc["type"].as_str().unwrap_or(""), exc["value"].as_str().unwrap_or(""));
                for f in exc["stacktrace"].as_array().into_iter().flatten() {
                    let loc = f["filename"].as_str().or(f["module"].as_str()).or(f["abs_path"].as_str()).unwrap_or("");
                    let line = f["lineno"].as_u64().map(|l| format!(":{l}")).unwrap_or_default();
                    let marker = if f["in_app"].as_bool().unwrap_or(true) { "*" } else { " " };
                    println!("  {marker} {} ({loc}{line})", f["function"].as_str().unwrap_or("?"));
                }
            }
            Ok(())
        }
        Cmd::Resolve { id, release } => {
            let v = ctx.send_json(reqwest::Method::PATCH, &format!("/api/v1/issues/{id}"), json!({"status": "resolved", "resolved_in_release": release}))?;
            println!("{} is now {}", v["short_id"].as_str().unwrap_or(&id), v["status"].as_str().unwrap_or(""));
            Ok(())
        }
        Cmd::Health { release } => {
            let q = release.map(|r| format!("?release={r}")).unwrap_or_default();
            let v = ctx.get(&format!("/api/v1/projects/{}/releases/health{q}", ctx.project()?))?;
            if ctx.json {
                return print_json(&v);
            }
            println!("{:<20} {:>9} {:>9} {:>12} {:>12}", "release", "sessions", "users", "crash-free s", "crash-free u");
            for r in v["releases"].as_array().into_iter().flatten() {
                println!(
                    "{:<20} {:>9} {:>9} {:>11.2}% {:>11.2}%",
                    r["release"].as_str().unwrap_or(""),
                    r["sessions"],
                    r["users"],
                    r["crash_free_sessions"].as_f64().unwrap_or(0.0) * 100.0,
                    r["crash_free_users"].as_f64().unwrap_or(0.0) * 100.0
                );
            }
            Ok(())
        }
        Cmd::SendTestEvent { key } => {
            let body = json!({"sdk": {"name": "vaka-cli", "version": env!("CARGO_PKG_VERSION")}, "items": [{
                "type": "error", "level": "error", "platform": "other", "release": "vaka-cli-test",
                "exception": {"type": "VakaTestError", "value": "This is a test event sent by `vaka send-test-event`", "stacktrace": [{"function": "send_test_event", "filename": "vaka-cli", "in_app": true}]}
            }]});
            let r = ctx.http.post(format!("{}/ingest/v1/batch", ctx.url)).header("x-vaka-key", key).json(&body).send()?;
            let v = parse(r)?;
            println!("accepted: {}. Look for a `VakaTestError` issue in the project.", v["accepted"]);
            Ok(())
        }
    }
}

fn print_json(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn report(ctx: &Ctx, file: &Path, v: &Value) {
    if ctx.json {
        println!("{v}");
    } else {
        let ids: Vec<String> = v["artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| a["debug_id"].as_str().map(|d| format!("{d} ({})", a["arch"].as_str().unwrap_or(""))).unwrap_or_else(|| a["name"].as_str().unwrap_or("").to_string()))
            .collect();
        println!("uploaded {} -> {}", file.display(), ids.join(", "));
    }
}

fn upload(ctx: &Ctx, what: Upload) -> Result<()> {
    match what {
        Upload::Sourcemaps { release, url_prefix, dir } => {
            let prefix = url_prefix.trim_start_matches('~').trim_end_matches('/').to_string();
            let mut n = 0;
            for entry in walkdir::WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("map") {
                    continue;
                }
                let rel = path.strip_prefix(&dir).unwrap_or(path).to_string_lossy().replace('\\', "/");
                // The source map is matched to the minified file's URL path.
                let minified = rel.strip_suffix(".map").unwrap_or(&rel);
                let name = format!("{prefix}/{minified}");
                let v = ctx.upload("sourcemap", &release, &name, path)?;
                report(ctx, path, &v);
                n += 1;
            }
            if n == 0 {
                bail!("no .map files found in {} (enable source maps in your bundler)", dir.display());
            }
            println!("{n} source maps uploaded for release {release}");
            Ok(())
        }
        Upload::Proguard { release, mapping } => {
            let v = ctx.upload("proguard", &release, "mapping.txt", &mapping)?;
            report(ctx, &mapping, &v);
            Ok(())
        }
        Upload::Dsym { release, paths } => {
            let files = find_dsym_files(&paths)?;
            if files.is_empty() {
                bail!("no dSYM DWARF files found (looked for *.dSYM/Contents/Resources/DWARF/*)");
            }
            for f in files {
                let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let v = ctx.upload("native", &release, &name, &f)?;
                report(ctx, &f, &v);
            }
            Ok(())
        }
        Upload::DebugFiles { release, paths } => {
            if paths.is_empty() {
                bail!("pass one or more debug files");
            }
            for f in paths {
                let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let v = ctx.upload("native", &release, &name, &f)?;
                report(ctx, &f, &v);
            }
            Ok(())
        }
    }
}

fn find_dsym_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_file() {
            out.push(p.clone());
            continue;
        }
        for entry in walkdir::WalkDir::new(p).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            let s = path.to_string_lossy();
            if path.is_file() && s.contains(".dSYM/Contents/Resources/DWARF/") {
                out.push(path.to_path_buf());
            }
        }
    }
    Ok(out)
}
