CREATE TABLE release_commits (
    project_id TEXT NOT NULL,
    release TEXT NOT NULL,
    sha TEXT NOT NULL,
    message TEXT NOT NULL,
    author_name TEXT NOT NULL,
    author_email TEXT NOT NULL,
    committed_at BIGINT NOT NULL,
    files TEXT NOT NULL,
    position BIGINT NOT NULL,
    PRIMARY KEY (project_id, release, sha)
);

CREATE TABLE ownership_rules (
    project_id TEXT PRIMARY KEY,
    rules TEXT NOT NULL,
    updated_at BIGINT NOT NULL,
    updated_by TEXT NOT NULL
);

ALTER TABLE issues ADD COLUMN owners TEXT NOT NULL DEFAULT '';

ALTER TABLE releases ADD COLUMN repository TEXT NOT NULL DEFAULT ''
