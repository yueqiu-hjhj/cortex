-- Vocabulary terms: per-subject, corpus-aware English word/phrase dictionary
-- distinct from the localStorage-backed flashcard glossary in src/lib/glossary.ts.
-- One row per (subject, normalized_term) pair, with optional definition and a
-- free-form examples blob (newline-separated snippets where the term appears in
-- the user's sources). Self-reported mastery (0..1) is updated by the workbench.
CREATE TABLE IF NOT EXISTS vocab_terms (
  id            TEXT PRIMARY KEY,
  subject_id    TEXT NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
  term          TEXT NOT NULL,
  normalized    TEXT NOT NULL,
  definition    TEXT,
  examples      TEXT,
  freq          INTEGER NOT NULL DEFAULT 0 CHECK(freq >= 0),
  mastery       REAL NOT NULL DEFAULT 0.0 CHECK(mastery BETWEEN 0 AND 1),
  status        TEXT NOT NULL DEFAULT 'new' CHECK(status IN ('new','learning','known')),  -- new | learning | known
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  UNIQUE(subject_id, normalized)
);
CREATE INDEX IF NOT EXISTS vocab_terms_subject_idx ON vocab_terms(subject_id, status);
CREATE INDEX IF NOT EXISTS vocab_terms_freq_idx ON vocab_terms(subject_id, freq DESC);
