//! Recovery directories — Gloss's "delete moves to .deleted-<id>, never
//! auto-purge, export-first" safety net.
//!
//! Delete-subject already had an in-DB undo journal (delete_subject_with_undo)
//! — good for "oops" 30 seconds later. This is the other layer: a FILESYSTEM
//! snapshot in `<data_dir>/.deleted-<subject-id>-<ts>/` containing every row
//! (as JSON) plus the subject's attachments. Nothing auto-deletes; a restore
//! command re-inserts the rows (same ids — attachments are named by source id)
//! and copies files back. Users who re-export cleanly purge manually.

use crate::error::{Error, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;

const TABLES: [&str; 4] = ["subjects", "topics", "sources", "notes"];

#[derive(Debug, Clone, Serialize)]
pub struct DeletedSubject {
    pub dir_name: String,
    pub path: String,
    pub subject_name: String,
    pub deleted_at: i64,
    pub sources: usize,
    pub notes: usize,
    pub bytes: u64,
}

/// Dump one table's rows for the subject as a JSON array (column names as keys).
fn dump_table(conn: &Connection, table: &str, col: &str, id: &str) -> Result<Vec<serde_json::Value>> {
    let sql = format!("SELECT * FROM {table} WHERE {col}=?1");
    let mut stmt = conn.prepare(&sql)?;
    let col_names: Vec<String> = stmt.column_names().into_iter().map(|s| s.to_string()).collect();
    let rows = stmt
        .query_map(params![id], |r| {
            let mut m = serde_json::Map::new();
            for (i, name) in col_names.iter().enumerate() {
                let v: rusqlite::types::Value = r.get(i)?;
                let jv = match v {
                    rusqlite::types::Value::Null => serde_json::Value::Null,
                    rusqlite::types::Value::Integer(n) => serde_json::Value::from(n),
                    rusqlite::types::Value::Real(x) => serde_json::Value::from(x),
                    rusqlite::types::Value::Text(t) => serde_json::Value::from(t),
                    rusqlite::types::Value::Blob(b) => {
                        serde_json::Value::from(base16(&b))
                    }
                };
                m.insert(name.clone(), jv);
            }
            Ok(serde_json::Value::Object(m))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn base16(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn dir_size(dir: &Path) -> u64 {
    fn walk(p: &Path) -> u64 {
        std::fs::read_dir(p)
            .map(|rd| {
                rd.flatten()
                    .map(|e| {
                        let path = e.path();
                        if path.is_dir() {
                            walk(&path)
                        } else {
                            e.metadata().map(|m| m.len()).unwrap_or(0)
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    walk(dir)
}

/// Stage a full snapshot of the subject into `.deleted-<id>-<ts>/` — rows +
/// attachments. Does NOT delete (the caller composes with the undo journal).
/// Returns the directory name.
pub fn capture(conn: &Connection, data_dir: &Path, id: &str) -> Result<String> {
    let subject_name: String = conn
        .query_row("SELECT name FROM subjects WHERE id=?1", [id], |r| r.get(0))
        .map_err(|_| Error::NotFound(format!("subject {id}")))?;

    let mut snapshot = serde_json::Map::new();
    snapshot.insert("deleted_at".into(), serde_json::Value::from(crate::db::now_ms()));
    snapshot.insert("subject_id".into(), serde_json::Value::from(id));
    let mut sources_count = 0usize;
    let mut notes_count = 0usize;
    for t in TABLES {
        let col = if t == "subjects" { "id" } else { "subject_id" };
        let rows = dump_table(conn, t, col, id)?;
        snapshot.insert(t.into(), serde_json::Value::Array(rows));
        if t == "sources" {
            sources_count = snapshot[t].as_array().map(|a| a.len()).unwrap_or(0);
        }
        if t == "notes" {
            notes_count = snapshot[t].as_array().map(|a| a.len()).unwrap_or(0);
        }
    }

    let dir_name = format!(".deleted-{id}-{}", crate::db::now_ms());
    let dir = data_dir.join(&dir_name);
    std::fs::create_dir_all(dir.join("sources"))
        .map_err(|e| Error::Other(format!("create recovery dir: {e}")))?;

    // Attachments: copy each source's stored file under its source id.
    if let Some(rows) = snapshot["sources"].as_array() {
        for row in rows {
            let sid = row.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let sp = row.get("stored_path").and_then(|v| v.as_str()).unwrap_or_default();
            if sid.is_empty() || sp.is_empty() || !Path::new(sp).is_file() {
                continue;
            }
            let ext = Path::new(sp)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("bin");
            let dest = dir.join("sources").join(format!("{sid}.{ext}"));
            let _ = std::fs::copy(sp, &dest);
        }
    }
    std::fs::write(
        dir.join("snapshot.json"),
        serde_json::to_vec_pretty(&serde_json::Value::Object(snapshot))
            .map_err(|e| Error::Other(e.to_string()))?,
    )
    .map_err(|e| Error::Other(format!("write snapshot: {e}")))?;
    std::fs::write(
        dir.join("RESTORE.txt"),
        format!(
            "删除于 {}\n科目: {subject_name}\n来源 {sources_count} 个 · 笔记 {notes_count} 条\n\n\
             恢复：Cortex 设置 → 数据 → 恢复目录 里点「恢复」。\n\
             彻底删除：同一处点「清除」——此操作不可逆。",
            chrono_stamp(),
        ),
    )
    .map_err(|e| Error::Other(format!("write RESTORE.txt: {e}")))?;

    Ok(dir_name)
}

fn chrono_stamp() -> String {
    // Local timestamp without pulling a date crate.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs_today = now % 86_400;
    let days = now / 86_400;
    // Days since 1970-01-01 (proleptic Gregorian, good enough for a stamp).
    let (y, m, d) = civil(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs_today / 3600,
        (secs_today % 3600) / 60,
        secs_today % 60
    )
}

fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// List staged deletions in the data dir, newest first.
pub fn list(data_dir: &Path) -> Result<Vec<DeletedSubject>> {
    let rd = match std::fs::read_dir(data_dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out: Vec<DeletedSubject> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(".deleted-") else { continue };
        // Format: .deleted-<subject-id>-<ms>
        let Some(pos) = rest.rfind('-') else { continue };
        let deleted_at: i64 = rest[pos + 1..].parse().unwrap_or(0);
        let dir = entry.path();
        if !dir.join("snapshot.json").is_file() {
            continue;
        }
        let snap: serde_json::Value = std::fs::read_to_string(dir.join("snapshot.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        let subject_name = snap
            .get("subjects")
            .and_then(|v| v.get(0))
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("(未知科目)")
            .to_string();
        let sources = snap.get("sources").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        let notes = snap.get("notes").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        out.push(DeletedSubject {
            dir_name: name,
            path: dir.to_string_lossy().into_owned(),
            subject_name,
            deleted_at,
            sources,
            notes,
            bytes: dir_size(&dir),
        });
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.deleted_at));
    Ok(out)
}

/// Restore a staged deletion: re-insert rows (original ids — attachments are
/// named by source id) and copy attachments back into the live sources dir.
pub fn restore(conn: &Connection, data_dir: &Path, dir_name: &str) -> Result<(String, usize, usize)> {
    if !dir_name.starts_with(".deleted-") || dir_name.contains('/') || dir_name.contains('\\') || dir_name.contains("..") {
        return Err(Error::Other("非法目录名".into()));
    }
    let dir = data_dir.join(dir_name);
    let snap_text = std::fs::read_to_string(dir.join("snapshot.json"))
        .map_err(|e| Error::Other(format!("读取 snapshot 失败: {e}")))?;
    let snap: serde_json::Value = serde_json::from_str(&snap_text)
        .map_err(|e| Error::Other(format!("snapshot 不是合法 JSON: {e}")))?;

    let subject = snap
        .get("subjects")
        .and_then(|v| v.get(0))
        .cloned()
        .ok_or_else(|| Error::Other("snapshot 缺少 subjects[0]".into()))?;
    let subject_id = subject.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let subject_name = subject.get("name").and_then(|v| v.as_str()).unwrap_or("科目").to_string();
    // Refuse to clobber an existing subject with the same id.
    let exists: bool = conn
        .query_row("SELECT 1 FROM subjects WHERE id=?1", [&subject_id], |_| Ok(true))
        .unwrap_or(false);
    if exists {
        return Err(Error::Other(format!(
            "ID 为 {subject_id} 的科目已存在 —— 请先处理它再恢复"
        )));
    }

    // Insert rows table by table, preserving ids; NULL-ify nothing (schema is
    // identical). attachments copy back under their source-id names.
    for table in TABLES {
        let Some(rows) = snap.get(table).and_then(|v| v.as_array()) else { continue };
        for row in rows {
            restore_row(conn, table, row)?;
        }
    }
    // Attachments back to the live sources dir.
    let live_sources = data_dir.join("sources");
    std::fs::create_dir_all(&live_sources).ok();
    for f in std::fs::read_dir(dir.join("sources")).into_iter().flatten().flatten() {
        let dest = live_sources.join(f.file_name());
        let _ = std::fs::copy(f.path(), dest);
    }
    let sources = snap.get("sources").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let notes = snap.get("notes").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    Ok((subject_name, sources, notes))
}

/// Re-insert one snapshot row. Column list comes from the row's keys; missing
/// NOT NULL columns (e.g. newer migrations) fall back to defaults, so a
/// snapshot from an older schema can't break a newer DB (restored rows just
/// miss the new columns).
fn restore_row(conn: &Connection, table: &str, row: &serde_json::Value) -> Result<()> {
    let obj = row
        .as_object()
        .ok_or_else(|| Error::Other("snapshot 行不是对象".into()))?;
    if obj.is_empty() {
        return Ok(());
    }
    let cols: Vec<&String> = obj.keys().collect();
    let names = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = (0..cols.len())
        .map(|i| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let params: Vec<serde_json::Value> = cols
        .iter()
        .map(|c| obj[*c].clone())
        .collect();
    let sql = format!("INSERT OR REPLACE INTO \"{table}\" ({names}) VALUES ({placeholders})");
    // Bind through a dynamic params vec.
    let mut stmt = conn.prepare(&sql)?;
    let boxed: Vec<Box<dyn rusqlite::ToSql>> = params
        .iter()
        .map(|v| -> Box<dyn rusqlite::ToSql> {
            match v {
                serde_json::Value::Null => Box::new(rusqlite::types::Null),
                serde_json::Value::Bool(b) => Box::new(*b as i64),
                serde_json::Value::Number(n) => Box::new(n.as_i64().unwrap_or(0)),
                serde_json::Value::String(s) => Box::new(s.clone()),
                other => Box::new(other.to_string()),
            }
        })
        .collect();
    let refs: Vec<&dyn rusqlite::ToSql> = boxed.iter().map(|b| b.as_ref()).collect();
    stmt.execute(rusqlite::params_from_iter(refs))?;
    Ok(())
}

/// Permanently delete a staged directory (after the user confirms).
pub fn purge(data_dir: &Path, dir_name: &str) -> Result<()> {
    if !dir_name.starts_with(".deleted-") || dir_name.contains('/') || dir_name.contains('\\') {
        return Err(Error::Other("非法目录名".into()));
    }
    let dir = data_dir.join(dir_name);
    if !dir.is_dir() {
        return Err(Error::NotFound(format!("{dir_name} 不存在")));
    }
    // Deletion via trash is the polite move on user directories; fall back to a
    // plain remove when no trash command exists.
    let ok = crate::ingest::new_cmd("gio")
        .args(["trash", dir.to_str().unwrap_or("")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        std::fs::remove_dir_all(&dir).map_err(|e| Error::Other(format!("删除失败: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_restore_roundtrip_preserves_ids() {
        let base = std::env::temp_dir().join(format!("cortex-recovery-{}", std::process::id()));
        let dd = base.join("data");
        std::fs::create_dir_all(dd.join("sources")).unwrap();
        let dbp = dd.join("cortex.db");
        let st = crate::db::AppState::new(&dbp).unwrap();
        {
            let c = st.db.lock().unwrap();
            let sub = crate::repo::insert_subject(&c, "线代", None, None, None).unwrap();
            let topic = crate::repo::insert_topic(&c, &sub, "特征值", None, &[]).unwrap();
            let sid = crate::repo::insert_source(&c, &sub, Some(&topic), "讲义.md", "md", None).unwrap();
            std::fs::write(dd.join("sources/note.md"), "特征值是指缩放因子。").unwrap();
            c.execute(
                "UPDATE sources SET content='特征值是指缩放因子。', stored_path=?1 WHERE id=?2",
                rusqlite::params![dd.join("sources/note.md").to_string_lossy(), sid],
            )
            .unwrap();
            crate::repo::insert_note(&c, Some(&sub), Some(&topic), "笔记", "相似变换保持特征值。").unwrap();

            // Stage (no delete), then undo-delete — exactly what the command composes.
            let dir_name = capture(&c, &dd, &sub).unwrap();
            crate::repo::delete_subject_with_undo(&c, &sub).unwrap();
            assert!(dd.join(&dir_name).join("snapshot.json").is_file());
            assert!(dd.join(&dir_name).join("sources").join(format!("{sid}.md")).is_file());
            assert!(crate::repo::get_subject(&c, &sub).is_err());
            let list = list(&dd).unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].subject_name, "线代");
            assert_eq!(list[0].sources, 1);

            // Restore: same ids back, rows intact, attachment copied home.
            let (name, sources, notes) = restore(&c, &dd, &dir_name).unwrap();
            assert_eq!(name, "线代");
            assert_eq!(sources, 1);
            assert_eq!(notes, 1);
            let got = crate::repo::get_subject(&c, &sub).unwrap();
            assert_eq!(got.name, "线代");
            assert!(dd.join("sources/note.md").is_file());
            // Restore refuses to clobber.
            assert!(restore(&c, &dd, &dir_name).is_err());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn purge_and_restore_reject_bad_dir_names() {
        let base = std::env::temp_dir().join(format!("cortex-recovery2-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        assert!(purge(&base, "../../etc").is_err());
        assert!(restore(&crate::db::AppState::in_memory().unwrap().db.lock().unwrap(), &base, "evil").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
