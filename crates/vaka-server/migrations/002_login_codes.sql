CREATE TABLE login_codes (
    user_id TEXT PRIMARY KEY,
    code_hash TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    attempts BIGINT NOT NULL DEFAULT 0
)
