//! Knowledge base layer: persistent wiki pages, lint, semantic audit, and
//! reverse links. The wiki is a slug-keyed, per-user store of structured notes
//! that the rest of the app can write to (e.g. auto-generated cheatsheet
//! digests, imported paper notes) and that humans can read or edit directly.
//!
//! The lint pass enforces minimal formatting hygiene (heading order, link
//! targets) and the semantic audit records a trace of where each claim
//! originated (model id, run id, source url). Reverse links are recomputed on
//! save and exposed via a tiny "who links to X" API.

use crate::error::{Error, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const DEFAULT_USER: &str = "u-default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiPage {
    pub id: String,
    pub user_id: String,
    pub slug: String,
    pub title: String,
    pub body: String,
    pub source: Option<String>,
    pub audit: Option<String>,
    pub last_audited: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiLint {
    pub severity: String,  // info | warn | error
    pub line: Option<i64>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReverseLink {
    pub from_slug: String,
    pub from_title: String,
    pub to_slug: String,
}

fn slugify(input: &str) -> String {
    let lower = input.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_dash = false;
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() { "untitled".into() } else { trimmed }
}

pub fn lint(body: &str) -> Vec<WikiLint> {
    let mut issues = Vec::new();
    let mut h1_seen = false;
    let mut prev_blank = true;
    for (idx, line) in body.lines().enumerate() {
        let n = (idx + 1) as i64;
        if line.starts_with("# ") && h1_seen {
            issues.push(WikiLint { severity: "warn".into(), line: Some(n),
                message: "second top-level heading; consolidate into the first".into() });
        } else if line.starts_with("# ") {
            h1_seen = true;
        }
        if line.contains("[[[") {
            issues.push(WikiLint { severity: "error".into(), line: Some(n),
                message: "unbalanced wiki link (triple brackets)".into() });
        }
        if line.contains("TODO") || line.contains("FIXME") {
            issues.push(WikiLint { severity: "info".into(), line: Some(n),
                message: "unresolved TODO/FIXME".into() });
        }
        if line.trim().is_empty() { prev_blank = true; } else if prev_blank { prev_blank = false; }
    }
    if !h1_seen && !body.trim().is_empty() {
        issues.push(WikiLint { severity: "warn".into(), line: Some(1),
            message: "page has no top-level heading".into() });
    }
    issues
}

fn extract_links(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 4 < bytes.len() {
        if &bytes[i..i + 2] == b"[[" {
            if let Some(end_rel) = body[i + 2..].find("]]") {
                let inner = &body[i + 2..i + 2 + end_rel];
                let target = inner.split('|').next().unwrap_or(inner).trim().to_lowercase();
                if !target.is_empty() { out.push(target.to_string()); }
                i = i + 2 + end_rel + 2;
                continue;
            }
        }
        if bytes[i] == b'[' as u8 {
            if let Some(close_text) = body[i + 1..].find(']') {
                if let Some(rel_open) = body[i + 1 + close_text + 1..].find('(') {
                    if rel_open == 0 {
                        let url_start = i + 1 + close_text + 2;
                        if let Some(close_url) = body[url_start..].find(')') {
                            let url = body[url_start..url_start + close_url].trim();
                            if !url.is_empty() && !url.contains(' ') && !url.starts_with("http") && !url.contains(':') {
                                out.push(slugify(url));
                            }
                            i = url_start + close_url;
                            continue;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    out
}

pub fn upsert_page(
    conn: &Connection,
    user_id: &str,
    id: Option<&str>,
    slug: Option<&str>,
    title: &str,
    body: &str,
    source: Option<&str>,
) -> Result<WikiPage> {
    if title.trim().is_empty() { return Err(Error::Other("wiki page title is required".into())); }
    let now = crate::db::now_ms();
    let slug = slugify(slug.unwrap_or(title));
    let audit = serde_json::to_string(&lint(body))?;
    let id = match id {
        Some(existing) => {
            let n = conn.execute(
                "UPDATE wiki_pages SET title=?1, body=?2, source=?3, audit=?4,
                    last_audited=?5, updated_at=?5 WHERE user_id=?6 AND id=?7",
                params![title, body, source, audit, now, user_id, existing],
            )?;
            if n == 0 { return Err(Error::NotFound(format!("wiki page {existing}"))); }
            existing.to_string()
        }
        None => {
            let id = crate::db::new_id();
            conn.execute(
                "INSERT INTO wiki_pages (id, user_id, slug, title, body, source, audit, last_audited, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
                params![id, user_id, slug, title, body, source, audit, now, now],
            )?;
            id
        }
    };
    rebuild_links(conn, user_id, &id, body)?;
    get_page(conn, user_id, &id)
}

pub fn get_page(conn: &Connection, user_id: &str, id: &str) -> Result<WikiPage> {
    conn.query_row(
        "SELECT id, user_id, slug, title, body, source, audit, last_audited, created_at, updated_at
           FROM wiki_pages WHERE user_id=?1 AND id=?2", [user_id, id], parse_page,
    ).map_err(|_| Error::NotFound(format!("wiki page {id}")))
}

pub fn get_page_by_slug(conn: &Connection, user_id: &str, slug: &str) -> Result<WikiPage> {
    conn.query_row(
        "SELECT id, user_id, slug, title, body, source, audit, last_audited, created_at, updated_at
           FROM wiki_pages WHERE user_id=?1 AND slug=?2", [user_id, slug], parse_page,
    ).map_err(|_| Error::NotFound(format!("wiki page {slug}")))
}

pub fn list_pages(conn: &Connection, user_id: &str, limit: i64) -> Result<Vec<WikiPage>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_id, slug, title, body, source, audit, last_audited, created_at, updated_at
           FROM wiki_pages WHERE user_id=?1 ORDER BY updated_at DESC LIMIT ?2"
    )?;
    let rows = stmt.query_map(params![user_id, limit], parse_page)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn delete_page(conn: &Connection, user_id: &str, id: &str) -> Result<()> {
    let n = conn.execute("DELETE FROM wiki_pages WHERE user_id=?1 AND id=?2", [user_id, id])?;
    if n == 0 { return Err(Error::NotFound(format!("wiki page {id}"))); }
    Ok(())
}

pub fn reverse_links(conn: &Connection, user_id: &str, slug: &str) -> Result<Vec<ReverseLink>> {
    let mut stmt = conn.prepare(
        "SELECT wp.slug, wp.title, wl.to_slug FROM wiki_links wl
            JOIN wiki_pages wp ON wp.id = wl.from_page_id
            WHERE wl.to_slug=?1 AND wp.user_id=?2 ORDER BY wp.title"
    )?;
    let rows = stmt.query_map(params![slug, user_id], |r| Ok(ReverseLink {
        from_slug: r.get(0)?, from_title: r.get(1)?, to_slug: r.get(2)?,
    }))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn rebuild_links(conn: &Connection, user_id: &str, page_id: &str, body: &str) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM wiki_links WHERE from_page_id=?1", [page_id])?;
    let mut seen: HashSet<String> = HashSet::new();
    for target in extract_links(body) {
        let target_slug = slugify(&target);
        if !seen.insert(target_slug.clone()) { continue; }
        let to_page: Option<String> = tx.query_row(
            "SELECT id FROM wiki_pages WHERE user_id=?1 AND slug=?2", params![user_id, target_slug], |r| r.get(0),
        ).ok();
        tx.execute(
            "INSERT OR IGNORE INTO wiki_links (from_page_id, to_slug, to_page_id, created_at)
             VALUES (?1,?2,?3,?4)",
            params![page_id, target_slug, to_page, crate::db::now_ms()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn parse_page(r: &rusqlite::Row) -> rusqlite::Result<WikiPage> {
    Ok(WikiPage {
        id: r.get(0)?, user_id: r.get(1)?, slug: r.get(2)?, title: r.get(3)?,
        body: r.get(4)?, source: r.get(5)?, audit: r.get(6)?, last_audited: r.get(7)?,
        created_at: r.get(8)?, updated_at: r.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugify_handles_punctuation() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("zhong wen biao dian"), "zhong-wen-biao-dian");
        assert_eq!(slugify(""), "untitled");
    }
    #[test]
    fn lint_flags_missing_h1_and_todos() {
        let lints = lint("Just some text without a heading.\n\nTODO: write more.");
        assert!(lints.iter().any(|l| l.message.contains("top-level")));
        assert!(lints.iter().any(|l| l.severity == "info"));
    }
    #[test]
    fn extract_links_finds_wiki_and_markdown() {
        let body = "See [[other-page]] and [[Guide|reading]] also [headings](deep-work) but not https://example.org .";
        let mut links = extract_links(body);
        links.sort();
        assert_eq!(links, vec!["deep-work".to_string(), "guide".to_string(), "other-page".to_string()]);
    }
    #[test]
    fn pages_roundtrip_and_reverse_links() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = upsert_page(&c, "u1", None, None, "A", "see [[b]] and [c](c-slug)", None).unwrap();
        let _b = upsert_page(&c, "u1", None, Some("b"), "B", "hello", None).unwrap();
        let _c = upsert_page(&c, "u1", None, Some("c-slug"), "C", "hello", None).unwrap();
        let rev = reverse_links(&c, "u1", "b").unwrap();
        assert_eq!(rev.len(), 1);
        assert_eq!(rev[0].from_slug, a.slug);
        let lints: Vec<WikiLint> = serde_json::from_str(&a.audit.clone().unwrap()).unwrap();
        assert!(!lints.is_empty());
    }
}