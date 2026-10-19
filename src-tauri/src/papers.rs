//! Research pipeline: the reading list and the excerpts taken from it.
//!
//! Before this module the app could *find* papers (`literature_search`) and
//! *display* one, but had nowhere to put "I plan to read this", "I read it",
//! "this sentence is the one I will cite". A research workflow that cannot
//! remember what you read is just a search box.
//!
//! The pipeline this enables:
//!
//!   search → paper (to_read) → reading task (work_items) → excerpts
//!          → wiki synthesis (item_links) → citation (BibTeX)
//!
//! Deliberate choices:
//!   • **Dedupe by DOI.** Importing the same paper from two providers must not
//!     create two rows; a partial unique index enforces it.
//!   • **Authors as a comma string.** A join table would be "correct" and would
//!     make every list query unreadable for zero practical gain at this scale.
//!   • **Excerpts keep the quote verbatim.** No summarising at capture time —
//!     the whole point of an excerpt is that you can paste it into a paper
//!     unchanged and defend it.

use crate::db::{new_id, now_ms};
use crate::db::AppState;
use crate::error::{Error, Result};
use crate::workitems::{self, ItemInput, WorkItem};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

pub const PAPER_STATUSES: &[&str] = &["to_read", "reading", "read", "cited", "dropped"];

#[derive(Debug, Clone, Serialize)]
pub struct Paper {
    pub id: String,
    pub title: String,
    pub authors: Option<String>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub abstract_text: Option<String>,
    pub status: String,
    pub rating: Option<i64>,
    pub notes: Option<String>,
    pub subject_id: Option<String>,
    pub source_id: Option<String>,
    pub added_at: i64,
    pub updated_at: i64,
    /// Derived: how many excerpts have been taken from this paper.
    pub excerpt_count: i64,
    /// Derived: open reading/writing tasks linked to it.
    pub open_task_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Excerpt {
    pub id: String,
    pub paper_id: Option<String>,
    pub source_id: Option<String>,
    pub quote: String,
    pub page: Option<i64>,
    pub note: Option<String>,
    pub tags: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Joined for display in the cross-paper excerpt list.
    pub paper_title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaperInput {
    pub title: String,
    pub authors: Option<String>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub abstract_text: Option<String>,
    pub subject_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaperPatch {
    pub status: Option<String>,
    pub rating: Option<i64>,
    pub notes: Option<String>,
    pub subject_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportOutcome {
    pub imported: i64,
    pub skipped: i64,
    pub titles: Vec<String>,
}

// ------------------------------------------------------------- row mapping

const PAPER_COLUMNS: &str = "p.id, p.title, p.authors, p.year, p.venue, p.doi, p.url, \
     p.abstract, p.status, p.rating, p.notes, p.subject_id, p.source_id, p.added_at, \
     p.updated_at, \
     (SELECT COUNT(*) FROM excerpts e WHERE e.paper_id = p.id), \
     (SELECT COUNT(*) FROM item_links l JOIN work_items w ON w.id = l.item_id \
        WHERE l.entity_table = 'papers' AND l.entity_id = p.id \
          AND w.status IN ('open','doing','blocked'))";

fn row_to_paper(row: &rusqlite::Row<'_>) -> rusqlite::Result<Paper> {
    Ok(Paper {
        id: row.get(0)?,
        title: row.get(1)?,
        authors: row.get(2)?,
        year: row.get(3)?,
        venue: row.get(4)?,
        doi: row.get(5)?,
        url: row.get(6)?,
        abstract_text: row.get(7)?,
        status: row.get(8)?,
        rating: row.get(9)?,
        notes: row.get(10)?,
        subject_id: row.get(11)?,
        source_id: row.get(12)?,
        added_at: row.get(13)?,
        updated_at: row.get(14)?,
        excerpt_count: row.get(15)?,
        open_task_count: row.get(16)?,
    })
}

fn normalise_doi(doi: Option<&str>) -> Option<String> {
    let d = doi?.trim();
    if d.is_empty() {
        return None;
    }
    // DOIs are case-insensitive and often pasted with a resolver prefix.
    let d = d
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("doi:")
        .trim();
    if d.is_empty() {
        None
    } else {
        Some(d.to_lowercase())
    }
}

// ------------------------------------------------------------------- CRUD

/// Insert a paper, or return the existing row when the DOI already exists.
pub fn import_paper(conn: &Connection, input: &PaperInput) -> Result<Paper> {
    if input.title.trim().is_empty() {
        return Err(Error::Other("paper title cannot be empty".into()));
    }
    let doi = normalise_doi(input.doi.as_deref());
    if let Some(d) = &doi {
        if let Ok(existing) = find_by_doi(conn, d) {
            return Ok(existing);
        }
    }
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO papers (id, title, authors, year, venue, doi, url, abstract, status, \
         rating, notes, subject_id, source_id, added_at, updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'to_read',NULL,NULL,?9,?10,?11,?11)",
        params![
            id,
            input.title.trim(),
            input.authors,
            input.year,
            input.venue,
            doi,
            input.url,
            input.abstract_text,
            input.subject_id,
            input.source_id,
            now,
        ],
    )?;
    get_paper(conn, &id)
}

fn find_by_doi(conn: &Connection, doi: &str) -> Result<Paper> {
    let sql = format!("SELECT {PAPER_COLUMNS} FROM papers p WHERE p.doi = ?1");
    conn.query_row(&sql, params![doi], row_to_paper)
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("paper with doi {doi}")),
            other => Error::Db(other),
        })
}

pub fn get_paper(conn: &Connection, id: &str) -> Result<Paper> {
    let sql = format!("SELECT {PAPER_COLUMNS} FROM papers p WHERE p.id = ?1");
    conn.query_row(&sql, params![id], row_to_paper)
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("paper {id}")),
            other => Error::Db(other),
        })
}

pub fn list_papers(
    conn: &Connection,
    status: Option<&str>,
    subject_id: Option<&str>,
    search: Option<&str>,
) -> Result<Vec<Paper>> {
    let mut sql = format!("SELECT {PAPER_COLUMNS} FROM papers p WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = status {
        if s != "all" {
            if !PAPER_STATUSES.contains(&s) {
                return Err(Error::Other(format!("invalid paper status '{s}'")));
            }
            sql.push_str(" AND p.status = ?");
            args.push(Box::new(s.to_string()));
        }
    }
    if let Some(sub) = subject_id {
        sql.push_str(" AND p.subject_id = ?");
        args.push(Box::new(sub.to_string()));
    }
    if let Some(term) = search {
        let like = format!("%{}%", term.trim());
        sql.push_str(" AND (p.title LIKE ? OR p.authors LIKE ? OR p.abstract LIKE ?)");
        args.push(Box::new(like.clone()));
        args.push(Box::new(like.clone()));
        args.push(Box::new(like));
    }
    // Unread first, then most recently touched.
    sql.push_str(
        " ORDER BY CASE p.status WHEN 'reading' THEN 0 WHEN 'to_read' THEN 1 \
         WHEN 'read' THEN 2 WHEN 'cited' THEN 3 ELSE 4 END, p.updated_at DESC",
    );
    let mut st = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = st.query_map(refs.as_slice(), row_to_paper)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn update_paper(conn: &Connection, id: &str, patch: &PaperPatch) -> Result<Paper> {
    let existing = get_paper(conn, id)?;
    if let Some(s) = &patch.status {
        if !PAPER_STATUSES.contains(&s.as_str()) {
            return Err(Error::Other(format!(
                "invalid paper status '{s}' (expected one of: {})",
                PAPER_STATUSES.join(", ")
            )));
        }
    }
    conn.execute(
        "UPDATE papers SET status=?2, rating=?3, notes=?4, subject_id=?5, source_id=?6, \
         updated_at=?7 WHERE id=?1",
        params![
            id,
            patch.status.clone().unwrap_or(existing.status),
            patch.rating.or(existing.rating),
            patch.notes.clone().or(existing.notes),
            patch.subject_id.clone().or(existing.subject_id),
            patch.source_id.clone().or(existing.source_id),
            now_ms(),
        ],
    )?;
    get_paper(conn, id)
}

pub fn delete_paper(conn: &Connection, id: &str) -> Result<()> {
    let changed = conn.execute("DELETE FROM papers WHERE id = ?1", params![id])?;
    if changed == 0 {
        return Err(Error::NotFound(format!("paper {id}")));
    }
    Ok(())
}

// --------------------------------------------------------------- excerpts

pub fn add_excerpt(
    conn: &Connection,
    paper_id: Option<&str>,
    source_id: Option<&str>,
    quote: &str,
    page: Option<i64>,
    note: Option<&str>,
    tags: Option<&str>,
) -> Result<Excerpt> {
    if quote.trim().is_empty() {
        return Err(Error::Other("an excerpt needs a quote".into()));
    }
    if paper_id.is_none() && source_id.is_none() {
        return Err(Error::Other(
            "an excerpt must belong to a paper or a source".into(),
        ));
    }
    if let Some(p) = paper_id {
        get_paper(conn, p)?;
    }
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO excerpts (id, paper_id, source_id, quote, page, note, tags, \
         created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
        params![id, paper_id, source_id, quote.trim(), page, note, tags, now],
    )?;
    // Reading a paper and taking notes from it is the definition of "reading".
    if let Some(p) = paper_id {
        conn.execute(
            "UPDATE papers SET status='reading', updated_at=?2 WHERE id=?1 AND status='to_read'",
            params![p, now],
        )?;
    }
    get_excerpt(conn, &id)
}

fn row_to_excerpt(row: &rusqlite::Row<'_>) -> rusqlite::Result<Excerpt> {
    Ok(Excerpt {
        id: row.get(0)?,
        paper_id: row.get(1)?,
        source_id: row.get(2)?,
        quote: row.get(3)?,
        page: row.get(4)?,
        note: row.get(5)?,
        tags: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        paper_title: row.get(9)?,
    })
}

const EXCERPT_COLUMNS: &str = "e.id, e.paper_id, e.source_id, e.quote, e.page, e.note, \
     e.tags, e.created_at, e.updated_at, p.title";

pub fn get_excerpt(conn: &Connection, id: &str) -> Result<Excerpt> {
    let sql = format!(
        "SELECT {EXCERPT_COLUMNS} FROM excerpts e \
         LEFT JOIN papers p ON p.id = e.paper_id WHERE e.id = ?1"
    );
    conn.query_row(&sql, params![id], row_to_excerpt)
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("excerpt {id}")),
            other => Error::Db(other),
        })
}

pub fn list_excerpts(conn: &Connection, paper_id: Option<&str>) -> Result<Vec<Excerpt>> {
    let sql = format!(
        "SELECT {EXCERPT_COLUMNS} FROM excerpts e \
         LEFT JOIN papers p ON p.id = e.paper_id \
         {} ORDER BY e.created_at DESC LIMIT 500",
        if paper_id.is_some() {
            "WHERE e.paper_id = ?1"
        } else {
            ""
        }
    );
    let mut st = conn.prepare(&sql)?;
    let rows = if let Some(pid) = paper_id {
        st.query_map(params![pid], row_to_excerpt)?
    } else {
        st.query_map([], row_to_excerpt)?
    };
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete_excerpt(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM excerpts WHERE id = ?1", params![id])?;
    Ok(())
}

// ---------------------------------------------------- reading task linkage

/// Start reading: create a `reading` work item, link it to the paper, and move
/// the paper to `reading`. The item is what the planner can schedule, so this is
/// the join between the research list and the week.
pub fn start_reading(conn: &Connection, paper_id: &str, due_ms: Option<i64>) -> Result<WorkItem> {
    let paper = get_paper(conn, paper_id)?;
    let item = workitems::create_item(
        conn,
        &ItemInput {
            kind: Some("reading".into()),
            title: format!("阅读：{}", paper.title),
            body: paper.authors.clone().or_else(|| paper.abstract_text.clone()),
            status: Some("open".into()),
            priority: Some("med".into()),
            due_ms,
            start_ms: None,
            estimate_min: Some(60),
            parent_id: None,
            rrule: None,
            subject_id: paper.subject_id.clone(),
            topic_id: None,
            goal_id: None,
        },
    )?;
    workitems::link(conn, &item.id, "papers", paper_id, "context")?;
    conn.execute(
        "UPDATE papers SET status='reading', updated_at=?2 WHERE id=?1 AND status='to_read'",
        params![paper_id, now_ms()],
    )?;
    Ok(item)
}

// ------------------------------------------------------------------ BibTeX

fn bibtex_escape(s: &str) -> String {
    s.replace('\\', "\\textbackslash{}")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('&', "\\&")
        .replace('%', "\\%")
}

/// `Ashish Vaswani, Noam Shazeer` → `Vaswani2026Attention`.
fn cite_key(paper: &Paper) -> String {
    let first_surname = paper
        .authors
        .as_deref()
        .unwrap_or("")
        .split(',')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .last()
        .unwrap_or("anon")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();
    let year = paper
        .year
        .map(|y| y.to_string())
        .unwrap_or_else(|| "nd".into());
    let word = paper
        .title
        .split_whitespace()
        .find(|w| w.chars().any(|c| c.is_alphanumeric()) && w.len() > 3)
        .unwrap_or("paper")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();
    format!("{first_surname}{year}{word}")
}

pub fn export_bibtex(conn: &Connection, ids: &[String]) -> Result<String> {
    let papers = if ids.is_empty() {
        list_papers(conn, None, None, None)?
    } else {
        let mut out = Vec::new();
        for id in ids {
            out.push(get_paper(conn, id)?);
        }
        out
    };

    let mut bib = String::new();
    for p in &papers {
        // Conference vs journal: guess from the venue string, defaulting to
        // @article — a wrong entry type is a trivial fix, a missing entry is not.
        let lower = p.venue.as_deref().unwrap_or("").to_lowercase();
        let kind = if lower.contains("conference")
            || lower.contains("proceedings")
            || lower.contains("workshop")
            || lower.contains("symposium")
        {
            "inproceedings"
        } else {
            "article"
        };
        let venue_field = if kind == "inproceedings" {
            "booktitle"
        } else {
            "journal"
        };

        bib.push_str(&format!("@{kind}{{{},\n", cite_key(p)));
        bib.push_str(&format!("  title     = {{{}}},\n", bibtex_escape(&p.title)));
        if let Some(a) = &p.authors {
            let joined = a
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" and ");
            bib.push_str(&format!("  author    = {{{}}},\n", bibtex_escape(&joined)));
        }
        if let Some(y) = p.year {
            bib.push_str(&format!("  year      = {{{y}}},\n"));
        }
        if let Some(v) = &p.venue {
            bib.push_str(&format!("  {venue_field} = {{{}}},\n", bibtex_escape(v)));
        }
        if let Some(d) = &p.doi {
            bib.push_str(&format!("  doi       = {{{}}},\n", bibtex_escape(d)));
        }
        if let Some(u) = &p.url {
            bib.push_str(&format!("  url       = {{{}}},\n", bibtex_escape(u)));
        }
        bib.push_str("}\n\n");
    }
    Ok(bib.trim_end().to_string())
}

struct BibEntry {
    fields: Vec<(String, String)>,
}

/// Minimal BibTeX reader: enough for entries exported by Zotero / Google Scholar
/// / arXiv. It deliberately ignores macros, `@string`, crossrefs and LaTeX
/// accents — a parser that silently mangles is worse than one that is honest
/// about its scope.
fn parse_bibtex(text: &str) -> Vec<BibEntry> {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != '@' {
            i += 1;
            continue;
        }
        i += 1;
        // entry type
        while i < bytes.len() && bytes[i].is_whitespace() {
            i += 1;
        }
        let mut kind = String::new();
        while i < bytes.len() && (bytes[i].is_alphanumeric() || bytes[i] == '_') {
            kind.push(bytes[i]);
            i += 1;
        }
        if kind.eq_ignore_ascii_case("comment") || kind.eq_ignore_ascii_case("string") {
            // Skip to the matching close brace.
            i = skip_balanced(&bytes, i).unwrap_or(bytes.len());
            continue;
        }
        while i < bytes.len() && bytes[i] != '{' && bytes[i] != '(' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let close = if bytes[i] == '{' { '}' } else { ')' };
        let body_start = i + 1;
        let body_end = match matching(&bytes, body_start, close) {
            Some(e) => e,
            None => break,
        };
        let body: String = bytes[body_start..body_end].iter().collect();
        if let Some(entry) = parse_entry_body(&body) {
            out.push(entry);
        }
        i = body_end + 1;
    }
    out
}

fn skip_balanced(bytes: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() && bytes[i] != '{' {
        i += 1;
    }
    matching(bytes, i + 1, '}').map(|e| e + 1)
}

fn matching(bytes: &[char], from: usize, close: char) -> Option<usize> {
    let open = if close == '}' { '{' } else { '(' };
    let mut depth = 1usize;
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == open {
            depth += 1;
        } else if bytes[i] == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn parse_entry_body(body: &str) -> Option<BibEntry> {
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0usize;
    // citation key up to the first top-level comma
    while i < chars.len() && chars[i] != ',' {
        i += 1;
    }
    i += 1;

    let mut fields = Vec::new();
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        let mut name = String::new();
        while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-') {
            name.push(chars[i]);
            i += 1;
        }
        if name.is_empty() {
            break;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            break;
        }
        i += 1;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let value = match chars[i] {
            '{' => {
                let end = matching(&chars, i + 1, '}')?;
                let v: String = chars[i + 1..end].iter().collect();
                i = end + 1;
                v
            }
            '"' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '"' {
                    j += 1;
                }
                let v: String = chars[start..j.min(chars.len())].iter().collect();
                i = (j + 1).min(chars.len());
                v
            }
            _ => {
                let start = i;
                while i < chars.len() && chars[i] != ',' {
                    i += 1;
                }
                chars[start..i].iter().collect()
            }
        };
        fields.push((name.to_lowercase(), clean_latex(&value)));
    }
    if fields.is_empty() {
        None
    } else {
        Some(BibEntry { fields })
    }
}

/// The handful of accents that actually appear in author names and titles.
/// Anything outside this table loses its accent rather than turning into noise.
fn accented(mark: char, letter: char) -> Option<char> {
    let idx = |base: &str| base.chars().position(|c| c == letter);
    match mark {
        '"' => idx("aeiouyAEIOUY").map(|i| "äëïöüÿÄËÏÖÜŸ".chars().nth(i).unwrap()),
        '\'' => idx("aeiouyAEIOUY").map(|i| "áéíóúýÁÉÍÓÚÝ".chars().nth(i).unwrap()),
        '`' => idx("aeiouAEIOU").map(|i| "àèìòùÀÈÌÒÙ".chars().nth(i).unwrap()),
        '^' => idx("aeiouAEIOU").map(|i| "âêîôûÂÊÎÔÛ".chars().nth(i).unwrap()),
        '~' => idx("anoANO").map(|i| "ãñõÃÑÕ".chars().nth(i).unwrap()),
        _ => None,
    }
}

/// Strip the handful of LaTeX constructs that would otherwise leak into titles
/// (`{DNA}`, `\'e`, `~`). Not a TeX engine — just enough that the list is legible.
fn clean_latex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' => {}
            '~' => out.push(' '),
            '\\' => {
                // Accent commands: \'e \`a \"o \^i \~n
                if let Some(next) = chars.peek().copied() {
                    if "'`\"^~.".contains(next) {
                        chars.next();
                        if next == '.' {
                            continue; // \. is a dot accent — the letter alone is fine
                        }
                        if let Some(letter) = chars.next() {
                            out.push(accented(next, letter).unwrap_or(letter));
                        }
                        continue;
                    }
                }
                // Named commands: keep the word, drop the backslash.
                while let Some(next) = chars.peek().copied() {
                    if next.is_alphanumeric() {
                        out.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    // Collapse the whitespace left behind by brace removal.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Last, First and Last2, First2` → `First Last, First2 Last2`.
fn normalise_authors(raw: &str) -> String {
    raw.split(" and ")
        .map(|a| {
            let a = a.trim();
            if let Some((last, first)) = a.split_once(',') {
                format!("{} {}", first.trim(), last.trim())
            } else {
                a.to_string()
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn import_bibtex(conn: &Connection, text: &str) -> Result<ImportOutcome> {
    let entries = parse_bibtex(text);
    if entries.is_empty() {
        return Err(Error::Other(
            "no BibTeX entries found — expected lines like @article{key, title={...}}".into(),
        ));
    }
    let mut imported = 0i64;
    let mut skipped = 0i64;
    let mut titles = Vec::new();

    for entry in entries {
        let field = |name: &str| {
            entry
                .fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        let Some(title) = field("title").filter(|t| !t.trim().is_empty()) else {
            skipped += 1;
            continue;
        };
        let authors = field("author").map(|a| normalise_authors(&a));
        let year = field("year").and_then(|y| {
            let digits: String = y.chars().filter(|c| c.is_ascii_digit()).collect();
            digits.get(0..4).and_then(|d| d.parse::<i64>().ok())
        });
        let venue = field("journal").or_else(|| field("booktitle")).or_else(|| field("publisher"));
        let doi = field("doi");
        let url = field("url");
        let abstract_text = field("abstract");

        let before = normalise_doi(doi.as_deref())
            .and_then(|d| find_by_doi(conn, &d).ok())
            .map(|p| p.id);

        let paper = import_paper(
            conn,
            &PaperInput {
                title: title.clone(),
                authors,
                year,
                venue,
                doi,
                url,
                abstract_text,
                subject_id: None,
                source_id: None,
            },
        )?;
        if before.is_some() {
            skipped += 1;
        } else {
            imported += 1;
            titles.push(paper.title);
        }
    }

    Ok(ImportOutcome {
        imported,
        skipped,
        titles,
    })
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;

    fn state() -> AppState {
        AppState::in_memory().unwrap()
    }

    fn paper(title: &str, doi: Option<&str>) -> PaperInput {
        PaperInput {
            title: title.into(),
            authors: Some("Ashish Vaswani, Noam Shazeer".into()),
            year: Some(2017),
            venue: Some("NeurIPS".into()),
            doi: doi.map(str::to_string),
            url: None,
            abstract_text: Some("The dominant sequence transduction models...".into()),
            subject_id: None,
            source_id: None,
        }
    }

    #[test]
    fn importing_the_same_doi_twice_returns_the_same_row() {
        let st = state();
        let c = st.db.lock().unwrap();
        let a = import_paper(&c, &paper("Attention Is All You Need", Some("10.5555/3295222"))).unwrap();
        // Case and resolver prefixes must not create a second row.
        let b = import_paper(
            &c,
            &paper("Attention is all you need", Some("https://doi.org/10.5555/3295222")),
        )
        .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(list_papers(&c, None, None, None).unwrap().len(), 1);
    }

    #[test]
    fn papers_without_doi_are_not_deduplicated() {
        let st = state();
        let c = st.db.lock().unwrap();
        import_paper(&c, &paper("Preprint A", None)).unwrap();
        import_paper(&c, &paper("Preprint A", None)).unwrap();
        assert_eq!(list_papers(&c, None, None, None).unwrap().len(), 2);
    }

    #[test]
    fn status_moves_to_reading_when_an_excerpt_is_taken() {
        let st = state();
        let c = st.db.lock().unwrap();
        let p = import_paper(&c, &paper("Attention", Some("10.1/x"))).unwrap();
        assert_eq!(p.status, "to_read");

        let e = add_excerpt(&c, Some(&p.id), None, "Scaled dot-product attention", Some(4), None, None).unwrap();
        assert_eq!(e.quote, "Scaled dot-product attention");

        let reloaded = get_paper(&c, &p.id).unwrap();
        assert_eq!(reloaded.status, "reading");
        assert_eq!(reloaded.excerpt_count, 1);
    }

    #[test]
    fn excerpts_require_a_parent_and_a_quote() {
        let st = state();
        let c = st.db.lock().unwrap();
        assert!(add_excerpt(&c, None, None, "orphan", None, None, None).is_err());
        let p = import_paper(&c, &paper("X", None)).unwrap();
        assert!(add_excerpt(&c, Some(&p.id), None, "   ", None, None, None).is_err());
    }

    #[test]
    fn start_reading_creates_a_linked_task() {
        let st = state();
        let c = st.db.lock().unwrap();
        let p = import_paper(&c, &paper("Attention", Some("10.1/y"))).unwrap();
        let item = start_reading(&c, &p.id, None).unwrap();
        assert_eq!(item.kind, "reading");
        assert!(item.title.contains("Attention"));

        let links = workitems::links(&c, &item.id).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].entity_table, "papers");

        // The reverse lookup is what a paper card shows as "1 open task".
        let reverse = workitems::items_for_entity(&c, "papers", &p.id).unwrap();
        assert_eq!(reverse.len(), 1);
        assert_eq!(get_paper(&c, &p.id).unwrap().open_task_count, 1);
    }

    #[test]
    fn bibtex_roundtrip_preserves_the_essentials() {
        let st = state();
        let c = st.db.lock().unwrap();
        import_paper(&c, &paper("Attention Is All You Need", Some("10.5555/3295222"))).unwrap();
        let bib = export_bibtex(&c, &[]).unwrap();
        assert!(bib.contains("@article{"));
        assert!(bib.contains("title     = {Attention Is All You Need}"));
        assert!(bib.contains("author    = {Ashish Vaswani and Noam Shazeer}"));
        assert!(bib.contains("doi       = {10.5555/3295222}"));

        // Re-importing the export into an empty DB reproduces the paper.
        let st2 = state();
        let c2 = st2.db.lock().unwrap();
        let out = import_bibtex(&c2, &bib).unwrap();
        assert_eq!(out.imported, 1);
        let p = list_papers(&c2, None, None, None).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].year, Some(2017));
        assert_eq!(p[0].doi.as_deref(), Some("10.5555/3295222"));
        assert_eq!(p[0].authors.as_deref(), Some("Ashish Vaswani, Noam Shazeer"));
    }

    #[test]
    fn bibtex_import_skips_duplicates_and_handles_messy_fields() {
        let st = state();
        let c = st.db.lock().unwrap();
        let bib = r#"
@inproceedings{vaswani2017,
  title = {Attention {Is} All You Need},
  author = {Vaswani, Ashish and Shazeer, Noam},
  year = {2017},
  booktitle = {Advances in Neural Information Processing Systems},
  doi = {10.5555/3295222}
}
@article{dup,
  title = {Attention Is All You Need},
  doi = {10.5555/3295222}
}
"#;
        let out = import_bibtex(&c, bib).unwrap();
        assert_eq!(out.imported, 1);
        assert_eq!(out.skipped, 1, "same DOI in the same batch is a duplicate");
        let p = list_papers(&c, None, None, None).unwrap();
        assert_eq!(p[0].title, "Attention Is All You Need");
        assert_eq!(p[0].authors.as_deref(), Some("Ashish Vaswani, Noam Shazeer"));
        assert_eq!(p[0].venue.as_deref(), Some("Advances in Neural Information Processing Systems"));
    }

    #[test]
    fn latex_accents_and_braces_are_cleaned() {
        assert_eq!(clean_latex("Schr\\\"odinger's {Cat}"), "Schrödinger's Cat");
        assert_eq!(clean_latex("A~B"), "A B");
    }

    #[test]
    fn list_filters_by_status_and_search() {
        let st = state();
        let c = st.db.lock().unwrap();
        let a = import_paper(&c, &paper("Attention", Some("10.1/a"))).unwrap();
        import_paper(&c, &paper("BERT", Some("10.1/b"))).unwrap();
        update_paper(&c, &a.id, &PaperPatch { status: Some("read".into()), rating: Some(5), notes: None, subject_id: None, source_id: None }).unwrap();

        assert_eq!(list_papers(&c, Some("read"), None, None).unwrap().len(), 1);
        assert_eq!(list_papers(&c, Some("to_read"), None, None).unwrap().len(), 1);
        assert_eq!(list_papers(&c, None, None, Some("bert")).unwrap().len(), 1);
        assert!(list_papers(&c, Some("nonsense"), None, None).is_err());
    }
}
// ======================================================================
// B4: cross-paper comparison matrix
// ======================================================================

/// Default comparison rows when the caller does not specify their own.
pub const DEFAULT_MATRIX_ROWS: &[&str] = &[
    "方法 / Approach",
    "数据 / Data",
    "结论 / Findings",
    "局限 / Limitations",
];

#[derive(Debug, Clone, Serialize)]
pub struct MatrixRowData {
    pub label: String,
    pub cells: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaperMatrix {
    pub columns: Vec<String>,
    pub rows: Vec<MatrixRowData>,
    pub generated_by: String,
    pub generated_at: i64,
}

pub fn collect_paper_card(conn: &Connection, paper_id: &str) -> Result<(Paper, String)> {
    let p = get_paper(conn, paper_id)?;
    let excerpts: Vec<Excerpt> = list_excerpts(conn, Some(paper_id))?;
    let mut card = String::new();
    card.push_str(&format!("# {}\n", p.title));
    if let Some(a) = &p.authors { card.push_str(&format!("authors: {a}\n")); }
    if let Some(y) = p.year { card.push_str(&format!("year: {y}\n")); }
    if let Some(v) = &p.venue { card.push_str(&format!("venue: {v}\n")); }
    if let Some(doi) = &p.doi { card.push_str(&format!("doi: {doi}\n")); }
    if let Some(abs) = &p.abstract_text {
        if !abs.trim().is_empty() {
            card.push_str("\n## abstract\n");
            card.push_str(abs);
            card.push('\n');
        }
    }
    if !excerpts.is_empty() {
        card.push_str("\n## excerpts\n");
        for e in &excerpts {
            card.push_str(&format!("- {}", e.quote.trim()));
            if let Some(pg) = e.page { card.push_str(&format!(" (p.{pg})")); }
            if let Some(n) = &e.note {
                if !n.trim().is_empty() { card.push_str(&format!(" - {n}")); }
            }
            card.push('\n');
        }
    }
    Ok((p, card))
}

pub fn build_matrix(
    app: AppHandle,
    paper_ids: Vec<String>,
    row_labels: Vec<String>,
) -> Result<PaperMatrix> {
    if paper_ids.is_empty() {
        return Err(Error::Other("select at least one paper".into()));
    }
    let rows: Vec<String> = if row_labels.is_empty() {
        DEFAULT_MATRIX_ROWS.iter().map(|s| s.to_string()).collect()
    } else { row_labels };

    let (cards, spec, keys) = {
        let state = app.state::<AppState>();
        let c = state.db.lock().unwrap();
        let mut cards = Vec::with_capacity(paper_ids.len());
        for id in &paper_ids {
            let (_paper, card) = collect_paper_card(&c, id)?;
            cards.push(card);
        }
        let spec = crate::commands::model_spec_for(&c, "cheatsheet");
        crate::commands::guard_offline_llm(&c, &spec)?;
        let keys = crate::commands::read_keys(&c)?;
        (cards, spec, keys)
    };

    let mut model = crate::llm::from_spec_or_any(&spec, &keys)
        .ok_or_else(|| Error::Other("No model configured - add an API key in Settings -> API keys".into()))?;
    {
        let state = app.state::<AppState>();
        let c = state.db.lock().unwrap();
        crate::commands::apply_budget(&mut model, &c, "cheatsheet");
    }

    let columns: Vec<String> = {
        let state = app.state::<AppState>();
        let c = state.db.lock().unwrap();
        let mut titles = Vec::with_capacity(paper_ids.len());
        for id in &paper_ids {
            titles.push(get_paper(&c, id)?.title);
        }
        titles
    };

    let mut out_rows: Vec<MatrixRowData> = Vec::with_capacity(rows.len());
    let mut generated_by = String::from("local cache");

    for label in rows {
        let user = format!(
            "PAPERS (in selection order, [1] is column 1):\n\n{}\n\n\
             ROW LABEL: {}\n\n\
             For EACH paper, in selection order, write a 1-3 sentence answer \
             grounded ONLY in that paper's abstract and excerpts. Quote verbatim \
             terms (dataset names, metric names, algorithm names) when the paper \
             uses them. NEVER invent facts beyond what the paper supplies; if \
             the paper is silent on this dimension, write an empty string and \
             the matrix view will render an em-dash.\n\n\
             Output JSON array of strings, in selection order, NO commentary, \
             NO markdown:",
            cards.iter().enumerate().map(|(i, c)| format!("[{i}] =====\n{c}")).collect::<Vec<_>>().join("\n\n"),
            label
        );
        let system = "You are a research synthesis assistant. You receive ONE row label (e.g. \"方法 / Approach\") and N paper cards numbered in selection order. For each paper, output ONE short cell answering the row label, grounded ONLY in that paper's abstract and excerpts. NEVER invent; if a paper does not address the dimension, return an empty string for that cell. Return a JSON array of N strings, in selection order, no markdown, no commentary. The output language follows the row label: if the row label is bilingual, match the second half (after the /).";
        let floor = 2048 + 1500 * paper_ids.len() as u32;
        model.set_max_tokens(floor.max(4096));
        let raw = model.complete(system, &user)?;
        generated_by = model.name();
        let parsed = match crate::llm::extract_json(&raw) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("[papers] matrix row unparseable, retrying");
                let raw2 = model.complete(system, &format!("{user}\n\nIMPORTANT: previous reply could not be parsed. Respond with ONLY the raw JSON array of N strings."))?;
                match crate::llm::extract_json(&raw2) {
                    Ok(v) => v,
                    Err(_) => {
                        let blanks = vec![String::new(); paper_ids.len()];
                        out_rows.push(MatrixRowData { label, cells: blanks });
                        continue;
                    }
                }
            }
        };
        let arr = parsed.as_array().cloned().unwrap_or_default();
        let mut cells: Vec<String> = Vec::with_capacity(paper_ids.len());
        for i in 0..paper_ids.len() {
            let txt = arr.get(i).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).unwrap_or_default();
            cells.push(txt);
        }
        out_rows.push(MatrixRowData { label, cells });
    }

    Ok(PaperMatrix { columns, rows: out_rows, generated_by, generated_at: now_ms() })
}

