#![recursion_limit = "512"]
mod admin;
mod alerts;
mod api;
mod auth;
mod ch;
mod config;
mod db;
mod docs;
mod grouping;
mod insights;
mod ingest;
mod maintenance;
mod mcp;
mod otp;
mod ownership;
mod models;
mod openapi;
mod processor;
mod product;
mod projects;
mod query;
mod s3;
mod scrub;
mod state;
mod symbolicate;
mod util;

use axum::http::{HeaderValue, Method};
use axum::routing::get;
use axum::{Json, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::decompression::RequestDecompressionLayer;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("VAKA_LOG").unwrap_or_else(|_| "info,tower_http=warn,sqlx=warn".into()))
        .init();
    let config = config::Config::from_env()?;
    std::fs::create_dir_all(config.artifact_dir())?;

    let db = db::connect(&config.db_url).await?;
    let ch = ch::ClickHouse::new(&config.clickhouse_url, &config.clickhouse_user, &config.clickhouse_password, &config.clickhouse_database);
    // ClickHouse may still be starting (docker-compose); retry for a while.
    let mut attempt = 0;
    loop {
        match ch.migrate().await {
            Ok(()) => break,
            Err(e) if attempt < 30 => {
                attempt += 1;
                tracing::warn!("waiting for ClickHouse ({e:#})");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e),
        }
    }

    let geoip = config.geoip_db.as_ref().and_then(|p| match maxminddb::Reader::open_readfile(p) {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::warn!("could not open GeoIP database {p}: {e}");
            None
        }
    });

    let (work_tx, work_rx) = tokio::sync::mpsc::channel(100_000);
    let state: state::AppState = Arc::new(state::AppStateInner {
        config: config.clone(),
        db,
        ch,
        work_tx,
        key_cache: Default::default(),
        geoip,
        symbols: Default::default(),
        http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?,
        s3: s3::S3Config::from_env().map(s3::S3::new).transpose()?,
        salt: Default::default(),
    });
    auth::bootstrap_admin(&state).await?;
    processor::spawn(state.clone(), work_rx);
    maintenance::spawn(state.clone());
    alerts::spawn_spike_detector(state.clone());
    alerts::spawn_digest_scheduler(state.clone());
    s3::spawn_startup_sync(state.clone());

    let ingest_cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::POST, Method::GET, Method::OPTIONS])
        .allow_headers(Any)
        .max_age(std::time::Duration::from_secs(86400));

    let app = Router::new()
        .merge(
            ingest::router()
                .layer(ingest_cors)
                .layer(RequestDecompressionLayer::new())
                .layer(tower_http::limit::RequestBodyLimitLayer::new(20 * 1024 * 1024)),
        )
        .merge(api::router())
        .merge(mcp::router())
        .merge(docs::router())
        .merge(admin::router())
        .route("/health", get(health))
        .layer(TraceLayer::new_for_http())
        .layer(tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    tracing::info!("Vaka listening on {} (public URL {})", config.listen, config.public_url);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn health(axum::extract::State(s): axum::extract::State<state::AppState>) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let db_ok = sqlx::query("SELECT 1").execute(&s.db).await.is_ok();
    let ch_ok = s.ch.ping().await;
    let status = if db_ok && ch_ok { axum::http::StatusCode::OK } else { axum::http::StatusCode::SERVICE_UNAVAILABLE };
    (status, Json(serde_json::json!({"ok": db_ok && ch_ok, "db": db_ok, "clickhouse": ch_ok, "version": env!("CARGO_PKG_VERSION")})))
}
