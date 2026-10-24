-- 0090_skill_portability.sql — skills become portable files.
--
-- A skill is the reusable part of an agent (a prompt + a way of working), but
-- until now it could only exist in this one database. Export/import needs two
-- facts that were implicit: where a skill came from and which revision it is.
--
--   source   local | bundled | imported
--   version  starts at 1; an import keeps the file's version so two machines
--            can tell whose copy is newer
--
-- Existing rows: the four seeded templates are marked bundled, everything else
-- stays local. Defaults keep old rows valid without a backfill dance.
ALTER TABLE agent_skills ADD COLUMN source TEXT NOT NULL DEFAULT 'local';
ALTER TABLE agent_skills ADD COLUMN version INTEGER NOT NULL DEFAULT 1;

UPDATE agent_skills SET source = 'bundled'
 WHERE name IN ('文献综述骨架', '实验复现清单', '章节草稿起草', '公式推导逐步走');
