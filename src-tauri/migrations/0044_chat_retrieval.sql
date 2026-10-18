-- 0044_chat_retrieval.sql — session-level retrieval accounting.
--
-- The competitive gap: "did this answer actually consult the library?" was
-- computed per answer (uncited/abstained flags on ChatAnswer) but never
-- PERSISTED, so a session could not show a retrieval rate and history could
-- not badge old answers. This column stores
-- {"hits": n, "scope": "subject|topic|source|sources|none", "abstained": bool}
-- per assistant message. NULL for user/system rows and for messages written
-- before this migration (rendered without a badge — no retroactive guessing).
ALTER TABLE chat_messages ADD COLUMN retrieval TEXT;
