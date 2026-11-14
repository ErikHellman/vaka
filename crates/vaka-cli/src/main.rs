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
    /// Release metadata
    Releases {
        #[command(subcommand)]
        what: Releases,
    },
    /// Code ownership rules (who owns which issues)
    Ownership {
        #[command(subcommand)]
        what: Ownership,
    },
    /// Code mappings: how stack-trace paths map to repository paths (for source links)
    CodeMappings {
        #[command(subcommand)]
        what: CodeMappings,
    },
    /// Heartbeat monitors for cron jobs (check-ins use the ingest key)
    Monitor {
        #[command(subcommand)]
        what: Monitor,
    },
    /// Send a test error with an ingest key to verify an installation
    SendTestEvent {
        /// Ingest key (vk_...)
        #[arg(long, env = "VAKA_KEY")]
        key: String,
    },
}

#[derive(Subcommand)]
enum Monitor {
    /// Run a command as a monitored run: in_progress, then ok or error from its exit code
    ///
    /// Example: vaka monitor run nightly-backup --cron "0 3 * * *" -- ./backup.sh
    Run {
        slug: String,
        #[command(flatten)]
        check_in: CheckInArgs,
        /// The command and its arguments
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Send one check-in (ok by default)
    CheckIn {
        slug: String,
        /// ok, error or in_progress
        #[arg(long, default_value = "ok")]
        status: String,
        /// Pair an in_progress check-in with its ok or error
        #[arg(long)]
        id: Option<String>,
        /// Error text for an error check-in
        #[arg(long)]
        message: Option<String>,
        #[command(flatten)]
        check_in: CheckInArgs,
    },
}

#[derive(clap::Args)]
struct CheckInArgs {
    /// Ingest key (vk_...)
    #[arg(long, env = "VAKA_KEY", hide_env_values = true)]
    key: String,
    /// Create the monitor on first check-in with this interval, e.g. 15m or 1h
    #[arg(long, conflicts_with = "cron")]
    every: Option<String>,
    /// Create the monitor on first check-in with this cron schedule, e.g. "0 3 * * *"
    #[arg(long)]
    cron: Option<String>,
    /// IANA time zone for --cron
    #[arg(long, requires = "cron")]
    timezone: Option<String>,
    /// Seconds a check-in may be late
    #[arg(long)]
    grace: Option<u64>,
    /// Seconds a run may take before it counts as failed (0 = no limit)
    #[arg(long)]
    max_runtime: Option<u64>,
    #[arg(long, env = "VAKA_RELEASE")]
    release: Option<String>,
    #[arg(long, env = "VAKA_ENVIRONMENT")]
    environment: Option<String>,
}

impl CheckInArgs {
    fn monitor(&self) -> Option<Value> {
        if self.every.is_none() && self.cron.is_none() {
            return None;
        }
        let mut schedule = json!({});
        if let Some(e) = &self.every {
            schedule["every"] = json!(e);
        }
        if let Some(c) = &self.cron {
            schedule["cron"] = json!(c);
        }
        if let Some(tz) = &self.timezone {
            schedule["timezone"] = json!(tz);
        }
        let mut m = json!({"schedule": schedule});
        if let Some(g) = self.grace {
            m["grace_s"] = json!(g);
        }
        if let Some(r) = self.max_runtime {
            m["max_runtime_s"] = json!(r);
        }
        Some(m)
    }
}

#[derive(Subcommand)]
enum Releases {
    /// Record the commits in a release from local git history (powers suspect commits on issues)
    SetCommits {
        /// Release, must match the `release` the SDK reports
        #[arg(long)]
        release: String,
        /// Git revision range, e.g. v1.2.0..HEAD. Default: previous tag..HEAD, or the last 100 commits
        #[arg(long)]
        range: Option<String>,
        /// Repository web URL for commit links. Default: derived from `git remote get-url origin`
        #[arg(long)]
        repo_url: Option<String>,
        /// Git working tree
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// List commits recorded for a release
    Commits {
        #[arg(long)]
        release: String,
    },
}

#[derive(Subcommand)]
enum Ownership {
    /// Print the project's ownership rules
    Show,
    /// Replace the project's ownership rules with the contents of a file
    Set { file: PathBuf },
}

#[derive(Subcommand)]
enum CodeMappings {
    /// Print the project's code mappings
    Show,
    /// Replace the code mappings with a JSON file: [{"stack_root", "source_root", "repository", "default_branch"}]
    Set { file: PathBuf },
    /// Replace the code mappings with a single mapping
    SetOne {
        /// Prefix of stack-trace paths, e.g. "build/src/" ("" matches every path)
        #[arg(long, default_value = "")]
        stack_root: String,
        /// Repository path that replaces it, e.g. "packages/web/src/"
        #[arg(long, default_value = "")]
        source_root: String,
        /// Repository web URL, e.g. https://github.com/acme/shop
        #[arg(long)]
        repository: String,
        #[arg(long, default_value = "main")]
        default_branch: String,
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
    // Exit quietly when piped into `head` and friends instead of panicking on EPIPE.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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
        Cmd::Releases { what } => releases(ctx, what),
        Cmd::Ownership { what } => match what {
            Ownership::Show => {
                let v = ctx.get(&format!("/api/v1/projects/{}/ownership", ctx.project()?))?;
                if ctx.json {
                    return print_json(&v);
                }
                print!("{}", v["rules"].as_str().unwrap_or(""));
                Ok(())
            }
            Ownership::Set { file } => {
                let rules = std::fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
                let v = ctx.send_json(reqwest::Method::PUT, &format!("/api/v1/projects/{}/ownership", ctx.project()?), json!({"rules": rules}))?;
                println!("{} ownership rules saved", v["rules"]);
                Ok(())
            }
        },
        Cmd::CodeMappings { what } => {
            let path = format!("/api/v1/projects/{}/code-mappings", ctx.project()?);
            let v = match what {
                CodeMappings::Show => ctx.get(&path)?,
                CodeMappings::Set { file } => {
                    let text = std::fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
                    let mappings: Value = serde_json::from_str(&text).context("the file must be a JSON array of mappings")?;
                    ctx.send_json(reqwest::Method::PUT, &path, json!({"mappings": mappings}))?
                }
                CodeMappings::SetOne { stack_root, source_root, repository, default_branch } => ctx.send_json(
                    reqwest::Method::PUT,
                    &path,
                    json!({"mappings": [{"stack_root": stack_root, "source_root": source_root, "repository": repository, "default_branch": default_branch}]}),
                )?,
            };
            if ctx.json {
                return print_json(&v);
            }
            for m in v["mappings"].as_array().into_iter().flatten() {
                println!(
                    "{:<30} -> {:<30} {} ({})",
                    m["stack_root"].as_str().filter(|s| !s.is_empty()).unwrap_or("(any)"),
                    m["source_root"].as_str().unwrap_or(""),
                    m["repository"].as_str().unwrap_or(""),
                    m["default_branch"].as_str().unwrap_or("")
                );
            }
            Ok(())
        }
        Cmd::Monitor { what } => monitor(ctx, what),
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

fn send_check_in(ctx: &Ctx, slug: &str, a: &CheckInArgs, mut body: Value, monitor: bool) -> Result<Value> {
    if let Some(r) = &a.release {
        body["release"] = json!(r);
    }
    if let Some(e) = &a.environment {
        body["environment"] = json!(e);
    }
    if monitor {
        if let Some(m) = a.monitor() {
            body["monitor"] = m;
        }
    }
    let slug: String = slug.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let r = ctx
        .http
        .post(format!("{}/ingest/v1/monitors/{slug}/check-ins", ctx.url))
        .header("x-vaka-key", &a.key)
        .timeout(Duration::from_secs(15))
        .json(&body)
        .send()?;
    parse(r)
}

fn monitor(ctx: &Ctx, what: Monitor) -> Result<()> {
    match what {
        Monitor::CheckIn { slug, status, id, message, check_in } => {
            let mut body = json!({"status": status});
            if let Some(id) = id {
                body["check_in_id"] = json!(id);
            }
            if let Some(m) = message {
                body["message"] = json!(m);
            }
            let v = send_check_in(ctx, &slug, &check_in, body, true)?;
            if ctx.json {
                println!("{v}");
            } else {
                println!("checked in {slug}: {}", v["state"].as_str().unwrap_or("ok"));
            }
            Ok(())
        }
        Monitor::Run { slug, check_in, command } => {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let id = format!("{:x}{:x}", nanos, std::process::id());
            // A failed check-in never stops the job: warn and run it anyway.
            let warn = |r: Result<Value>| {
                if let Err(e) = r {
                    eprintln!("vaka: check-in for {slug} failed: {e:#}");
                }
            };
            warn(send_check_in(ctx, &slug, &check_in, json!({"status": "in_progress", "check_in_id": id}), true));
            let started = std::time::Instant::now();
            let result = std::process::Command::new(&command[0]).args(&command[1..]).status();
            let ms = started.elapsed().as_millis() as u64;
            let (code, body) = match &result {
                Ok(st) if st.success() => (0, json!({"status": "ok", "check_in_id": id, "duration_ms": ms})),
                Ok(st) => {
                    let code = st.code().unwrap_or(1);
                    let msg = match st.code() {
                        Some(c) => format!("{} exited with code {c}", command[0]),
                        None => format!("{} was killed by a signal", command[0]),
                    };
                    (code, json!({"status": "error", "check_in_id": id, "duration_ms": ms, "message": msg}))
                }
                Err(e) => (127, json!({"status": "error", "check_in_id": id, "duration_ms": ms, "message": format!("cannot run {}: {e}", command[0])})),
            };
            warn(send_check_in(ctx, &slug, &check_in, body, false));
            if let Err(e) = result {
                eprintln!("error: cannot run {}: {e}", command[0]);
            }
            std::process::exit(code);
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

fn releases(ctx: &Ctx, what: Releases) -> Result<()> {
    match what {
        Releases::SetCommits { release, range, repo_url, repo } => {
            let git = |args: &[&str]| -> Result<String> {
                let out = std::process::Command::new("git").arg("-C").arg(&repo).args(args).output().context("cannot run git")?;
                if !out.status.success() {
                    bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
                }
                Ok(String::from_utf8_lossy(&out.stdout).to_string())
            };
            let range = match range {
                Some(r) => vec![r],
                None => match git(&["describe", "--tags", "--abbrev=0", "HEAD^"]) {
                    Ok(tag) if !tag.trim().is_empty() => vec![format!("{}..HEAD", tag.trim())],
                    _ => vec!["-n".into(), "100".into(), "HEAD".into()],
                },
            };
            let mut args: Vec<&str> = vec!["log", "--no-merges", "--name-only", "--format=%x1e%H%x1f%an%x1f%ae%x1f%ct%x1f%B%x1f"];
            args.extend(range.iter().map(|s| s.as_str()));
            let log = git(&args)?;
            let commits = parse_git_log(&log);
            if commits.is_empty() {
                bail!("no commits in {}", range.join(" "));
            }
            let repo_url = repo_url.or_else(|| git(&["remote", "get-url", "origin"]).ok().map(|u| web_url(u.trim())));
            let tip = range.last().and_then(|r| r.split_once("..").map(|(_, b)| b.trim_start_matches('.').to_string())).filter(|t| !t.is_empty()).unwrap_or_else(|| "HEAD".into());
            let head = git(&["rev-parse", &tip]).ok().map(|h| h.trim().to_string());
            let n = commits.len();
            let v = ctx.send_json(
                reqwest::Method::PUT,
                &format!("/api/v1/projects/{}/releases/{}/commits", ctx.project()?, encode(&release)),
                json!({"repository": repo_url, "head_commit": head, "commits": commits}),
            )?;
            if ctx.json {
                return print_json(&v);
            }
            println!("{} commits recorded for release {release} ({n} read from {})", v["commits"], range.join(" "));
            let resolved: Vec<&str> = v["resolved_issues"].as_array().into_iter().flatten().filter_map(|i| i.as_str()).collect();
            if !resolved.is_empty() {
                println!("resolved by commit messages: {}", resolved.join(", "));
            }
            Ok(())
        }
        Releases::Commits { release } => {
            let v = ctx.get(&format!("/api/v1/projects/{}/releases/{}/commits", ctx.project()?, encode(&release)))?;
            if ctx.json {
                return print_json(&v);
            }
            for c in v["commits"].as_array().into_iter().flatten() {
                let sha = c["sha"].as_str().unwrap_or("");
                println!("{} {:<20} {}", &sha[..sha.len().min(10)], c["author_name"].as_str().unwrap_or(""), c["message"].as_str().unwrap_or("").lines().next().unwrap_or(""));
            }
            Ok(())
        }
    }
}

/// Parse `git log --name-only` output in the format used by `set-commits`.
fn parse_git_log(log: &str) -> Vec<Value> {
    log.split('\x1e')
        .filter(|c| !c.trim().is_empty())
        .filter_map(|c| {
            let f: Vec<&str> = c.splitn(6, '\x1f').collect();
            if f.len() < 6 {
                return None;
            }
            let files: Vec<&str> = f[5].lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            Some(json!({
                "sha": f[0].trim(), "author_name": f[1], "author_email": f[2],
                "timestamp": f[3].trim().parse::<i64>().unwrap_or(0), "message": f[4].trim(), "files": files,
            }))
        })
        .collect()
}

/// git@github.com:o/r.git or https://user@github.com/o/r.git -> https://github.com/o/r
fn web_url(remote: &str) -> String {
    let r = remote.trim_end_matches(".git");
    if let Some(rest) = r.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{path}");
        }
    }
    if let Some(rest) = r.strip_prefix("ssh://git@") {
        return format!("https://{rest}");
    }
    if let Some(rest) = r.strip_prefix("https://") {
        return format!("https://{}", rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest));
    }
    r.to_string()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_log_parsing() {
        let log = "\x1eabc\x1fAda\x1fada@x.se\x1f1700000000\x1fFix cart\n\nBody\n\x1f\n\nsrc/cart.ts\nsrc/a.ts\n\x1edef\x1fBo\x1fbo@x.se\x1f1700000001\x1fDocs\n\x1f\n\nREADME.md\n";
        let c = parse_git_log(log);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["files"], json!(["src/cart.ts", "src/a.ts"]));
        assert_eq!(c[0]["message"], "Fix cart\n\nBody");
        assert_eq!(c[1]["sha"], "def");
        assert_eq!(web_url("git@github.com:o/r.git"), "https://github.com/o/r");
        assert_eq!(web_url("https://tok@github.com/o/r.git"), "https://github.com/o/r");
        assert_eq!(encode("app@1.0+2"), "app%401.0%2B2");
    }
}
