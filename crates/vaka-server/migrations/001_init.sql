CREATE TABLE users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    disabled BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE web_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);

CREATE TABLE invites (
    token_hash TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    role TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    accepted_at BIGINT
);

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    platform TEXT NOT NULL,
    retention_days BIGINT NOT NULL,
    scrub_enabled BIGINT NOT NULL DEFAULT 1,
    scrub_fields TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL
);

CREATE TABLE ingest_keys (
    key TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    revoked_at BIGINT
);

CREATE INDEX ingest_keys_project ON ingest_keys (project_id);

CREATE TABLE api_tokens (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    token_prefix TEXT NOT NULL,
    project_id TEXT,
    scope TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    expires_at BIGINT,
    last_used_at BIGINT,
    revoked_at BIGINT
);

CREATE TABLE issues (
    id TEXT PRIMARY KEY,
    short_id BIGINT NOT NULL,
    project_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    title TEXT NOT NULL,
    culprit TEXT NOT NULL,
    level TEXT NOT NULL,
    platform TEXT NOT NULL,
    status TEXT NOT NULL,
    assignee_id TEXT,
    first_seen BIGINT NOT NULL,
    last_seen BIGINT NOT NULL,
    times_seen BIGINT NOT NULL,
    first_release TEXT NOT NULL,
    last_release TEXT NOT NULL,
    resolved_at BIGINT,
    resolved_in_release TEXT,
    regressed_at BIGINT,
    ignored_until BIGINT,
    UNIQUE (project_id, fingerprint)
);

CREATE INDEX issues_project_last_seen ON issues (project_id, last_seen);
CREATE INDEX issues_project_short ON issues (project_id, short_id);

CREATE TABLE issue_comments (
    id TEXT PRIMARY KEY,
    issue_id TEXT NOT NULL,
    author TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE INDEX issue_comments_issue ON issue_comments (issue_id);

CREATE TABLE releases (
    project_id TEXT NOT NULL,
    version TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (project_id, version)
);

CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    release TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    debug_id TEXT NOT NULL DEFAULT '',
    path TEXT NOT NULL,
    size BIGINT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE INDEX artifacts_lookup ON artifacts (project_id, kind, release);
CREATE INDEX artifacts_debug ON artifacts (project_id, debug_id);

CREATE TABLE alert_rules (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    name TEXT NOT NULL,
    trigger TEXT NOT NULL,
    channel TEXT NOT NULL,
    target TEXT NOT NULL,
    secret TEXT NOT NULL DEFAULT '',
    threshold BIGINT NOT NULL DEFAULT 0,
    enabled BIGINT NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL
);

CREATE TABLE alert_deliveries (
    id TEXT PRIMARY KEY,
    rule_id TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    trigger TEXT NOT NULL,
    status TEXT NOT NULL,
    detail TEXT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE INDEX alert_deliveries_rule ON alert_deliveries (rule_id, created_at);

CREATE TABLE daily_salts (
    day TEXT PRIMARY KEY,
    salt TEXT NOT NULL
);

CREATE TABLE audit_log (
    id TEXT PRIMARY KEY,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    detail TEXT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE TABLE counters (
    name TEXT PRIMARY KEY,
    value BIGINT NOT NULL
)
