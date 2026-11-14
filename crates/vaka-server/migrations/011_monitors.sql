-- Health monitoring: heartbeat (cron), HTTP and TLS monitors, their incidents and secrets.
CREATE TABLE monitors (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    slug TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    enabled BIGINT NOT NULL DEFAULT 1,
    environment TEXT NOT NULL DEFAULT '',
    config TEXT NOT NULL DEFAULT '{}',
    interval_s BIGINT NOT NULL DEFAULT 60,
    timeout_ms BIGINT NOT NULL DEFAULT 10000,
    fail_threshold BIGINT NOT NULL DEFAULT 1,
    recover_threshold BIGINT NOT NULL DEFAULT 1,
    state TEXT NOT NULL DEFAULT 'unknown',
    state_since BIGINT NOT NULL,
    flapping BIGINT NOT NULL DEFAULT 0,
    last_transition_at BIGINT,
    paused_until BIGINT,
    consecutive_fail BIGINT NOT NULL DEFAULT 0,
    consecutive_ok BIGINT NOT NULL DEFAULT 0,
    last_check_at BIGINT,
    last_result TEXT NOT NULL DEFAULT '',
    last_error TEXT NOT NULL DEFAULT '',
    next_check_at BIGINT,
    expected_at BIGINT,
    transitions TEXT NOT NULL DEFAULT '[]',
    run_id TEXT NOT NULL DEFAULT '',
    run_started_at BIGINT,
    lease_owner TEXT NOT NULL DEFAULT '',
    lease_until BIGINT,
    tls_not_after BIGINT,
    created_by TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE (project_id, slug)
);
CREATE INDEX monitors_due ON monitors (enabled, next_check_at);

CREATE TABLE monitor_secrets (
    monitor_id TEXT NOT NULL,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (monitor_id, name)
);

CREATE TABLE monitor_incidents (
    id TEXT PRIMARY KEY,
    monitor_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    started_at BIGINT NOT NULL,
    ended_at BIGINT,
    first_error TEXT NOT NULL DEFAULT '',
    failed_checks BIGINT NOT NULL DEFAULT 0,
    context TEXT NOT NULL DEFAULT '{}',
    acknowledged_by TEXT NOT NULL DEFAULT '',
    acknowledged_at BIGINT
);
CREATE INDEX monitor_incidents_monitor ON monitor_incidents (monitor_id, started_at);
CREATE INDEX monitor_incidents_project ON monitor_incidents (project_id, started_at);

ALTER TABLE alert_deliveries ADD COLUMN subject TEXT NOT NULL DEFAULT 'issue';
