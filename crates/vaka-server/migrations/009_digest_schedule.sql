-- Digest schedules: {frequency, hour, weekday, timezone} as JSON (empty means daily at the threshold hour (UTC)).
ALTER TABLE alert_rules ADD COLUMN schedule TEXT NOT NULL DEFAULT ''
