-- 2.4: cross-paper extraction cells gain PRISMA-grade provenance. Every model
-- value can now carry the verbatim sentence that supports it (evidence) and,
-- when that sentence was located verbatim in the paper card, a back-link to
-- the excerpt row it became / reused. Manual edits clear both (stale proof).
ALTER TABLE extract_cells ADD COLUMN evidence TEXT;
ALTER TABLE extract_cells ADD COLUMN excerpt_id TEXT;
