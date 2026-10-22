-- Growth profile: a cross-conversation learner object.
--
-- Cortex already collects the raw signals a "learner profile" needs — per-node
-- BKT mastery, confidence calibration, and tutor misjudgements. This table is
-- the human-owned object that *reasons* over them. Every item is:
--   • traceable   — `evidence` points back to the row that justified it
--   • finite-state — active → paused / resolved / dismissed, never deleted silently
--   • auditable   — the model only ever *proposes*; a row exists only after the
--                   user confirms. No "agent grows itself" black box.
--
-- `origin='derived'` rows are seeded by `growth_derive`; `origin='manual'` by the
-- user. Status is the only mutable axis the UI flips.

CREATE TABLE growth_items (
    id          TEXT PRIMARY KEY,
    subject_id  TEXT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,            -- goal | misconception | review | strength
    title       TEXT NOT NULL,
    detail      TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'active',  -- active | paused | resolved | dismissed
    origin      TEXT NOT NULL DEFAULT 'manual',  -- derived | manual
    evidence    TEXT NOT NULL DEFAULT '[]',      -- JSON array of {source,ref,excerpt}
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    resolved_at INTEGER
);
CREATE INDEX idx_growth_items_subject ON growth_items(subject_id);
CREATE INDEX idx_growth_items_status  ON growth_items(status);
CREATE INDEX idx_growth_items_kind    ON growth_items(kind);
