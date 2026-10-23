-- 0084_paper_journal.sql — journal-quality signals on top of paper_quality.
--
-- `cortex-paper-meta --check` now also asks OpenAlex for the venue of each
-- DOI. OpenAlex's nested `primary_location.source` is a *dehydrated* object:
-- it carries `is_in_doaj`, `is_core` and `type`, but NOT `works_count` /
-- `cited_by_count` (those only exist on /sources). So the classification uses
-- the flags that are actually present.
--
-- This is advisory, not an accusation: DOAJ is a positive whitelist, `core`
-- means OpenAlex's curated core collection, and `journal` only means "a
-- journal we could not confirm in either" — a new or niche title looks the
-- same, so the UI must never call it predatory.
--
-- Nullable on purpose: rows checked before this migration keep NULL and are
-- re-checked on the next `--check --force`.
ALTER TABLE paper_quality ADD COLUMN journal TEXT;
ALTER TABLE paper_quality ADD COLUMN issn TEXT;
ALTER TABLE paper_quality ADD COLUMN in_doaj INTEGER;
ALTER TABLE paper_quality ADD COLUMN is_core INTEGER;
ALTER TABLE paper_quality ADD COLUMN journal_type TEXT;
ALTER TABLE paper_quality ADD COLUMN journal_quality TEXT;
