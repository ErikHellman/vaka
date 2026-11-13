ALTER TABLE alert_rules ADD COLUMN filters TEXT NOT NULL DEFAULT '';

ALTER TABLE alert_rules ADD COLUMN max_per_hour BIGINT NOT NULL DEFAULT 0;

ALTER TABLE alert_deliveries ADD COLUMN payload TEXT NOT NULL DEFAULT '';

ALTER TABLE alert_deliveries ADD COLUMN attempts BIGINT NOT NULL DEFAULT 1;

ALTER TABLE alert_deliveries ADD COLUMN next_attempt_at BIGINT;

CREATE INDEX alert_deliveries_retry ON alert_deliveries (status, next_attempt_at)
