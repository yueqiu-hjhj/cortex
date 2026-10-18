-- 0046_chunk_offsets.sql — character offsets for citation verification.
--
-- KnowNote's passages record start/end offsets so an external agent can verify
-- a claim by slicing the source text itself. Cortex's chunks carried only
-- `ord` + `loc` ("p.14"). These two columns pin each chunk to its exact
-- character span in the source's extracted text.
--   - new chunks get offsets at ingest (sequential index_of, same algorithm
--     as the backfill command)
--   - NULL = not yet located (pre-0046 chunks) — `backfill_chunk_offsets`
--     fills them; search still works without offsets.
ALTER TABLE chunks ADD COLUMN char_start INTEGER;
ALTER TABLE chunks ADD COLUMN char_end   INTEGER;
