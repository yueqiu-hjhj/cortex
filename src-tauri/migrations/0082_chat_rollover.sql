-- Chat auto-rollover (KnowNote borrow): a conversation that grows past the
-- context budget is summarized, archived, and continued in a fresh thread —
-- never silently. `rolled_from` records the lineage so the thread list can
-- show which conversation a thread continues.
ALTER TABLE chat_threads ADD COLUMN rolled_from TEXT REFERENCES chat_threads(id) ON DELETE SET NULL;
