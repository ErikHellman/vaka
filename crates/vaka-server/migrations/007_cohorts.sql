CREATE TABLE cohorts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    definition TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    created_by TEXT NOT NULL,
    UNIQUE (project_id, name)
)
