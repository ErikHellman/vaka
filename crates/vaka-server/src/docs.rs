//! Agent-friendly documentation served by every instance: `/llms.txt`,
//! `/docs/*.md`, and per-platform install guides with placeholders filled in.

use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

pub const INSTALL_PLATFORMS: &[&str] = &["javascript", "android", "ios", "rust", "go", "jvm", "node", "python", "dotnet", "http"];

macro_rules! doc {
    ($name:literal) => {
        ($name, include_str!(concat!("../../../docs/", $name)))
    };
}

pub const DOCS: &[(&str, &str)] = &[
    doc!("index.md"),
    doc!("quickstart.md"),
    doc!("self-hosting.md"),
    doc!("deploy-fly.md"),
    doc!("ingest.md"),
    doc!("api.md"),
    doc!("mcp.md"),
    doc!("privacy.md"),
    doc!("alerts.md"),
    doc!("performance.md"),
    doc!("flags.md"),
    doc!("anomalies.md"),
    doc!("feedback.md"),
    doc!("opentelemetry.md"),
    doc!("cli.md"),
    doc!("sdk-javascript.md"),
    doc!("sdk-android.md"),
    doc!("sdk-ios.md"),
    doc!("sdk-rust.md"),
    doc!("sdk-go.md"),
    doc!("sdk-jvm.md"),
    doc!("sdk-node.md"),
    doc!("sdk-python.md"),
    doc!("sdk-dotnet.md"),
    doc!("sdk-http.md"),
];

const INSTALL: &[(&str, &str)] = &[
    ("javascript", include_str!("../../../docs/install/javascript.md")),
    ("android", include_str!("../../../docs/install/android.md")),
    ("ios", include_str!("../../../docs/install/ios.md")),
    ("rust", include_str!("../../../docs/install/rust.md")),
    ("go", include_str!("../../../docs/install/go.md")),
    ("jvm", include_str!("../../../docs/install/jvm.md")),
    ("node", include_str!("../../../docs/install/node.md")),
    ("python", include_str!("../../../docs/install/python.md")),
    ("dotnet", include_str!("../../../docs/install/dotnet.md")),
    ("http", include_str!("../../../docs/install/http.md")),
];

const LLMS_TXT: &str = include_str!("../../../docs/llms.txt");

pub fn install_markdown(platform: &str, endpoint: &str, key: &str, project: &str) -> Option<String> {
    let platform = match platform {
        "javascript" | "js" | "browser" | "web" => "javascript",
        "android" | "kotlin" => "android",
        "ios" | "swift" => "ios",
        "rust" => "rust",
        "go" | "golang" => "go",
        "jvm" | "java" | "scala" | "spring" => "jvm",
        "node" | "nodejs" | "node.js" | "express" | "fastify" => "node",
        "python" | "py" | "django" | "flask" | "fastapi" => "python",
        "dotnet" | ".net" | "csharp" | "c#" | "aspnetcore" | "fsharp" => "dotnet",
        _ => "http",
    };
    INSTALL
        .iter()
        .find(|(p, _)| *p == platform)
        .map(|(_, t)| fill(t, endpoint, key, project))
}

fn fill(t: &str, endpoint: &str, key: &str, project: &str) -> String {
    t.replace("{{ENDPOINT}}", endpoint).replace("{{KEY}}", key).replace("{{PROJECT}}", project)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/llms.txt", get(llms))
        .route("/docs", get(|| async { axum::response::Redirect::to("/docs/") }))
        .route("/docs/", get(|State(s): State<AppState>| async move { markdown(&s, "index.md") }))
        .route("/docs/{name}", get(doc_page))
        .route("/docs/install/{name}", get(install_page))
        .route("/sdk/vaka.js", get(|| async { js(JS_SDK) }))
        .route("/sdk/vaka-feedback.js", get(|| async { js(JS_FEEDBACK) }))
}

async fn llms(State(s): State<AppState>) -> Response {
    markdown_raw(fill(LLMS_TXT, &s.config.public_url, "<ingest key>", "<project>"))
}

fn markdown(s: &AppState, name: &str) -> Response {
    match DOCS.iter().find(|(n, _)| *n == name) {
        Some((_, body)) => markdown_raw(fill(body, &s.config.public_url, "<ingest key>", "<project>")),
        None => (StatusCode::NOT_FOUND, format!("no doc named {name}; see /docs/")).into_response(),
    }
}

fn markdown_raw(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], body).into_response()
}

async fn doc_page(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    let name = if name.ends_with(".md") { name } else { format!("{name}.md") };
    markdown(&s, &name)
}

async fn install_page(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    let platform = name.trim_end_matches(".md");
    match install_markdown(platform, &s.config.public_url, "<ingest key>", "<project>") {
        Some(md) => markdown_raw(md),
        None => (StatusCode::NOT_FOUND, "unknown platform").into_response(),
    }
}

const JS_SDK: &str = include_str!("../../../sdks/js/dist/vaka.min.js");
const JS_FEEDBACK: &str = include_str!("../../../sdks/js/dist/vaka-feedback.min.js");

fn js(body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        body,
    )
        .into_response()
}
