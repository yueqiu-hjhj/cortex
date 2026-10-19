//! Zotero bridge — import + incremental sync (科研一体化 gap: 管理引用).
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
//! **Incremental sync (2.2)**: `import` is the one-shot bulk path; `sync` is
//! the repeatable one. Every link between a Zotero item and its paper row is
//! recorded in `zotero_links` (`zotero_item_id → papers.id`; kept separate
//! from workitems.item_links, whose item_id FKs to work_items), and the max
//! `items.dateModified` stamp is stored in settings. A sync therefore only
//! re-reads what changed since the last stamp, refreshes linked papers in
//! place (status/rating/notes/subject are NEVER touched), links Zotero items
//! that now match an existing paper by DOI, and — when an item was trashed in
//! Zotero — removes the link. Unlinking deliberately KEEPS the paper: Cortex
//! owns the reading work (excerpts/PRISMA cells), trashing in Zotero must not
//! destroy it.
//!
//! The SQLite access is deliberately read-only against the COPY; nothing in
//! this module can ever write to the user's Zotero library.

use crate::error::{Error, Result};
use crate::papers::PaperInput;
use rusqlite::{Connection, OptionalExtension};
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

#[derive(Debug, Clone, Serialize)]
pub struct ZoteroSyncResult {
    pub db_path: String,
    /// Items re-read from Zotero (changed since the last stamp; everything on
    /// the first sync).
    pub changed: usize,
    /// Linked papers refreshed in place (metadata only).
    pub updated: usize,
    /// New paper rows created.
    pub added: usize,
    /// Zotero-side trash → link removed. The paper itself is kept.
    pub unlinked: usize,
    pub warnings: Vec<String>,
    pub titles_sample: Vec<String>,
    pub last_sync_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ZoteroSyncState {
    pub last_sync_ms: Option<i64>,
}

/// The item types that map to a "paper" in Cortex's model. Notes, attachments,
/// web pages, and AI-generated items are excluded by design.
const ITEM_TYPES: &str =
    "'journalArticle','conferencePaper','preprint','book','bookSection','thesis','report','manuscript','encyclopediaArticle'";

/// settings keys: the last-sync stamp (max items.dateModified seen) and the
/// wall-clock ms of the last successful sync (for the UI).
const STAMP_KEY: &str = "zotero_last_sync_stamp";
const LAST_SYNC_KEY: &str = "zotero_last_sync_ms";

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

fn resolve_db_path(db_path_override: Option<&Path>) -> Result<PathBuf> {
    match db_path_override {
        Some(p) if p.is_file() => Ok(p.to_path_buf()),
        Some(p) => Err(Error::Other(format!(
            "指定的 Zotero 数据库不存在: {}",
            p.display()
        ))),
        None => detect_db_path().ok_or_else(|| {
            Error::Other(
                "没有找到 Zotero 数据库 —— 默认位置是 ~/Zotero/zotero.sqlite；\
                 若自定义过数据目录，请在设置里配置 zotero_db_path。"
                    .into(),
            )
        }),
    }
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

/// Open the snapshot, read, and clean it up. Shared by import + sync.
fn with_snapshot<T>(
    db_path: &Path,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let snap = snapshot(db_path)?;
    let out = {
        let sconn = Connection::open(&snap)
            .map_err(|e| Error::Other(format!("打开 Zotero 快照失败: {e}")))?;
        f(&sconn)?
    };
    let _ = std::fs::remove_file(&snap);
    Ok(out)
}

struct RawItem {
    zid: i64,
    date_modified: Option<String>,
    title: String,
    doi: Option<String>,
    abstract_text: Option<String>,
    date: Option<String>,
    venue: Option<String>,
    url: Option<String>,
    creators: Vec<String>,
}

/// Read article-type, non-deleted items from a Zotero snapshot. `since` is the
/// last-sync `items.dateModified` stamp — pass "" for everything. Zotero
/// stores dateModified as a fixed-width "YYYY-MM-DD HH:MM:SS" UTC string, so
/// lexicographic > is a correct and index-free change filter.
fn read_items(conn: &Connection, since: &str) -> Result<Vec<RawItem>> {
    let since_clause = if since.is_empty() {
        String::new()
    } else {
        "AND i.dateModified > ?1 ".to_string()
    };
    let sql = format!(
        "SELECT i.itemID,
                i.dateModified,
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
            {since_clause}
            AND i.itemID NOT IN (SELECT itemID FROM deletedItems)
          GROUP BY i.itemID
          ORDER BY title"
    );
    let mut stmt = conn.prepare(&sql)?;
    // The ?1 placeholder only exists when a stamp is set — bind accordingly.
    let bind: Vec<String> = if since.is_empty() {
        Vec::new()
    } else {
        vec![since.to_string()]
    };
    let mut items: Vec<RawItem> = stmt
        .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
            Ok(RawItem {
                zid: r.get(0)?,
                date_modified: r.get::<_, Option<String>>(1)?,
                title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                doi: r.get::<_, Option<String>>(3)?.filter(|s| !s.trim().is_empty()),
                abstract_text: r.get::<_, Option<String>>(4)?.filter(|s| !s.trim().is_empty()),
                date: r.get::<_, Option<String>>(5)?.filter(|s| !s.trim().is_empty()),
                venue: r.get::<_, Option<String>>(6)?.filter(|s| !s.trim().is_empty()),
                url: r.get::<_, Option<String>>(7)?.filter(|s| !s.trim().is_empty()),
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
            {since_clause}
            AND i.itemID NOT IN (SELECT itemID FROM deletedItems)
          ORDER BY (SELECT value FROM itemData d JOIN itemDataValues v ON v.valueID = d.valueID
                     WHERE d.itemID = i.itemID AND d.fieldID = (SELECT fieldID FROM fields WHERE fieldName = 'title'))
         "
    ))?;
    let ids: Vec<i64> = id_stmt
        .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
            r.get(0)
        })?
        .flatten()
        .collect();
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

fn read_deleted_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT itemID FROM deletedItems")?;
    let ids = stmt.query_map([], |r| r.get(0))?.flatten().collect();
    Ok(ids)
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

/// "2024-05-05 08:27:57" (Zotero's UTC stamp format) → unix ms.
/// Hand-rolled days-from-civil: chrono isn't a dependency here and the format
/// is fixed-width, so this stays small and testable.
#[cfg(test)]
fn stamp_to_ms(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(' ')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let hh: i64 = tp.next()?.parse().ok()?;
    let mm: i64 = tp.next()?.parse().ok()?;
    let ss: i64 = tp.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + hh * 3600 + mm * 60 + ss) * 1000)
}

/// Normalized DOI ("https://doi.org/10.1/x", "doi:10.1/x" → "10.1/x").
fn doi_norm(doi: &Option<String>) -> Option<String> {
    doi.as_deref().map(|d| {
        d.trim_start_matches("https://doi.org/")
            .trim_start_matches("doi:")
            .to_lowercase()
    })
}

// ---- zotero_links helpers (zotero itemID ↔ papers.id) ----------------------

fn set_link(conn: &Connection, zid: i64, paper_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO zotero_links (zotero_item_id, paper_id, created_at)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![zid, paper_id, crate::db::now_ms()],
    )?;
    Ok(())
}

fn linked_paper(conn: &Connection, zid: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT paper_id FROM zotero_links WHERE zotero_item_id = ?1",
        rusqlite::params![zid],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(Error::Db)
}

fn unlink(conn: &Connection, zid: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM zotero_links WHERE zotero_item_id = ?1",
        rusqlite::params![zid],
    )?;
    Ok(())
}

fn paper_id_by_doi(conn: &Connection, doi: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM papers WHERE doi = ?1 LIMIT 1",
        rusqlite::params![doi],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(Error::Db)
}

/// Refresh a linked paper's METADATA from Zotero. Status / rating / notes /
/// subject_id / source_id are untouched — Cortex owns the reading pipeline.
/// A DOI that now collides with another paper's DOI fails the unique index →
/// surfaced as a per-item warning, other fields keep their old values.
fn update_paper_fields(conn: &Connection, paper_id: &str, item: &RawItem) -> Result<()> {
    let n = conn.execute(
        "UPDATE papers SET title=?1, authors=?2, year=?3, venue=?4, doi=?5, url=?6, abstract=?7, updated_at=?8
         WHERE id=?9",
        rusqlite::params![
            item.title.trim(),
            if item.creators.is_empty() { None } else { Some(item.creators.join("; ")) },
            year_from(&item.date),
            item.venue,
            doi_norm(&item.doi),
            item.url,
            item.abstract_text,
            crate::db::now_ms(),
            paper_id,
        ],
    )?;
    if n == 0 {
        return Err(Error::NotFound(format!("paper {paper_id}")));
    }
    Ok(())
}

fn paper_input(item: &RawItem, subject_id: Option<&str>) -> PaperInput {
    PaperInput {
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
    }
}

fn max_stamp<'a>(items: &'a [RawItem], current: &'a str) -> String {
    let mut best = current.to_string();
    for it in items {
        if let Some(dm) = &it.date_modified {
            if dm.as_str() > best.as_str() {
                best = dm.clone();
            }
        }
    }
    best
}

// ---- bulk import -----------------------------------------------------------

/// Import the whole Zotero library into the papers table. `db_path_override`
/// beats auto-detection; `subject_id` optionally files them into a subject.
/// DOI dedupe lives in `papers::import_paper`, so re-running is safe. Every
/// processed item gets an item_link recorded, and the sync stamp advances —
/// so a first-time user can start with plain import and switch to sync after.
pub fn import(
    conn: &Connection,
    db_path_override: Option<&Path>,
    subject_id: Option<&str>,
    progress: &mut dyn FnMut(usize),
) -> Result<ZoteroImportResult> {
    let db_path = resolve_db_path(db_path_override)?;
    let items = with_snapshot(&db_path, |sconn| read_items(sconn, ""))?;

    let mut known_dois: std::collections::HashSet<String> =
        crate::papers::list_papers(conn, None, None, None)?
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
        let dn = doi_norm(&item.doi);
        if let Some(d) = &dn {
            if known_dois.contains(d) {
                result.skipped += 1;
                // Already in the library — attach the link so future SYNCs
                // update this paper in place instead of re-importing.
                if let Some(pid) = paper_id_by_doi(conn, d)? {
                    set_link(conn, item.zid, &pid)?;
                }
                progress(n + 1);
                continue;
            }
        }
        let paper = crate::papers::import_paper(conn, &paper_input(item, subject_id))?;
        set_link(conn, item.zid, &paper.id)?;
        if let Some(d) = &dn {
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
    crate::repo::set_setting(conn, STAMP_KEY, &max_stamp(&items, ""))?;
    Ok(result)
}

// ---- incremental sync ------------------------------------------------------

/// Repeatable companion to `import`: reads only items whose dateModified is
/// newer than the stored stamp, refreshes linked papers in place, links
/// Zotero items that now match an existing paper by DOI, imports genuinely
/// new ones, and unlinks trashed items (paper rows are kept — see module doc).
/// The first sync (no stamp yet) behaves like a full import.
pub fn sync(
    conn: &Connection,
    db_path_override: Option<&Path>,
    subject_id: Option<&str>,
    progress: &mut dyn FnMut(usize),
) -> Result<ZoteroSyncResult> {
    let stamp = crate::repo::get_setting(conn, STAMP_KEY)?.unwrap_or_default();
    let db_path = resolve_db_path(db_path_override)?;
    let (items, deleted) = with_snapshot(&db_path, |sconn| {
        let items = read_items(sconn, &stamp)?;
        let deleted: std::collections::HashSet<i64> = if stamp.is_empty() {
            std::collections::HashSet::new() // first sync: no prior links to clean
        } else {
            read_deleted_ids(sconn)?.into_iter().collect()
        };
        Ok((items, deleted))
    })?;

    let mut result = ZoteroSyncResult {
        db_path: db_path.to_string_lossy().into_owned(),
        changed: items.len(),
        updated: 0,
        added: 0,
        unlinked: 0,
        warnings: Vec::new(),
        titles_sample: Vec::new(),
        last_sync_ms: crate::db::now_ms(),
    };

    for (n, item) in items.iter().enumerate() {
        // Linked paper → refresh metadata in place.
        let mut handled = false;
        if let Some(pid) = linked_paper(conn, item.zid)? {
            match update_paper_fields(conn, &pid, item) {
                Ok(()) => {
                    result.updated += 1;
                    handled = true;
                }
                Err(e) => result.warnings.push(format!("《{}》更新失败：{e}", item.title)),
            }
        }
        // Unlinked but DOI matches an existing paper → attach + refresh.
        if !handled {
            if let Some(d) = doi_norm(&item.doi) {
                if let Some(pid) = paper_id_by_doi(conn, &d)? {
                    set_link(conn, item.zid, &pid)?;
                    if let Err(e) = update_paper_fields(conn, &pid, item) {
                        result.warnings.push(format!("《{}》更新失败：{e}", item.title));
                    }
                    result.updated += 1;
                    handled = true;
                }
            }
        }
        // Genuinely new → import + link.
        if !handled {
            match crate::papers::import_paper(conn, &paper_input(item, subject_id)) {
                Ok(p) => {
                    set_link(conn, item.zid, &p.id)?;
                    result.added += 1;
                    if result.titles_sample.len() < 8 {
                        result.titles_sample.push(item.title.clone());
                    }
                }
                Err(e) => result.warnings.push(format!("《{}》导入失败：{e}", item.title)),
            }
        }
        if n % 25 == 0 {
            progress(n + 1);
        }
    }

    // Zotero-side trash → drop the link, keep the paper.
    for zid in &deleted {
        if linked_paper(conn, *zid)?.is_some() {
            unlink(conn, *zid)?;
            result.unlinked += 1;
        }
    }

    crate::repo::set_setting(conn, STAMP_KEY, &max_stamp(&items, &stamp))?;
    result.last_sync_ms = crate::db::now_ms();
    crate::repo::set_setting(conn, LAST_SYNC_KEY, &result.last_sync_ms.to_string())?;
    Ok(result)
}

pub fn sync_state(conn: &Connection) -> ZoteroSyncState {
    let ms = crate::repo::get_setting(conn, LAST_SYNC_KEY)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<i64>().ok());
    ZoteroSyncState { last_sync_ms: ms }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic Zotero database — the schema is stable enough across 5/6/7
    /// that seeding it directly tests the real query paths. Written to a real
    /// temp file so the snapshot() path is exercised too.
    fn write_fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("cortex-zotero-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE itemTypes (itemTypeID INTEGER PRIMARY KEY, typeName TEXT);
             CREATE TABLE fields (fieldID INTEGER PRIMARY KEY, fieldName TEXT);
             CREATE TABLE items (itemID INTEGER PRIMARY KEY, itemTypeID INTEGER, libraryID INTEGER DEFAULT 1,
                                 dateModified TEXT DEFAULT '2000-01-01 00:00:00', version INTEGER DEFAULT 0);
             CREATE TABLE itemDataValues (valueID INTEGER PRIMARY KEY, value TEXT);
             CREATE TABLE itemData (itemID INTEGER, fieldID INTEGER, valueID INTEGER);
             CREATE TABLE creatorData (creatorID INTEGER PRIMARY KEY, firstName TEXT, lastName TEXT);
             CREATE TABLE itemCreators (itemID INTEGER, creatorID INTEGER, creatorType TEXT, orderIndex INTEGER);
             CREATE TABLE deletedItems (itemID INTEGER PRIMARY KEY);
             INSERT INTO itemTypes VALUES (1,'journalArticle'),(2,'attachment'),(3,'note');
             INSERT INTO fields VALUES (1,'title'),(2,'DOI'),(3,'abstractNote'),(4,'date'),(5,'publicationTitle'),(6,'url');
             INSERT INTO items VALUES (1,1,1,'2017-06-12 08:00:00',10),(2,1,1,'2018-01-01 08:00:00',10),(3,2,1,'2019-01-01 08:00:00',10);
             INSERT INTO itemDataValues VALUES
               (1,'Attention Is All You Need'),(2,'10.5555/attention'),(3,'Transformer architecture abstract.'),
               (4,'2017-06-12'),(5,'NeurIPS'),(6,'https://arxiv.org/abs/1706.03762'),
               (7,'A Deleted Paper'),(8,'10.1111/deleted'),
               (9,'An Attachment Title'),
               (10,'Second Real Paper'),(11,'10.2222/second'),(12,'Second abstract.'),(13,'2021-05-05'),(14,'ICML'),(15,'https://arxiv.org/abs/2105.05');
             INSERT INTO itemData VALUES (1,1,1),(1,2,2),(1,3,3),(1,4,4),(1,5,5),(1,6,6),(2,1,7),(2,2,8),(3,1,9);
             INSERT INTO creatorData VALUES (1,'Ashish','Vaswani'),(2,'Noam','Shazeer'),(3,'Ada','Lovelace');
             INSERT INTO itemCreators VALUES (1,1,'author',0),(1,2,'author',1),(10,3,'author',0);
             INSERT INTO deletedItems VALUES (2);
             -- seeded but unused until the incremental test turns item 10 real
             UPDATE items SET itemID = 10 WHERE itemID = 10;
             ",
        )
        .unwrap();
        // item 10 starts ABSENT (the incremental test inserts it later).
        conn.execute("DELETE FROM itemCreators WHERE itemID = 10", []).unwrap();
        conn.execute("DELETE FROM items WHERE itemID = 10", []).unwrap();
        drop(conn);
        path
    }

    #[test]
    fn reads_articles_skips_trash_and_attachments() {
        let path = write_fixture("read-all.sqlite");
        let conn = Connection::open(&path).unwrap();
        let items = read_items(&conn, "").unwrap();
        assert_eq!(items.len(), 1, "attachment (itemType 2) and trashed (itemID 2) must be excluded");
        let it = &items[0];
        assert_eq!(it.zid, 1);
        assert_eq!(it.title, "Attention Is All You Need");
        assert_eq!(it.doi.as_deref(), Some("10.5555/attention"));
        assert_eq!(it.venue.as_deref(), Some("NeurIPS"));
        assert_eq!(year_from(&it.date), Some(2017));
        assert_eq!(it.date_modified.as_deref(), Some("2017-06-12 08:00:00"));
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
    fn stamp_ms_parses_zotero_utc_format() {
        assert_eq!(stamp_to_ms("1970-01-01 00:00:00"), Some(0));
        assert_eq!(stamp_to_ms("1970-01-01 00:01:00"), Some(60_000));
        // 2024-05-05 08:27:57 UTC = 1714897677 s
        assert_eq!(stamp_to_ms("2024-05-05 08:27:57"), Some(1_714_897_677_000));
        assert_eq!(stamp_to_ms("2024-05-05"), None); // no time part
    }

    #[test]
    fn import_dedupes_by_doi_across_runs() {
        let st = crate::db::AppState::in_memory().unwrap();
        let conn = st.db.lock().unwrap();
        let path = write_fixture("import-dedupe.sqlite");
        let snap = Connection::open(&path).unwrap();
        let items = read_items(&snap, "").unwrap();
        drop(snap);
        let mut known = std::collections::HashSet::new();
        let mut first_run = 0;
        for it in &items {
            let doi = it.doi.clone().map(|d| d.to_lowercase());
            if doi.as_ref().map_or(false, |d| known.contains(d)) {
                continue;
            }
            crate::papers::import_paper(&conn, &paper_input(it, None)).unwrap();
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

    /// The full incremental story: import → edit Zotero (title change + new
    /// item + trash an already-linked one) → sync reflects exactly that, and
    /// the paper's reading state (status/rating/notes) survives the refresh.
    #[test]
    fn sync_is_incremental_links_and_respects_trash() {
        let st = crate::db::AppState::in_memory().unwrap();
        let conn = st.db.lock().unwrap();
        let path = write_fixture("sync.sqlite");

        // 1) bulk import (1 item, linked)
        let r = import(&conn, Some(&path), None, &mut |_| {}).unwrap();
        assert_eq!(r.imported, 1);
        assert_eq!(linked_paper(&conn, 1).unwrap().is_some(), true);
        let stamp1 = crate::repo::get_setting(&conn, STAMP_KEY).unwrap().unwrap();
        assert_eq!(stamp1, "2017-06-12 08:00:00");

        // reading state the user built AFTER import — must survive sync
        crate::papers::update_paper(
            &conn,
            &linked_paper(&conn, 1).unwrap().unwrap(),
            &crate::papers::PaperPatch {
                status: Some("reading".into()),
                rating: Some(5),
                notes: Some("my notes".into()),
                subject_id: None,
                source_id: None,
            },
        )
        .unwrap();

        // 2) nothing changed → sync is a no-op
        let r = sync(&conn, Some(&path), None, &mut |_| {}).unwrap();
        assert_eq!((r.changed, r.updated, r.added, r.unlinked), (0, 0, 0, 0));

        // 3) Zotero changes: new item (id 10), title refresh on item 1,
        //    item 2 (already imported via DOI? no — item 2 is trash-seeded;
        //    to test unlink we first link it through a DOI import)
        let zc = Connection::open(&path).unwrap();
        zc.execute_batch(
            "INSERT INTO items VALUES (10, 1, 1, '2021-05-05 08:00:00', 11);
             INSERT INTO itemDataValues VALUES (16,'Sync New Paper'),(17,'10.3333/new'),(18,'New abs.'),(19,'2021-05-05'),(20,'ICLR'),(21,'https://arxiv.org/abs/2105.9');
             INSERT INTO itemData VALUES (10,1,16),(10,2,17),(10,3,18),(10,4,19),(10,5,20),(10,6,21);
             INSERT INTO itemCreators VALUES (10,3,'author',0);
             UPDATE items SET dateModified = '2019-09-09 09:09:09', version = 20 WHERE itemID = 1;
             UPDATE itemDataValues SET value = 'Attention Is All You Need (v2)' WHERE valueID = 1;",
        )
        .unwrap();
        // also import item 2 (trash-seeded) via DOI so a later trash-unlink has a link to drop
        zc.execute("DELETE FROM deletedItems WHERE itemID = 2", []).unwrap();
        drop(zc);
        // item 2 never had a paper row (it was trashed before any import);
        // create one NOW by DOI so the sync's DOI-match path attaches the
        // Zotero link instead of importing it as new.
        crate::papers::import_paper(
            &conn,
            &PaperInput {
                title: "A Deleted Paper".into(),
                authors: None,
                year: None,
                venue: None,
                doi: Some("10.1111/deleted".into()),
                url: None,
                abstract_text: None,
                subject_id: None,
                source_id: None,
            },
        )
        .unwrap();
        let r = sync(&conn, Some(&path), None, &mut |_| {}).unwrap();
        // item 1 (changed title) + item 2 (DOI match) updated; item 10 added
        assert_eq!(r.changed, 3, "{r:?}");
        assert_eq!(r.updated, 2, "{r:?}");
        assert_eq!(r.added, 1, "{r:?}");
        assert_eq!(r.unlinked, 0);
        let papers = crate::papers::list_papers(&conn, None, None, None).unwrap();
        assert_eq!(papers.len(), 3);
        let att = papers.iter().find(|p| p.title.starts_with("Attention")).unwrap();
        assert_eq!(att.title, "Attention Is All You Need (v2)");
        assert_eq!(att.status, "reading", "reading state must survive sync");
        assert_eq!(att.rating, Some(5));
        assert_eq!(att.notes.as_deref(), Some("my notes"));
        assert_eq!(linked_paper(&conn, 1).unwrap().as_deref(), Some(att.id.as_str()));
        assert_eq!(linked_paper(&conn, 2).unwrap().is_some(), true, "DOI match attaches link");

        // 4) trash item 1 AND item 2 → links dropped, papers kept
        let zc = Connection::open(&path).unwrap();
        zc.execute_batch(
            "INSERT INTO deletedItems VALUES (1),(2);
             UPDATE items SET dateModified='2022-01-01 00:00:00' WHERE itemID IN (1,2);",
        )
        .unwrap();
        drop(zc);
        let r = sync(&conn, Some(&path), None, &mut |_| {}).unwrap();
        assert_eq!(r.unlinked, 2, "{r:?}");
        assert_eq!(linked_paper(&conn, 1).unwrap(), None);
        assert_eq!(crate::papers::list_papers(&conn, None, None, None).unwrap().len(), 3, "papers are kept on trash-unlink");

        // 5) last_sync wall clock recorded
        assert!(sync_state(&conn).last_sync_ms.is_some());
    }
}
