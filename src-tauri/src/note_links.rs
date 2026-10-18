//! Wiki-style `[[double links]]` between notes (competitive gap: 双链).
//!
//! Writing `[[德布罗意波长]]` or `[[德布罗意波长|物质波]]` in a note creates a
//! link. The index (`note_links`) is rebuilt whenever a note is saved, and the
//! target's backlinks panel lists every note that mentions it. Resolution is by
//! lowercased title slug — renaming a target re-slugs on save, no id hunt.
//!
//! Deliberate scope: this is a LINK INDEX, not a graph engine. No auto-linking,
//! no aliases beyond the `|alias` display form, no link to sources (they have
//! citations instead).

use crate::db::{new_id, now_ms};
use crate::error::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Backlink {
    pub note_id: String,
    pub title: String,
    /// The display alias used at the link site (None when the link was bare).
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedLink {
    pub target: String,
    pub alias: Option<String>,
}

/// Extract `[[target]]` / `[[target|alias]]` occurrences in order, deduped by
/// (target, alias). Tolerates spaces, rejects empty targets and nested `[`.
pub fn parse_links(body: &str) -> Vec<ParsedLink> {
    let mut out: Vec<ParsedLink> = Vec::new();
    let bytes: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == '[' && bytes[i + 1] == '[' {
            // Find the closing `]]`.
            let mut j = i + 2;
            let mut inner = String::new();
            let mut closed = false;
            while j + 1 < bytes.len() {
                if bytes[j] == ']' && bytes[j + 1] == ']' {
                    closed = true;
                    break;
                }
                // A nested `[` aborts this candidate — malformed, skip.
                if bytes[j] == '[' {
                    break;
                }
                inner.push(bytes[j]);
                j += 1;
            }
            if closed {
                let inner = inner.trim();
                if !inner.is_empty() {
                    let (target, alias) = match inner.split_once('|') {
                        Some((t, a)) => (t.trim(), Some(a.trim().to_string())),
                        None => (inner, None),
                    };
                    if !target.is_empty() {
                        let link = ParsedLink {
                            target: target.to_string(),
                            alias: alias.filter(|a| !a.is_empty()),
                        };
                        if !out.contains(&link) {
                            out.push(link);
                        }
                    }
                }
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Slug used for matching: trimmed + lowercased. Chinese text has no case, so
/// this mainly normalizes Latin titles.
pub fn slug(title: &str) -> String {
    title.trim().to_lowercase()
}

/// Rebuild the outbound-link index for one note. Called on create/update;
/// deleting the note cascades via the FK.
pub fn rebuild(conn: &Connection, note_id: &str, body: &str) -> Result<()> {
    conn.execute("DELETE FROM note_links WHERE from_note_id = ?1", [note_id])?;
    let ts = now_ms();
    for link in parse_links(body) {
        conn.execute(
            "INSERT INTO note_links (id, from_note_id, target_slug, alias, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![new_id(), note_id, slug(&link.target), link.alias, ts],
        )?;
    }
    Ok(())
}

/// Notes that link TO the given title (backlinks panel).
pub fn backlinks(conn: &Connection, title: &str) -> Result<Vec<Backlink>> {
    let mut st = conn.prepare(
        "SELECT n.id, n.title, l.alias
           FROM note_links l JOIN notes n ON n.id = l.from_note_id
          WHERE l.target_slug = ?1
          ORDER BY n.updated_at DESC",
    )?;
    let rows = st
        .query_map([slug(title)], |r| {
            Ok(Backlink {
                note_id: r.get(0)?,
                title: r.get(1)?,
                alias: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The note whose title matches a link target (for click-through), if any.
pub fn resolve(conn: &Connection, target: &str) -> Result<Option<(String, String)>> {
    let s = slug(target);
    Ok(conn
        .query_row(
            "SELECT id, title FROM notes WHERE lower(trim(title)) = ?1 LIMIT 1",
            [s],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_aliased_links() {
        let body = "见 [[德布罗意波长]] 与 [[物质波|德布罗意波长]]，重复 [[德布罗意波长]] 去重。";
        let links = parse_links(body);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "德布罗意波长");
        assert_eq!(links[0].alias, None);
        assert_eq!(links[1].target, "物质波");
        assert_eq!(links[1].alias.as_deref(), Some("德布罗意波长"));
    }

    #[test]
    fn ignores_malformed_and_empty() {
        assert!(parse_links("[[]] 空").is_empty());
        assert!(parse_links("[[|只别名]]").is_empty());
        assert!(parse_links("没有链接").is_empty());
        // Unclosed `[[` must not swallow the rest of the note.
        assert!(parse_links("[[未闭合 内容").is_empty());
        assert_eq!(parse_links("[[a]] [[b").len(), 1);
    }

    #[test]
    fn rebuild_and_backlinks_roundtrip() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = crate::repo::insert_note(&c, None, None, "笔记A", "引用 [[笔记B|B 篇]] 和 [[不存在]]", ).unwrap();
        let b = crate::repo::insert_note(&c, None, None, "笔记B", "被引用", ).unwrap();
        rebuild(&c, &a, "引用 [[笔记B|B 篇]] 和 [[不存在]]").unwrap();

        let backs = backlinks(&c, "笔记B").unwrap();
        assert_eq!(backs.len(), 1);
        assert_eq!(backs[0].note_id, a);
        assert_eq!(backs[0].alias.as_deref(), Some("B 篇"));
        // An UNRESOLVED target still has backlinks — that is exactly how the
        // UI discovers "referenced but not yet written". It just resolves to
        // no note.
        assert_eq!(backlinks(&c, "不存在").unwrap().len(), 1);
        assert!(resolve(&c, "不存在").unwrap().is_none());
        // Case-insensitive resolution for Latin titles.
        let (rid, rtitle) = resolve(&c, "笔记b").unwrap().unwrap();
        assert_eq!(rid, b);
        assert_eq!(rtitle, "笔记B");
        // Rebuild with a new body replaces the old index.
        rebuild(&c, &a, "只引用 [[笔记B]]").unwrap();
        let backs = backlinks(&c, "笔记B").unwrap();
        assert_eq!(backs[0].alias, None);
        // Deleting the source note removes its outbound links (FK cascade).
        crate::repo::delete_note(&c, &a).unwrap();
        assert!(backlinks(&c, "笔记B").unwrap().is_empty());
    }
}
