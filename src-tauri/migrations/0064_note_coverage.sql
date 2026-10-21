-- Coverage used to be one number with two meanings: "I imported material on
-- this" and "I wrote about this". Notes got no say at all. Split the derived
-- part out so a note-driven raise can be recomputed exactly — and fall back
-- when the note is edited or deleted — instead of being a one-way increment.
PRAGMA foreign_keys = ON;

ALTER TABLE knowledge_nodes ADD COLUMN source_coverage REAL NOT NULL DEFAULT 0.0;
UPDATE knowledge_nodes SET source_coverage = coverage;

-- Which knowledge points each note actually mentions. `hits` is kept because
-- "mentions once in passing" and "returns to it five times" are different
-- evidence, even though coverage currently only counts distinct notes.
CREATE TABLE IF NOT EXISTS note_concepts (
  note_id    TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
  node_id    TEXT NOT NULL REFERENCES knowledge_nodes(id) ON DELETE CASCADE,
  hits       INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (note_id, node_id)
);
CREATE INDEX IF NOT EXISTS idx_note_concepts_note ON note_concepts(note_id);
CREATE INDEX IF NOT EXISTS idx_note_concepts_node ON note_concepts(node_id);
