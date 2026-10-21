-- Clips that became flashcards. A separate table rather than a column on
-- `clips`, because one passage can legitimately yield several cards, and
-- because deleting the clip must NOT delete the cards: the cards are the
-- durable artifact, the clip is only the receipt of where they came from.
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS clip_cards (
  clip_id     TEXT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
  material_id TEXT NOT NULL REFERENCES materials(id) ON DELETE CASCADE,
  cards       INTEGER NOT NULL DEFAULT 1,       -- how many cards this batch made
  created_at  INTEGER NOT NULL,
  PRIMARY KEY (clip_id, material_id)
);
CREATE INDEX IF NOT EXISTS idx_clip_cards_clip ON clip_cards(clip_id);
CREATE INDEX IF NOT EXISTS idx_clip_cards_material ON clip_cards(material_id);
