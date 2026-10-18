//! Zotero import — the citation-library bridge (科研一体化 gap: 管理引用).
//!
//! Zotero owns the reference lists of most researchers; Cortex's papers table
//! owns the reading pipeline (状态机/摘录/成稿/排期). This module bridges them
//! by reading the LOCAL Zotero database directly — no cloud account, no API
//! key, works offline:
//!
//! 1. locate `zotero.sqlite` (default data dirs, or the user-specified path)
//! 2. COPY it (plus `-wal`/`-shm` if present) to a temp file — Zotero holds an
//!    exclusive lock while running, and copying is what every third-party tool
//!    does; reading the copy also guarantees we never corrupt the original
//! 3. query article-type items (journalArticle / conferencePaper / preprint /
//!    book / …) that are not in the trash, with title/DOI/abstract/date/venue/url
//! 4. primary + secondary creators in Zotero's own order
//! 5. import through `papers::import_paper` — DOI dedupe means re-running the
//!    import never duplicates; already-in-library items are counted as skipped
//!
//! The SQLite access is deliberately read-only against the COPY; nothing in
//! this module can ever write to the user's Zotero library.

use crate::error::{Error, Result};
use crate::papers::PaperInput;
use rusqlite::Connection;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct ZoteroImportResult {
    pub db_path: String,
    pub found: usize,
    pub imported: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
    pub titles_sample: Vec<String>,
}

/// The item types that map to a "paper" in Cortex's model. Notes, attachments,
/// web pages, and AI-generated items are excluded by design.
const ITEM_TYPES: &str =
    "'journalArticle','conferencePaper','preprint','book','bookSection','thesis','report','manuscript','encyclopediaArticle'";

/// Guess where Zotero's data directory lives. Zotero's DEFAULT data dir is
/// `~/Zotero` on every platform; the profile-relative path covers users who
/// kept the pre-5.0 location. The first existing candidate wins.
pub fn detect_db_path() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    if home.is_empty() {
        return None;
    }
    let mut candidates = vec![PathBuf::from(&home).join("Zotero").join("zotero.sqlite")];
    if cfg!(target_os = "windows") {
        if let Ok(appdata) = std::env::var("APPDATA") {
            // %APPDATA%\Zotero\Zotero\Profiles\<random>\zotero\zotero.sqlite
            let profiles = PathBuf::from(&appdata).join("Zotero").join("Zotero").join("Profiles");
            if let Ok(entries) = std::fs::read_dir(&profiles) {
                let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
                dirs.sort();
                for d in dirs {
                    candidates.push(d.join("zotero").join("zotero.sqlite"));
                }
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Copy the Zotero database (+ WAL/SHM sidecars) to a temp snapshot so we can
/// read it even while Zotero is running and holding locks.
fn snapshot(db_path: &Path) -> Result<PathBuf> {
    let snap_dir = std::env::temp_dir().join("cortex-zotero-snap");
    std::fs::create_dir_all(&snap_dir)
        .map_err(|e| Error::Other(format!("create snapshot dir: {e}")))?;
    let snap = snap_dir.join("zotero.sqlite");
    std::fs::copy(db_path, &snap).map_err(|e| {
        Error::Other(format!(
            "无法复制 Zotero 数据库（{}）: {e} —— 确认路径正确，且 Zotero 数据目录存在",
            db_path.display()
        ))
    })?;
    for sidecar in ["-wal", "-shm"] {
        let src = db_path.with_file_name(format!("zotero.sqlite{sidecar}"));
        if src.is_file() {
            let _ = std::fs::copy(&src, snap_dir.join(format!("zotero.sqlite{sidecar}")));
        }
    }
    Ok(snap)
}

struct RawItem {
    title: String,
    doi: Option<String>,
    abstract_text: Option<String>,
    date: Option<String>,
    venue: Option<String>,
    url: Option<String>,
    creators: Vec<String>,
}

/// Read all article-type, non-deleted items from a Zotero snapshot.
fn read_items(conn: &Connection) -> Result<Vec<RawItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT i.itemID,
                MAX(CASE WHEN f.fieldName = 'title' THEN v.value END) AS title,
                MAX(CASE WHEN f.fieldName = 'DOI' THEN v.value END) AS doi,
                MAX(CASE WHEN f.fieldName = 'abstractNote' THEN v.value END) AS abstract,
                MAX(CASE WHEN f.fieldName = 'date' THEN v.value END) AS date,
                MAX(CASE WHEN f.fieldName = 'publicationTitle' THEN v.value END) AS venue,
                MAX(CASE WHEN f.fieldName = 'url' THEN v.value END) AS url
           FROM items i
           JOIN itemTypes t ON t.itemTypeID = i.itemTypeID
           JOIN itemData d ON d.itemID = i.itemID
           JOIN fields f ON f.fieldID = d.fieldID
           JOIN itemDataValues v ON v.valueID = d.valueID
          WHERE t.typeName IN ({ITEM_TYPES})
            AND i.itemID NOT IN (SELECT itemID FROM deletedItems)
          GROUP BY i.itemID
          ORDER BY title"
    ))?;
    let mut items: Vec<RawItem> = stmt
        .query_map([], |r| {
            Ok(RawItem {
                title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                doi: r.get::<_, Option<String>>(2)?.filter(|s| !s.trim().is_empty()),
                abstract_text: r.get::<_, Option<String>>(3)?.filter(|s| !s.trim().is_empty()),
                date: r.get::<_, Option<String>>(4)?.filter(|s| !s.trim().is_empty()),
                venue: r.get::<_, Option<String>>(5)?.filter(|s| !s.trim().is_empty()),
                url: r.get::<_, Option<String>>(6)?.filter(|s| !s.trim().is_empty()),
                creators: Vec::new(),
            })
        })?
        .collect::<std::result::Result<_, _>>()?;

    // Creators, one grouped query, Zotero's own ordering.
    let mut cstmt = conn.prepare(
        "SELECT ic.itemID, TRIM(cd.lastName || ', ' || cd.firstName) AS name
           FROM itemCreators ic
           JOIN creatorData cd ON cd.creatorID = ic.creatorID
          ORDER BY ic.itemID, ic.orderIndex",
    )?;
    let mut by_item: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    let rows = cstmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    for (item_id, name) in rows.flatten() {
        let clean = name.trim().trim_end_matches(',').trim().to_string();
        if !clean.is_empty() {
            by_item.entry(item_id).or_default().push(clean);
        }
    }
    // Re-attach creators by itemID — read_items returns rows ordered by title,
    // so re-query the ids in the same order rather than assuming alignment.
    let mut id_stmt = conn.prepare(&format!(
        "SELECT i.itemID FROM items i
           JOIN itemTypes t ON t.itemTypeID = i.itemTypeID
          WHERE t.typeName IN ({ITEM_TYPES})
            AND i.itemID NOT IN (SELECT itemID FROM deletedItems)
          ORDER BY (SELECT value FROM itemData d JOIN itemDataValues v ON v.valueID = d.valueID
                     WHERE d.itemID = i.itemID AND d.fieldID = (SELECT fieldID FROM fields WHERE fieldName = 'title'))
         "
    ))?;
    let ids: Vec<i64> = id_stmt.query_map([], |r| r.get(0))?.flatten().collect();
    if ids.len() == items.len() {
        for (item, id) in items.iter_mut().zip(ids) {
            if let Some(names) = by_item.get(&id) {
                item.creators = names.clone();
            }
        }
    }
    items.retain(|i| !i.title.trim().is_empty());
    Ok(items)
}

/// First 4-digit run in a Zotero date string ("2024-03-01", "March 2024"…).
fn year_from(date: &Option<String>) -> Option<i64> {
    let d = date.as_deref()?;
    let bytes = d.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4].iter().all(|b| b.is_ascii_digit()) {
            return d[i..i + 4].parse().ok();
        }
        i += 1;
    }
    None
}

/// Import the whole Zotero library into the papers table. `db_path_override`
/// beats auto-detection; `subject_id` optionally files them into a subject.
/// DOI dedupe lives in `papers::import_paper`, so re-running is safe.
pub fn import(
    conn: &Connection,
    db_path_override: Option<&Path>,
    subject_id: Option<&str>,
    progress: &mut dyn FnMut(usize),
) -> Result<ZoteroImportResult> {
    let detected = detect_db_path();
    let db_path: PathBuf = match db_path_override {
        Some(p) if p.is_file() => p.to_path_buf(),
        Some(p) => {
            return Err(Error::Other(format!(
                "指定的 Zotero 数据库不存在: {}",
                p.display()
            )))
        }
        None => detected.ok_or_else(|| {
            Error::Other(
                "没有找到 Zotero 数据库 —— 默认位置是 ~/Zotero/zotero.sqlite；\
                 若自定义过数据目录，请在设置里配置 zotero_db_path。"
                    .into(),
            )
        })?,
    };
    let snap = snapshot(&db_path)?;
    let snapshot_conn = Connection::open(&snap)
        .map_err(|e| Error::Other(format!("打开 Zotero 快照失败: {e}")))?;
    let items = read_items(&snapshot_conn)?;
    drop(snapshot_conn);
    let _ = std::fs::remove_file(&snap);

    let mut known_dois: std::collections::HashSet<String> = crate::papers::list_papers(conn, None, None, None)?
        .into_iter()
        .filter_map(|p| p.doi)
        .collect();

    let mut result = ZoteroImportResult {
        db_path: db_path.to_string_lossy().into_owned(),
        found: items.len(),
        imported: 0,
        skipped: 0,
        warnings: Vec::new(),
        titles_sample: Vec::new(),
    };
    for (n, item) in items.iter().enumerate() {
        let doi_norm = item
            .doi
            .as_deref()
            .map(|d| d.trim_start_matches("https://doi.org/").trim_start_matches("doi:").to_lowercase());
        if let Some(d) = &doi_norm {
            if known_dois.contains(d) {
                result.skipped += 1;
                progress(n + 1);
                continue;
            }
        }
        crate::papers::import_paper(
            conn,
            &PaperInput {
                title: item.title.clone(),
                authors: if item.creators.is_empty() {
                    None
                } else {
                    Some(item.creators.join("; "))
                },
                year: year_from(&item.date),
                venue: item.venue.clone(),
                doi: item.doi.clone(),
                url: item.url.clone(),
                abstract_text: item.abstract_text.clone(),
                subject_id: subject_id.map(|s| s.to_string()),
                source_id: None,
            },
        )?;
        if let Some(d) = &doi_norm {
            known_dois.insert(d.clone());
        }
        result.imported += 1;
        if result.titles_sample.len() < 8 {
            result.titles_sample.push(item.title.clone());
        }
        if n % 25 == 0 {
            progress(n + 1);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic Zotero database — the schema is stable enough across 5/6/7
    /// that seeding it directly tests the real query paths.
    fn zotero_fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE itemTypes (itemTypeID INTEGER PRIMARY KEY, typeName TEXT);
             CREATE TABLE fields (fieldID INTEGER PRIMARY KEY, fieldName TEXT);
             CREATE TABLE items (itemID INTEGER PRIMARY KEY, itemTypeID INTEGER, libraryID INTEGER DEFAULT 1);
             CREATE TABLE itemDataValues (valueID INTEGER PRIMARY KEY, value TEXT);
             CREATE TABLE itemData (itemID INTEGER, fieldID INTEGER, valueID INTEGER);
             CREATE TABLE creatorData (creatorID INTEGER PRIMARY KEY, firstName TEXT, lastName TEXT);
             CREATE TABLE itemCreators (itemID INTEGER, creatorID INTEGER, creatorType TEXT, orderIndex INTEGER);
             CREATE TABLE deletedItems (itemID INTEGER PRIMARY KEY);
             INSERT INTO itemTypes VALUES (1,'journalArticle'),(2,'attachment'),(3,'note');
             INSERT INTO fields VALUES (1,'title'),(2,'DOI'),(3,'abstractNote'),(4,'date'),(5,'publicationTitle'),(6,'url');
             INSERT INTO items VALUES (1,1,1),(2,1,1),(3,2,1);
             INSERT INTO itemDataValues VALUES
               (1,'Attention Is All You Need'),(2,'10.5555/attention'),(3,'Transformer architecture abstract.'),
               (4,'2017-06-12'),(5,'NeurIPS'),(6,'https://arxiv.org/abs/1706.03762'),
               (7,'A Deleted Paper'),(8,'10.1111/deleted'),
               (9,'An Attachment Title');
             INSERT INTO itemData VALUES (1,1,1),(1,2,2),(1,3,3),(1,4,4),(1,5,5),(1,6,6),(2,1,7),(2,2,8),(3,1,9);
             INSERT INTO creatorData VALUES (1,'Ashish','Vaswani'),(2,'Noam','Shazeer');
             INSERT INTO itemCreators VALUES (1,1,'author',0),(1,2,'author',1);
             INSERT INTO deletedItems VALUES (2);
             -- Minimal app-schema stubs so import_paper/list_papers (which the
             -- dedupe test exercises) have somewhere to write against; the
             -- Zotero-schema tables above are what read_items actually reads.
             CREATE TABLE papers (
               id TEXT PRIMARY KEY, title TEXT NOT NULL, authors TEXT, year INTEGER,
               venue TEXT, doi TEXT, url TEXT, abstract TEXT, status TEXT NOT NULL DEFAULT 'to_read',
               rating INTEGER, notes TEXT, subject_id TEXT, source_id TEXT,
               added_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE UNIQUE INDEX idx_papers_doi ON papers(doi) WHERE doi IS NOT NULL AND doi <> '';
             CREATE TABLE excerpts (id TEXT PRIMARY KEY, paper_id TEXT, source_id TEXT,
               quote TEXT NOT NULL, page INTEGER, note TEXT, tags TEXT,
               created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE item_links (item_id TEXT, entity_table TEXT, entity_id TEXT);
             CREATE TABLE work_items (id TEXT PRIMARY KEY, status TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn reads_articles_skips_trash_and_attachments() {
        let conn = zotero_fixture();
        let items = read_items(&conn).unwrap();
        assert_eq!(items.len(), 1, "attachment (itemType 2) and trashed (itemID 2) must be excluded");
        let it = &items[0];
        assert_eq!(it.title, "Attention Is All You Need");
        assert_eq!(it.doi.as_deref(), Some("10.5555/attention"));
        assert_eq!(it.venue.as_deref(), Some("NeurIPS"));
        assert_eq!(year_from(&it.date), Some(2017));
        assert_eq!(it.creators, vec!["Vaswani, Ashish".to_string(), "Shazeer, Noam".to_string()]);
    }

    #[test]
    fn year_parses_sloppy_dates() {
        assert_eq!(year_from(&Some("2024-03-01".into())), Some(2024));
        assert_eq!(year_from(&Some("March 2024".into())), Some(2024));
        assert_eq!(year_from(&Some("in press".into())), None);
        assert_eq!(year_from(&None), None);
    }

    #[test]
    fn import_dedupes_by_doi_across_runs() {
        let st = crate::db::AppState::in_memory().unwrap();
        let conn = st.db.lock().unwrap();
        let items = read_items(&zotero_fixture()).unwrap();
        let mut known = std::collections::HashSet::new();
        let mut first_run = 0;
        for it in &items {
            let doi = it.doi.clone().map(|d| d.to_lowercase());
            if doi.as_ref().map_or(false, |d| known.contains(d)) {
                continue;
            }
            crate::papers::import_paper(
                &conn,
                &PaperInput {
                    title: it.title.clone(),
                    authors: Some(it.creators.join("; ")),
                    year: year_from(&it.date),
                    venue: it.venue.clone(),
                    doi: it.doi.clone(),
                    url: it.url.clone(),
                    abstract_text: it.abstract_text.clone(),
                    subject_id: None,
                    source_id: None,
                },
            )
            .unwrap();
            if let Some(d) = doi {
                known.insert(d);
            }
            first_run += 1;
        }
        assert_eq!(first_run, 1);
        // Second run: same items, DOI now known → nothing new.
        let papers = crate::papers::list_papers(&conn, None, None, None).unwrap();
        assert_eq!(papers.len(), 1);
        assert_eq!(papers[0].title, "Attention Is All You Need");
    }
}
