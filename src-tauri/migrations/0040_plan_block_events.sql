-- 0040_plan_block_events.sql — D4: time-blocking write-back to Google Calendar.
--
-- Maps plan_blocks to the Google Calendar events created for them, so pushing
-- is idempotent (update instead of duplicate), a reschedule in Google Calendar
-- can flow back into the block times, and deleting the block (or marking it
-- done) removes the calendar slot.
--
-- A separate table, not a column on plan_blocks: the livesync merge inventories
-- tables generically, and a standard id/created_at/updated_at shape here keeps
-- that machinery happy while keeping the mapping out of the planner's merge
-- logic. The extended property `cortex_plan_block: <block_id>` is ALSO written
-- onto each Google event, so the pull side can match events without this table
-- (the table is the fast path; the marker is the source of truth).
CREATE TABLE IF NOT EXISTS plan_block_events (
  id              TEXT PRIMARY KEY,
  block_id        TEXT NOT NULL UNIQUE,
  google_event_id TEXT NOT NULL,
  synced_title    TEXT,
  synced_start    INTEGER,
  synced_end      INTEGER,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
);
