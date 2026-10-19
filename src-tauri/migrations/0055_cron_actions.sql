-- 3.4b cron action types: a job is either a `prompt` (default — today's
-- behaviour) or a `sync` (run a research connector into the linked subject).
-- action_ref holds the connector_id for sync jobs; prompt stays as-is so old
-- rows read back unchanged.
ALTER TABLE cron_jobs ADD COLUMN action TEXT NOT NULL DEFAULT 'prompt';
ALTER TABLE cron_jobs ADD COLUMN action_ref TEXT;
