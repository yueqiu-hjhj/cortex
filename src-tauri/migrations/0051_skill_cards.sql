-- 0051_skill_cards.sql — cross-book methodology precipitation.
--
-- cross_book::detect finds concepts that recur across books; this is what
-- they become: a SKILL CARD — what the method is, when to reach for it,
-- concrete steps, and which books demonstrate it. Derived data (the
-- recurring concept + appearances) is honest; the prose is the model's,
-- so cards are hypotheses a student edits, not facts.
CREATE TABLE IF NOT EXISTS skill_cards (
  id           TEXT PRIMARY KEY,
  concept      TEXT NOT NULL,            -- lowercased match key (one card per concept)
  title        TEXT NOT NULL,
  summary      TEXT NOT NULL,
  when_to_use  TEXT NOT NULL,
  steps        TEXT NOT NULL,            -- JSON array of strings
  appearances  TEXT NOT NULL,            -- JSON array of [subject_id, subject_name]
  model        TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS skill_cards_concept_idx ON skill_cards(concept);
