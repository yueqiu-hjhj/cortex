-- L2: agent write-back queue.
--
-- Agents (Codex / Claude Code / any MCP client) must not edit notes directly.
-- They append a *proposal* here; the desktop app renders a diff and only the
-- user's click moves anything into `notes` and the vault.
--
-- `base_body` is the note body snapshotted at propose time — accepting a
-- proposal whose base no longer matches the live note is refused unless the
-- user forces it, so a stale draft can never silently undo a later edit.
-- `body` is always the FINAL body, whatever mode produced it: one diff
-- renderer and one apply path for all three kinds.
CREATE TABLE IF NOT EXISTS agent_proposals (
    id             TEXT PRIMARY KEY,
    kind           TEXT NOT NULL,                     -- note_new | note_append | note_replace
    status         TEXT NOT NULL DEFAULT 'pending',   -- pending | accepted | rejected | withdrawn
    note_id        TEXT,                              -- target note; NULL for note_new
    title          TEXT NOT NULL,
    subject_id     TEXT,
    base_body      TEXT NOT NULL DEFAULT '',
    body           TEXT NOT NULL,
    rationale      TEXT NOT NULL DEFAULT '',
    author         TEXT NOT NULL DEFAULT '',
    created_at     INTEGER NOT NULL,
    resolved_at    INTEGER,
    result_note_id TEXT
);

CREATE INDEX IF NOT EXISTS idx_agent_proposals_status
    ON agent_proposals(status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_proposals_note
    ON agent_proposals(note_id, status);
