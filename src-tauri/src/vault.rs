//! Vault layer (P1): notes live as Markdown files on disk; SQLite is the index.
//!
//! Every workflow that promises "AI writes my notes" — Obsidian, Codex, Claude
//! Code, plain `git` — speaks *files*, not SQLite. A note that exists only in
//! `cortex.db` is invisible to all of them, so this module inverts the storage
//! relationship: the `.md` file becomes the source of truth and the `notes`
//! table becomes a rebuildable cache.
//!
//! Layout (inside the configured vault root):
//! ```text
//! <vault>/notes/<title>.md   one file per note: YAML frontmatter + body
//! <vault>/inbox/             AI drafts awaiting triage (P4)
//! <vault>/moc/               maps of content (P4)
//! <vault>/.cortex/           vault metadata written by this module
//! ```
//!
//! Frontmatter keys are deliberately a small, hand-parsed subset of YAML —
//! `key: scalar` per line. No `serde_yaml`: the format is tiny, the parser is
//! ~60 lines, and we avoid a dependency that only ever reads six fields.
//!
//! Deliberate scope for P1: writes go DB -> file on every save, and
//! [`reindex`] pulls the whole vault back DB-side at startup. Live file
//! watching (an edit made inside Obsidian while the app is open) is P2; until
//! then such an edit is picked up on the next launch or explicit reindex.
//!
//! Deletions are one-directional on purpose: a note whose file vanished keeps
//! its DB row with `vault_path = NULL`. Losing a file to a sync bug must never
//! silently destroy the user's notes.

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use crate::models::Note;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// Setting key holding an absolute vault path. Absent/empty => the default
/// `<app data>/vault` location.
pub const SETTING_VAULT_PATH: &str = "vault_path";

/// Bumped when the on-disk layout changes in a way the indexer must notice.
const LAYOUT_VERSION: &str = "1";

/// Directories that hold app/vault metadata rather than notes: never indexed.
const SKIP_DIRS: &[&str] = &[".obsidian", ".trash", ".git", ".cortex", "node_modules"];

/// Resolve the vault root. `configured` wins when non-empty (this is how an
/// existing Obsidian vault gets adopted); otherwise the app-private default is
/// used. Returns `None` only for the in-memory test DB with no data dir.
pub fn resolve_root(configured: Option<&str>, data_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = configured.map(str::trim).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    data_dir.map(|d| d.join("vault"))
}

/// Read the configured root out of `settings` and resolve it against the app
/// data dir. Every command path goes through here so there is one definition
/// of "where the vault is".
pub fn root_for(conn: &Connection, data_dir: Option<&Path>) -> Option<PathBuf> {
    let configured = crate::repo::get_setting(conn, SETTING_VAULT_PATH)
        .ok()
        .flatten();
    resolve_root(configured.as_deref(), data_dir)
}

/// Create the directory skeleton and stamp the layout version. Idempotent.
pub fn ensure_layout(root: &Path) -> Result<()> {
    for sub in ["notes", "inbox", "moc", ".cortex"] {
        std::fs::create_dir_all(root.join(sub))?;
    }
    let marker = root.join(".cortex").join("layout");
    if std::fs::read_to_string(&marker).unwrap_or_default().trim() != LAYOUT_VERSION {
        std::fs::write(&marker, LAYOUT_VERSION)?;
    }
    Ok(())
}

/// One note as it exists in a file (or as it should be rendered into one).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NoteFile {
    pub id: String,
    pub title: String,
    pub body: String,
    pub subject: Option<String>,
    pub topic: Option<String>,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    /// Frontmatter lines this module does not interpret — `tags`, `aliases`,
    /// anything an Obsidian plugin owns. Kept verbatim and written back, so
    /// stamping `cortex_id` into a foreign file never strips the user's keys.
    pub extra: Vec<String>,
}

/// Strip characters Windows forbids in filenames, collapse whitespace, cap the
/// length. Chinese titles survive intact — the slug is only a filename, never
/// an identifier (that is `cortex_id`).
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        match ch {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' | '\t' => {
                out.push('-')
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    let trimmed = out.trim().trim_matches('.').trim();
    let capped: String = trimmed.chars().take(80).collect();
    let capped = capped.trim().to_string();
    if capped.is_empty() {
        "untitled".to_string()
    } else {
        capped
    }
}

/// Quote a scalar for the frontmatter when it would otherwise be ambiguous
/// (contains `:`, starts with a YAML indicator, has surrounding spaces, ...).
fn scalar(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    let needs_quote = value.chars().any(|c| matches!(c, ':' | '#' | '"' | '\''))
        || value.chars().any(|c| c.is_control())
        || value.trim() != value
        || value.starts_with(['-', '@', '`', '[', ']', '{', '}', '*', '&', '!', '%', '>', '|']);
    if !needs_quote {
        return value.to_string();
    }
    let mut o = String::with_capacity(value.len() + 2);
    o.push('"');
    for c in value.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut o = String::with_capacity(inner.len());
        let mut esc = false;
        for c in inner.chars() {
            if esc {
                o.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    other => other,
                });
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else {
                o.push(c);
            }
        }
        return o;
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}

/// RFC 3339 in UTC, second precision, for frontmatter timestamps.
fn iso8601(ms: i64) -> String {
    let fallback = "1970-01-01T00:00:00Z".to_string();
    let Ok(ts) = time::OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000) else {
        return fallback;
    };
    ts.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or(fallback)
}

/// Render a note as `---\nfrontmatter\n---\n\nbody`.
pub fn render(note: &NoteFile) -> String {
    let mut s = String::with_capacity(note.body.len() + 256);
    s.push_str("---\n");
    s.push_str(&format!("cortex_id: {}\n", scalar(&note.id)));
    s.push_str(&format!("title: {}\n", scalar(&note.title)));
    if let Some(subject) = note.subject.as_deref().filter(|s| !s.trim().is_empty()) {
        s.push_str(&format!("subject: {}\n", scalar(subject)));
    }
    if let Some(topic) = note.topic.as_deref().filter(|s| !s.trim().is_empty()) {
        s.push_str(&format!("topic: {}\n", scalar(topic)));
    }
    if let Some(created) = note.created {
        s.push_str(&format!("created: {}\n", iso8601(created)));
    }
    if let Some(updated) = note.updated {
        s.push_str(&format!("updated: {}\n", iso8601(updated)));
    }
    for line in &note.extra {
        s.push_str(line);
        s.push('\n');
    }
    s.push_str("---\n\n");
    s.push_str(note.body.trim_end_matches(['\n', '\r']));
    s.push('\n');
    s
}

/// Split `---` frontmatter from the body and read the keys we understand.
/// Files without frontmatter parse to a `NoteFile` with an empty id — the
/// indexer adopts those by title or creates a fresh note.
pub fn parse(text: &str) -> NoteFile {
    let mut out = NoteFile::default();
    let mut rest = text;
    if let Some(stripped) = text.strip_prefix("---") {
        // The opening fence must be followed by a newline, not be `----`.
        let after_fence = stripped.strip_prefix('\n').or_else(|| stripped.strip_prefix("\r\n"));
        if let Some(after_fence) = after_fence {
            if let Some(end) = find_fence_end(after_fence) {
                for line in after_fence[..end].lines() {
                    let Some((key, value)) = line.split_once(':') else {
                        // Not `key: value` (a bare `- item` list entry, a stray
                        // comment): keep it so the round-trip is lossless.
                        if !line.trim().is_empty() {
                            out.extra.push(line.to_string());
                        }
                        continue;
                    };
                    let value = unquote(value);
                    match key.trim() {
                        "cortex_id" => out.id = value.trim().to_string(),
                        "title" => out.title = value,
                        "subject" => out.subject = Some(value).filter(|v| !v.is_empty()),
                        "topic" => out.topic = Some(value).filter(|v| !v.is_empty()),
                        // Re-written from the DB on every save; reading them back
                        // would only fight the file's mtime.
                        "created" | "updated" => {}
                        _ => out.extra.push(line.to_string()),
                    }
                }
                let body_start = after_fence[end..]
                    .find('\n')
                    .map(|i| end + i + 1)
                    .unwrap_or(after_fence.len());
                rest = &after_fence[body_start..];
            }
        }
    }
    out.body = rest.trim_start_matches(['\n', '\r']).to_string();
    out
}

/// Byte offset of the closing `---` fence within `text`.
fn find_fence_end(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// Absolute path for a note inside `root`, given its relative path.
fn abs(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.replace('\\', "/"))
}

/// Write (or create) the file backing `note`, record its relative path in the
/// `notes` row, and return that path. Atomic: temp file then rename, so a crash
/// never leaves a half-written note behind.
pub fn write_note(conn: &Connection, root: &Path, note: &Note) -> Result<String> {
    ensure_layout(root)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT vault_path FROM notes WHERE id=?1",
            params![note.id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();

    // Reuse the existing filename so renaming a note title doesn't churn files;
    // fall back to the slug and de-duplicate against other live notes.
    let rel = match existing.filter(|p| !p.trim().is_empty() && abs(root, p).exists()) {
        Some(p) => p,
        None => {
            let slug = slugify(&note.title);
            let mut candidate = format!("notes/{slug}.md");
            let mut n = 2;
            // Both the index AND the filesystem must be free: when the vault is
            // an existing Obsidian vault, a file with this name may already
            // exist outside our index and must never be clobbered.
            while abs(root, &candidate).exists()
                || conn
                    .query_row(
                        "SELECT id FROM notes WHERE vault_path=?1",
                        params![candidate],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                    .is_some()
            {
                candidate = format!("notes/{slug}-{n}.md");
                n += 1;
                if n > 999 {
                    candidate = format!("notes/{slug}-{}.md", &note.id[..note.id.len().min(8)]);
                    break;
                }
            }
            candidate
        }
    };

    let target = abs(root, &rel);
    // Preserve the frontmatter keys we do not own (`tags`, `aliases`, plugin
    // fields) by reading them off the file we are about to overwrite. Without
    // this, re-saving a note that was adopted from an Obsidian vault would
    // silently strip that vault's metadata.
    let extra = if target.exists() {
        std::fs::read_to_string(&target)
            .map(|t| parse(&t).extra)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let (subject, topic) = lookup_names(conn, note.subject_id.as_deref(), note.topic_id.as_deref());
    let file = NoteFile {
        id: note.id.clone(),
        title: note.title.clone(),
        body: note.body.clone(),
        subject,
        topic,
        created: Some(note.created_at),
        updated: Some(note.updated_at),
        extra,
    };
    let text = render(&file);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::read_to_string(&target).ok().as_deref() != Some(text.as_str()) {
        let tmp = target.with_extension("md.tmp");
        std::fs::write(&tmp, &text)?;
        std::fs::rename(&tmp, &target)?;
    }

    conn.execute(
        "UPDATE notes SET vault_path=?2 WHERE id=?1",
        params![note.id, rel],
    )?;
    Ok(rel)
}

/// Remove the file backing a note (best effort — the DB row is already gone).
pub fn remove_note(root: &Path, rel: &str) {
    if rel.trim().is_empty() {
        return;
    }
    let target = abs(root, rel);
    if target.exists() {
        let _ = std::fs::remove_file(target);
    }
}

fn lookup_names(
    conn: &Connection,
    subject_id: Option<&str>,
    topic_id: Option<&str>,
) -> (Option<String>, Option<String>) {
    let subject = subject_id.and_then(|id| {
        conn.query_row("SELECT name FROM subjects WHERE id=?1", params![id], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .ok()
        .flatten()
    });
    let topic = topic_id.and_then(|id| {
        conn.query_row("SELECT name FROM topics WHERE id=?1", params![id], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .ok()
        .flatten()
    });
    (subject, topic)
}

fn lookup_ids(
    conn: &Connection,
    subject: Option<&str>,
    topic: Option<&str>,
) -> (Option<String>, Option<String>) {
    let sid = subject.and_then(|name| {
        conn.query_row("SELECT id FROM subjects WHERE name=?1", params![name], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .ok()
        .flatten()
    });
    let tid = topic.and_then(|name| {
        conn.query_row("SELECT id FROM topics WHERE name=?1", params![name], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .ok()
        .flatten()
    });
    (sid, tid)
}

/// One `.md` found on disk during a scan.
#[derive(Debug, Clone)]
pub struct ScanEntry {
    pub rel_path: String,
    pub note: NoteFile,
    pub mtime: i64,
    pub text: String,
}

/// Walk `<root>/notes` (plus any other top-level folders the user keeps notes
/// in) and parse every `.md`. Hidden/app dirs are skipped.
pub fn scan(root: &Path) -> Result<Vec<ScanEntry>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    walk(root, root, &mut out, 0)?;
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<ScanEntry>, depth: usize) -> Result<()> {
    // Depth cap: a vault pointed at a home directory must not walk the world.
    if depth > 6 {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            walk(root, &path, out, depth + 1)?;
        } else if ft.is_file() && name.to_lowercase().ends_with(".md") {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or_else(now_ms);
            let mut note = parse(&text);
            if note.title.trim().is_empty() {
                note.title = name.trim_end_matches(".md").trim_end_matches(".MD").to_string();
            }
            out.push(ScanEntry {
                rel_path: rel,
                note,
                mtime,
                text,
            });
        }
    }
    Ok(())
}

/// Outcome of a vault -> DB reindex pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct IndexStats {
    /// Markdown files considered.
    pub files: usize,
    /// Files that had no `cortex_id` and were bound to an existing note or
    /// imported as a new one.
    pub adopted: usize,
    /// Notes whose DB row was refreshed from file contents.
    pub updated: usize,
    /// Notes already in sync (no write).
    pub unchanged: usize,
    /// Notes whose file disappeared; the row is kept, `vault_path` cleared.
    pub orphaned: usize,
}

/// Rebuild the `notes` index from the files on disk — the whole point of the
/// vault layer. Files win: their title/body overwrite the DB row. Notes whose
/// file is gone keep their row (never delete user data on a filesystem scan).
pub fn reindex(conn: &Connection, root: &Path) -> Result<IndexStats> {
    ensure_layout(root)?;
    let entries = scan(root)?;
    let mut stats = IndexStats {
        files: entries.len(),
        ..Default::default()
    };
    let mut seen: Vec<String> = Vec::with_capacity(entries.len());

    for entry in &entries {
        let mut note = entry.note.clone();

        // 1. Match by stable id, else by title among unbound notes.
        let mut db_id: Option<(String, String, i64)> = None;
        if !note.id.trim().is_empty() {
            db_id = conn
                .query_row(
                    "SELECT id, body, updated_at FROM notes WHERE id=?1",
                    params![note.id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)),
                )
                .optional()?;
        }
        if db_id.is_none() {
            note.id.clear();
            let by_title = conn
                .query_row(
                    "SELECT id, body, updated_at FROM notes
                     WHERE title=?1 AND (vault_path IS NULL OR vault_path='')
                     ORDER BY updated_at DESC LIMIT 1",
                    params![note.title],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)),
                )
                .optional()?;
            if let Some((id, body, updated)) = by_title {
                note.id = id;
                db_id = Some((note.id.clone(), body, updated));
            }
        }

        match db_id {
            Some((id, body, _updated)) => {
                seen.push(id.clone());
                let (subject_id, topic_id) =
                    lookup_ids(conn, note.subject.as_deref(), note.topic.as_deref());
                let changed = body != note.body
                    || conn
                        .query_row(
                            "SELECT title, vault_path FROM notes WHERE id=?1",
                            params![id],
                            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                        )
                        .optional()?
                        .map(|(t, p)| t != note.title || p.as_deref() != Some(entry.rel_path.as_str()))
                        .unwrap_or(true);
                conn.execute(
                    "UPDATE notes SET title=?2, body=?3, subject_id=COALESCE(?4, subject_id),
                            topic_id=COALESCE(?5, topic_id), vault_path=?6 WHERE id=?1",
                    params![id, note.title, note.body, subject_id, topic_id, entry.rel_path],
                )?;
                if changed {
                    crate::note_links::rebuild(conn, &id, &note.body)?;
                    stats.updated += 1;
                } else {
                    stats.unchanged += 1;
                }
                // Adopt the file: stamp the id so future scans match directly.
                if entry.note.id != id {
                    note.id = id;
                    let text = render(&note);
                    if text != entry.text {
                        let _ = write_raw(root, &entry.rel_path, &text);
                    }
                }
            }
            None => {
                // 2. Brand-new file: import it as a note.
                let id = new_id();
                let ts = entry.mtime.max(1);
                let (subject_id, topic_id) =
                    lookup_ids(conn, note.subject.as_deref(), note.topic.as_deref());
                conn.execute(
                    "INSERT INTO notes (id, subject_id, topic_id, title, body, created_at, updated_at, vault_path)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
                    params![id, subject_id, topic_id, note.title, note.body, ts, entry.rel_path],
                )?;
                crate::note_links::rebuild(conn, &id, &note.body)?;
                seen.push(id.clone());
                stats.adopted += 1;
                note.id = id;
                let text = render(&note);
                if text != entry.text {
                    let _ = write_raw(root, &entry.rel_path, &text);
                }
            }
        }
    }

    // 3. Notes whose file vanished: keep the row, drop the stale path.
    let mut stmt = conn.prepare(
        "SELECT id, vault_path FROM notes WHERE vault_path IS NOT NULL AND vault_path<>''",
    )?;
    let stale: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (id, _path) in stale {
        if !seen.contains(&id) {
            conn.execute("UPDATE notes SET vault_path=NULL WHERE id=?1", params![id])?;
            stats.orphaned += 1;
        }
    }

    Ok(stats)
}

fn write_raw(root: &Path, rel: &str, text: &str) -> Result<()> {
    let target = abs(root, rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

/// Export every DB note that has no file yet. Used the first time a vault is
/// enabled so an existing library is not stranded in SQLite.
pub fn export_missing(conn: &Connection, root: &Path) -> Result<usize> {
    ensure_layout(root)?;
    let notes: Vec<Note> = {
        let mut stmt = conn.prepare(
            "SELECT id, subject_id, topic_id, title, body, source_id, created_at, updated_at, vault_path
             FROM notes WHERE vault_path IS NULL OR vault_path=''",
        )?;
        let rows = stmt.query_map([], crate::repo::map_note)?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut written = 0;
    for note in &notes {
        let root = root.to_path_buf();
        write_note(conn, &root, note)?;
        written += 1;
    }
    Ok(written)
}

/// Number of `.md` files under the vault root, for status reporting.
pub fn count_files(root: &Path) -> usize {
    scan(root).map(|v| v.len()).unwrap_or(0)
}

/// Convert an io failure into the crate error with vault context.
pub fn io<T>(r: std::io::Result<T>, what: &str) -> Result<T> {
    r.map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("vault {what}: {e}"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;

    fn root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cortex-vault-test-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slug_strips_forbidden_chars_keeps_cjk() {
        assert_eq!(slugify("认知负荷: 理论与\n实践"), "认知负荷- 理论与-实践");
        assert_eq!(slugify("   "), "untitled");
        assert_eq!(slugify("a/b\\c*d?e"), "a-b-c-d-e");
        assert_eq!(slugify("..."), "untitled");
    }

    #[test]
    fn frontmatter_roundtrips_through_render_and_parse() {
        let note = NoteFile {
            id: "n_123".into(),
            title: "间隔重复: FSRS 与 SM-2".into(),
            body: "第一行\n[[双链]] 与 `code:` 都保留\n".into(),
            subject: Some("认知心理学".into()),
            topic: None,
            created: Some(1_700_000_000_000),
            updated: Some(1_700_000_060_000),
            extra: vec!["tags: [fsrs, 间隔重复]".into()],
        };
        let text = render(&note);
        assert!(text.starts_with("---\ncortex_id: n_123\n"));
        assert!(text.contains("tags: [fsrs, 间隔重复]"));
        let back = parse(&text);
        assert_eq!(back.extra, note.extra);
        assert_eq!(back.id, "n_123");
        assert_eq!(back.title, note.title);
        assert_eq!(back.subject.as_deref(), Some("认知心理学"));
        assert_eq!(back.body.trim_end(), note.body.trim_end());
    }

    #[test]
    fn parse_tolerates_plain_markdown_without_frontmatter() {
        let parsed = parse("# 大纲\n\n- 一\n- 二\n");
        assert!(parsed.id.is_empty());
        assert!(parsed.title.is_empty());
        assert!(parsed.body.contains("- 一"));
    }

    #[test]
    fn write_note_then_reindex_adopts_a_hand_written_file() {
        let dir = root();
        let state = AppState::in_memory().unwrap();
        let conn = state.db.lock().unwrap();

        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(
            dir.join("notes").join("手工笔记.md"),
            "# 手工笔记\n\n在 Obsidian 里写的。\n",
        )
        .unwrap();

        let stats = reindex(&conn, &dir).unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.adopted, 1);

        let (title, path): (String, String) = conn
            .query_row("SELECT title, vault_path FROM notes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "手工笔记");
        assert_eq!(path, "notes/手工笔记.md");

        // The file was stamped with an id, and a second pass is a no-op.
        let text = std::fs::read_to_string(dir.join("notes").join("手工笔记.md")).unwrap();
        assert!(text.contains("cortex_id:"));
        let again = reindex(&conn, &dir).unwrap();
        assert_eq!(again.unchanged, 1);
        assert_eq!(again.adopted, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reindex_keeps_row_when_file_disappears() {
        let dir = root();
        let state = AppState::in_memory().unwrap();
        let conn = state.db.lock().unwrap();
        let id = crate::repo::insert_note(&conn, None, None, "会被删掉", "正文").unwrap();
        let note = crate::repo::get_note(&conn, &id).unwrap();
        write_note(&conn, &dir, &note).unwrap();

        std::fs::remove_file(dir.join("notes").join("会被删掉.md")).unwrap();
        let stats = reindex(&conn, &dir).unwrap();
        assert_eq!(stats.orphaned, 1);
        // Data preserved, only the link to the file was dropped.
        let body: String = conn
            .query_row("SELECT body FROM notes WHERE id=?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(body, "正文");
    }

    #[test]
    fn adopting_a_foreign_file_keeps_its_frontmatter() {
        let dir = root();
        let state = AppState::in_memory().unwrap();
        let conn = state.db.lock().unwrap();
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        let path = dir.join("notes").join("外来笔记.md");
        std::fs::write(
            &path,
            "---\ntags: [obsidian, 外部]\naliases: [OBN]\n---\n\n正文 [[甲]]\n",
        )
        .unwrap();

        reindex(&conn, &dir).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("cortex_id:"), "id not stamped: {text}");
        assert!(text.contains("tags: [obsidian, 外部]"), "lost tags: {text}");
        assert!(text.contains("aliases: [OBN]"), "lost aliases: {text}");

        // A normal edit + save must not strip them either.
        conn.execute("UPDATE notes SET body='改过的正文'", []).unwrap();
        let id: String = conn
            .query_row("SELECT id FROM notes", [], |r| r.get(0))
            .unwrap();
        let note = crate::repo::get_note(&conn, &id).unwrap();
        write_note(&conn, &dir, &note).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("tags: [obsidian, 外部]"),
            "lost tags after save: {text}"
        );
        assert!(text.contains("改过的正文"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_missing_writes_every_db_only_note() {
        let dir = root();
        let state = AppState::in_memory().unwrap();
        let conn = state.db.lock().unwrap();
        crate::repo::insert_note(&conn, None, None, "甲", "A").unwrap();
        crate::repo::insert_note(&conn, None, None, "乙", "B").unwrap();

        assert_eq!(export_missing(&conn, &dir).unwrap(), 2);
        assert_eq!(count_files(&dir), 2);
        assert_eq!(export_missing(&conn, &dir).unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
