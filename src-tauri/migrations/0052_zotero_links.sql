-- Zotero itemID ↔ papers.id bridge for the incremental sync (roadmap 2.2).
--
-- Deliberately NOT workitems.item_links: that table's item_id carries a
-- FOREIGN KEY to work_items(id), and a Zotero item is not a work item —
-- writing "zotero:<itemID>" rows there would violate the FK on every
-- connection (foreign_keys = ON app-wide) and pollute the work-item graph.
-- One row per linked Zotero item; papers cascade on delete.
CREATE TABLE IF NOT EXISTS zotero_links (
  zotero_item_id INTEGER PRIMARY KEY,
  paper_id       TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
  created_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_zotero_links_paper ON zotero_links(paper_id);
