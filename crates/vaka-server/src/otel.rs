//! OpenTelemetry ingestion (OTLP/HTTP traces and logs) and log queries.

use crate::state::AppState;
use serde_json::Value;

/// Logs that belong to a trace (filled in by OTLP log ingestion).
pub async fn logs_for_trace(_state: &AppState, _project_id: &str, _trace_id: &str, _t0: i64) -> anyhow::Result<Vec<Value>> {
    Ok(Vec::new())
}
