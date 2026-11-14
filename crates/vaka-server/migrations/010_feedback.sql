-- User feedback (in-app widgets, crash prompts, API) and its screenshots.
CREATE TABLE feedback (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    client_id TEXT NOT NULL,
    short_id BIGINT NOT NULL,
    message TEXT NOT NULL,
    contact_email TEXT NOT NULL DEFAULT '',
    contact_name TEXT NOT NULL DEFAULT '',
    user_id TEXT NOT NULL DEFAULT '',
    anon_id TEXT NOT NULL DEFAULT '',
    session_id TEXT NOT NULL DEFAULT '',
    release TEXT NOT NULL DEFAULT '',
    environment TEXT NOT NULL DEFAULT '',
    platform TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    device TEXT NOT NULL DEFAULT '',
    country TEXT NOT NULL DEFAULT '',
    sdk TEXT NOT NULL DEFAULT '',
    flags TEXT NOT NULL DEFAULT '',
    tags TEXT NOT NULL DEFAULT '',
    event_id TEXT NOT NULL DEFAULT '',
    issue_id TEXT,
    source TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'new',
    assignee_id TEXT,
    sentiment TEXT,
    topics TEXT NOT NULL DEFAULT '',
    summary TEXT NOT NULL DEFAULT '',
    tag_state TEXT NOT NULL DEFAULT 'pending',
    tag_attempts BIGINT NOT NULL DEFAULT 0,
    tag_next_at BIGINT,
    attachment_count BIGINT NOT NULL DEFAULT 0,
    alerted BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    received_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE (project_id, client_id),
    UNIQUE (project_id, short_id)
);
CREATE INDEX feedback_project_created ON feedback (project_id, created_at);
CREATE INDEX feedback_issue ON feedback (issue_id);
CREATE INDEX feedback_user ON feedback (project_id, user_id);
CREATE INDEX feedback_session ON feedback (project_id, session_id);
CREATE INDEX feedback_pending ON feedback (alerted, tag_state);
CREATE TABLE feedback_attachments (
    id TEXT PRIMARY KEY,
    feedback_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    position BIGINT NOT NULL,
    name TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size BIGINT NOT NULL,
    sha256 TEXT NOT NULL,
    width BIGINT NOT NULL,
    height BIGINT NOT NULL,
    path TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE INDEX feedback_attachments_feedback ON feedback_attachments (feedback_id)
