//! Minimal ClickHouse client over the HTTP interface.
//!
//! Inserts use `JSONEachRow`; selects use `FORMAT JSON` with server-side query
//! parameters (`{name:Type}`) so user input is never interpolated into SQL.

use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Clone)]
pub struct ClickHouse {
    http: reqwest::Client,
    url: String,
    user: String,
    password: String,
    pub database: String,
}

impl ClickHouse {
    pub fn new(url: &str, user: &str, password: &str, database: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("http client"),
            url: url.trim_end_matches('/').to_string(),
            user: user.to_string(),
            password: password.to_string(),
            database: database.to_string(),
        }
    }

    fn req(&self, db: bool) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .post(&self.url)
            .header("X-ClickHouse-User", &self.user)
            .header("X-ClickHouse-Key", &self.password);
        if db {
            r = r.query(&[("database", &self.database)]);
        }
        r
    }

    pub async fn execute_raw(&self, sql: &str, use_db: bool) -> anyhow::Result<String> {
        let resp = self.req(use_db).body(sql.to_string()).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("clickhouse error ({status}): {}", text.trim());
        }
        Ok(text)
    }

    pub async fn execute(&self, sql: &str) -> anyhow::Result<()> {
        self.execute_raw(sql, true).await.map(|_| ())
    }

    pub async fn execute_with(&self, sql: &str, params: &[(&str, String)]) -> anyhow::Result<()> {
        let mut r = self.req(true);
        for (k, v) in params {
            r = r.query(&[(format!("param_{k}"), v)]);
        }
        let resp = r.body(sql.to_string()).send().await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("clickhouse error ({status}): {}", resp.text().await?.trim());
        }
        Ok(())
    }

    pub async fn insert<T: serde::Serialize>(&self, table: &str, rows: &[T]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut body = String::new();
        for row in rows {
            body.push_str(&serde_json::to_string(row)?);
            body.push('\n');
        }
        let resp = self
            .req(true)
            .query(&[
                ("query", format!("INSERT INTO {table} FORMAT JSONEachRow")),
                ("date_time_input_format", "best_effort".to_string()),
                ("input_format_skip_unknown_fields", "1".to_string()),
            ])
            .body(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("clickhouse insert into {table} failed ({status}): {}", resp.text().await?.trim());
        }
        Ok(())
    }

    /// Run a SELECT and return rows as JSON objects.
    pub async fn query(&self, sql: &str, params: &[(&str, String)]) -> anyhow::Result<Vec<Value>> {
        let mut r = self.req(true).query(&[
            ("default_format", "JSON"),
            ("output_format_json_quote_64bit_integers", "0"),
        ]);
        for (k, v) in params {
            r = r.query(&[(format!("param_{k}"), v)]);
        }
        let resp = r.body(sql.to_string()).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("clickhouse query failed ({status}): {}", text.trim());
        }
        let v: Value = serde_json::from_str(&text)?;
        Ok(v.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default())
    }

    pub async fn query_as<T: DeserializeOwned>(&self, sql: &str, params: &[(&str, String)]) -> anyhow::Result<Vec<T>> {
        let rows = self.query(sql, params).await?;
        rows.into_iter().map(|r| Ok(serde_json::from_value(r)?)).collect()
    }

    pub async fn ping(&self) -> bool {
        self.execute_raw("SELECT 1", false).await.is_ok()
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        self.execute_raw(&format!("CREATE DATABASE IF NOT EXISTS {}", self.database), false).await?;
        for stmt in SCHEMA.split(";\n").map(str::trim).filter(|s| !s.is_empty()) {
            self.execute(stmt).await?;
        }
        Ok(())
    }
}

/// ClickHouse schema. Every table carries `retention_days`, set per row from the
/// project's setting at ingest time, and a TTL that drops rows when they expire.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS errors (
    event_id UUID,
    project_id LowCardinality(String),
    issue_id String,
    fingerprint String,
    timestamp DateTime64(3, 'UTC'),
    received_at DateTime64(3, 'UTC'),
    platform LowCardinality(String),
    level LowCardinality(String),
    release String,
    environment LowCardinality(String),
    exception_type String,
    exception_value String,
    message String,
    culprit String,
    handled UInt8,
    fatal UInt8,
    frames String,
    exceptions String,
    breadcrumbs String,
    tags Map(String, String),
    contexts String,
    debug_images String,
    user_id String,
    anon_id String,
    session_id String,
    device_model LowCardinality(String),
    os_name LowCardinality(String),
    os_version LowCardinality(String),
    browser LowCardinality(String),
    country LowCardinality(String),
    url String,
    sdk_name LowCardinality(String),
    sdk_version LowCardinality(String),
    symbolicated UInt8,
    retention_days UInt16
) ENGINE = MergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (project_id, issue_id, timestamp)
TTL toDateTime(timestamp) + toIntervalDay(retention_days);

CREATE TABLE IF NOT EXISTS sessions (
    session_id String,
    project_id LowCardinality(String),
    distinct_id String,
    user_id String,
    release String,
    environment LowCardinality(String),
    platform LowCardinality(String),
    status LowCardinality(String),
    started DateTime64(3, 'UTC'),
    timestamp DateTime64(3, 'UTC'),
    duration_ms UInt64,
    error_count UInt32,
    retention_days UInt16
) ENGINE = ReplacingMergeTree(timestamp)
PARTITION BY toYYYYMM(started)
ORDER BY (project_id, session_id)
TTL toDateTime(started) + toIntervalDay(retention_days);

CREATE TABLE IF NOT EXISTS events (
    event_id UUID,
    project_id LowCardinality(String),
    timestamp DateTime64(3, 'UTC'),
    type LowCardinality(String),
    name String,
    url String,
    hostname String,
    path String,
    referrer String,
    referrer_host String,
    utm_source String,
    utm_medium String,
    utm_campaign String,
    utm_term String,
    utm_content String,
    country LowCardinality(String),
    device_type LowCardinality(String),
    browser LowCardinality(String),
    browser_version LowCardinality(String),
    os_name LowCardinality(String),
    os_version LowCardinality(String),
    device_model LowCardinality(String),
    app_version LowCardinality(String),
    release String,
    environment LowCardinality(String),
    platform LowCardinality(String),
    visitor_id String,
    user_id String,
    session_id String,
    props Map(String, String),
    retention_days UInt16
) ENGINE = MergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (project_id, type, timestamp)
TTL toDateTime(timestamp) + toIntervalDay(retention_days);
"#;
