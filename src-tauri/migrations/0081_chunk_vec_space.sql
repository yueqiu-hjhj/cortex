-- Per-embedding-space chunk stamping (KnowNote borrow). Cosine similarity is
-- only meaningful inside one embedding space: after switching embed models,
-- old vectors compared against new ones produce plausible-looking scores that
-- mean nothing. Every chunk now carries the space its vector belongs to
-- (embedder.space(), e.g. "onnx:384" / "gemini:text-embedding-004:768"), and
-- vector search only considers the query's space.
--
-- Existing rows are '' (unlabeled). The first ingest after this migration
-- adopts them into the then-current space (the pre-0081 assumption that they
-- were made with the configured embedder); every later space switch retires
-- them properly instead of comparing across spaces.
ALTER TABLE chunks ADD COLUMN vec_space TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_chunks_space ON chunks(vec_space);
