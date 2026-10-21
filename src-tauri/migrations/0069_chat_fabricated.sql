-- Chat: persist how many fabricated citations citecheck stripped.
--
-- 0044 persisted retrieval accounting so history could badge old answers;
-- the fabrication count was computed on every answer (ChatAnswer.
-- fabricated_dropped) but thrown away, so the one number that says "the
-- model tried to cite sources that do not exist" survived only until the
-- next reload. Same fix, same shape: a nullable INTEGER column.
ALTER TABLE chat_messages ADD COLUMN fabricated_dropped INTEGER;
