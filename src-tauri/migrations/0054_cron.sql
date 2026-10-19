-- 0054: Cron / scheduled automation (3.4). A job = prompt + schedule; runs are
-- logged with their output so results stay auditable without opening a source.
CREATE TABLE IF NOT EXISTS cron_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    prompt TEXT NOT NULL,
    subject_id TEXT,
    kind TEXT NOT NULL DEFAULT 'interval',   -- 'interval' | 'daily'
    interval_minutes INTEGER,
    daily_time TEXT,                          -- "HH:MM" local wall time
    tz_offset_minutes INTEGER NOT NULL DEFAULT 480,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_run_at INTEGER,
    next_run_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cron_jobs_due ON cron_jobs (enabled, next_run_at);

CREATE TABLE IF NOT EXISTS cron_runs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    status TEXT NOT NULL DEFAULT 'running',   -- running | ok | error
    source_id TEXT,
    output TEXT,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_cron_runs_job ON cron_runs (job_id, started_at);
