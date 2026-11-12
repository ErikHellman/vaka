CREATE TABLE goals (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    step TEXT NOT NULL,
    value_prop TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    created_by TEXT NOT NULL,
    UNIQUE (project_id, name)
)
