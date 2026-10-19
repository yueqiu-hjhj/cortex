-- 3.4b continuation: sync-type cron jobs can optionally auto-generate
-- flashcards from the sources this run imported (and seed them as due-now
-- SRS rows, so they surface in Review Today without manual scheduling).
ALTER TABLE cron_jobs ADD COLUMN auto_flashcards INTEGER NOT NULL DEFAULT 0;
