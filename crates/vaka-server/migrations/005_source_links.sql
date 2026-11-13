CREATE TABLE code_mappings (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    stack_root TEXT NOT NULL,
    source_root TEXT NOT NULL,
    repository TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    position BIGINT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE INDEX code_mappings_project ON code_mappings (project_id);

CREATE TABLE issue_links (
    id TEXT PRIMARY KEY,
    issue_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE INDEX issue_links_issue ON issue_links (issue_id);

ALTER TABLE releases ADD COLUMN head_commit TEXT NOT NULL DEFAULT ''
