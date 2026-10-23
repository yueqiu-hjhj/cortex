-- 0085_goal_plan.sql — long-term plan text, week numbering, and week-tagged tasks.
--
-- The chain the roadmap promised was "long-term plan → weekly plan → daily
-- plan", but goals could only be broken down deterministically from BKT
-- mastery (and only when bound to a subject). A pasted 12-week plan had no
-- home, and tasks had no week number, so planning week 1 could pull week 12's
-- work.
--
--   goals.plan_text   the pasted plan (source of truth for extraction)
--   goals.plan_weeks  how many weeks it covers (4..=52)
--   goals.start_ms    week 1 anchor; NULL = deadline - weeks*7
--   work_items.week_index  1-based week the task belongs to (NULL = any)
--
-- `week_index` is deliberately on the item, not on a separate week table: the
-- planner already treats items as the unit of work, and a week is just a tag
-- that filters candidates.
ALTER TABLE goals ADD COLUMN plan_text TEXT;
ALTER TABLE goals ADD COLUMN plan_weeks INTEGER;
ALTER TABLE goals ADD COLUMN start_ms INTEGER;
ALTER TABLE work_items ADD COLUMN week_index INTEGER;

CREATE INDEX IF NOT EXISTS idx_wi_goal_week ON work_items(goal_id, week_index);
