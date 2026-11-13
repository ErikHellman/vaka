CREATE TABLE flags (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    key TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    rollout INTEGER NOT NULL DEFAULT 100,
    variants TEXT NOT NULL DEFAULT '',
    rules TEXT NOT NULL DEFAULT '',
    environments TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    created_by TEXT NOT NULL,
    UNIQUE (project_id, key)
);
CREATE TABLE experiments (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    flag_key TEXT NOT NULL,
    goal TEXT NOT NULL,
    control TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    started_at BIGINT NOT NULL,
    created_by TEXT NOT NULL,
    UNIQUE (project_id, name)
)
