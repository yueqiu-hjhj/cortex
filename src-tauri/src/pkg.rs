//! `.cortex-pkg.tar.gz` — the portable archive (P0 #2 in the competitive plan).
//!
//! `backup.rs` ships encrypted snapshots to the CLOUD (age + rclone). That is a
//! disaster-recovery tool, not a portable one: you cannot grep it, diff it, or
//! hand it to a colleague. This module produces the other half — a plain,
//! inspectable archive:
//!
//! ```text
//! cortex-pkg.tar.gz
//! ├── manifest.json      format/version, per-file SHA-256, counts, orphans
//! ├── cortex.db          WAL-checkpointed snapshot (no -wal sidecar needed)
//! └── sources/           original attachments, named <source_id>.<ext>
//! ```
//!
//! Design choices (each mirrors a lesson from KnowNote / Gloss):
//! - **Hash everything, verify on import.** A tampered byte is caught and the
//!   file named — not a silent half-restore.
//! - **Refuse orphans by default.** A DB row whose file is gone means the
//!   snapshot lies; exporting it silently would spread the lie. `force` is
//!   available but the skipped ids are recorded in the manifest.
//! - **No absolute paths in the manifest.** Attachments are addressed by
//!   `<source_id>.<ext>`, so the archive is location-independent.
//! - **Import repoints stored_path.** The DB carries absolute paths from the
//!   exporting machine; after extraction every source is re-pointed against
//!   the new `<target>/sources`, so previews keep working.
//!
//! Vector embeddings are model-specific; the manifest records the chunk
//! dimension so a mismatched target can be detected (see `embedding_dim`).

use crate::error::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "cortex-pkg";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgFile {
    /// Relative path inside the archive (e.g. `sources/abc.pdf`).
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgCounts {
    pub subjects: i64,
    pub sources: i64,
    pub chunks: i64,
    pub notes: i64,
    pub papers: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgManifest {
    pub format: String,
    pub format_version: u32,
    pub app_version: String,
    pub created_at: i64,
    pub db_sha256: String,
    /// Chunk vector dimension at export time (NULL when the library is empty).
    /// A target whose embedder produces a different dimension should re-embed.
    pub embedding_dim: Option<i64>,
    pub counts: PkgCounts,
    pub files: Vec<PkgFile>,
    /// Source ids whose attachment was missing at export time (force mode only).
    pub orphans: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PkgExportResult {
    pub out: String,
    pub files: usize,
    pub bytes: u64,
    pub orphans: Vec<String>,
    /// SHA-256 of the archive itself — what the user can quote when sharing.
    pub sha256: String,
    pub counts: PkgCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct PkgVerifyResult {
    pub ok: bool,
    pub files: usize,
    pub problems: Vec<String>,
    pub counts: Option<PkgCounts>,
    pub created_at: Option<i64>,
    pub app_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PkgImportResult {
    pub files: usize,
    pub counts: PkgCounts,
    pub repointed: usize,
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| Error::Other(format!("open {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| Error::Other(format!("read {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn counts(conn: &Connection) -> Result<PkgCounts> {
    // Tolerant by design: an archive may come from an older schema (a table
    // that doesn't exist yet counts as 0, never an export-blocking error).
    let count_or_zero = |table: &str| -> i64 {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !exists {
            return 0;
        }
        conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| r.get(0))
            .unwrap_or(0)
    };
    Ok(PkgCounts {
        subjects: count_or_zero("subjects"),
        sources: count_or_zero("sources"),
        chunks: count_or_zero("chunks"),
        notes: count_or_zero("notes"),
        papers: count_or_zero("papers"),
    })
}

/// One `stored_path` per source, resolved. Missing files are returned as
/// orphans instead of being silently skipped.
fn source_attachments(conn: &Connection) -> Result<(Vec<(String, PathBuf)>, Vec<String>)> {
    let mut st = conn.prepare(
        "SELECT id, stored_path FROM sources WHERE stored_path IS NOT NULL AND stored_path <> ''",
    )?;
    let rows: Vec<(String, String)> = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut found = Vec::new();
    let mut orphans = Vec::new();
    for (id, path) in rows {
        let p = PathBuf::from(&path);
        if p.is_file() {
            found.push((id, p));
        } else {
            orphans.push(id);
        }
    }
    Ok((found, orphans))
}

fn add_path_to_tar<W: Write>(
    tar: &mut tar::Builder<W>,
    archive_name: &str,
    src: &Path,
) -> Result<()> {
    let mut f = std::fs::File::open(src)
        .map_err(|e| Error::Other(format!("open {}: {e}", src.display())))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(file_size(src));
    header.set_mode(0o644);
    header.set_mtime(crate::db::now_ms().max(0) as u64 / 1000);
    header.set_cksum();
    tar.append_data(&mut header, archive_name, &mut f)
        .map_err(|e| Error::Other(format!("tar {archive_name}: {e}")))?;
    Ok(())
}

/// Export the library at `data_dir` into a `.cortex-pkg.tar.gz` at `out`.
/// The DB is checkpointed first so the archive is self-contained (no `-wal`).
pub fn export(
    data_dir: &Path,
    out: &Path,
    force_orphans: bool,
    progress: &mut dyn FnMut(&str, u8),
) -> Result<PkgExportResult> {
    let db_path = data_dir.join("cortex.db");
    if !db_path.is_file() {
        return Err(Error::Other(format!(
            "找不到数据库: {} —— 请指定正确的数据目录",
            db_path.display()
        )));
    }
    progress("准备数据库快照…", 5);
    // WAL checkpoint so cortex.db alone is the whole story.
    {
        let conn = Connection::open(&db_path)
            .map_err(|e| Error::Other(format!("open db: {e}")))?;
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE").ok();
    }
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Error::Other(format!("open db (ro): {e}")))?;
    let c = counts(&conn)?;
    let embedding_dim: Option<i64> = conn
        .query_row("SELECT MAX(dim) FROM chunks", [], |r| r.get(0))
        .ok()
        .flatten();
    let (attachments, orphans) = source_attachments(&conn)?;
    drop(conn);

    if !orphans.is_empty() && !force_orphans {
        return Err(Error::Other(format!(
            "有 {} 个来源的附件缺失（例如 {}）——归档会不完整。\
             用 force 跳过并把缺失项写进 manifest，或先修复这些来源。",
            orphans.len(),
            orphans.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        )));
    }

    // Stage: manifest + db + attachments, hashed before writing the tar.
    // Unique per call — parallel exports (tests, CLI + UI) must not share it.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stage = std::env::temp_dir().join(format!("cortex-pkg-{}-{stamp}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(stage.join("sources"))
        .map_err(|e| Error::Other(format!("stage: {e}")))?;

    let db_staged = stage.join("cortex.db");
    std::fs::copy(&db_path, &db_staged)
        .map_err(|e| Error::Other(format!("copy db: {e}")))?;
    let db_sha = sha256_file(&db_staged)?;

    let mut files = Vec::with_capacity(attachments.len());
    for (i, (id, src)) in attachments.iter().enumerate() {
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let name = format!("sources/{id}.{ext}");
        let dest = stage.join(&name);
        std::fs::copy(src, &dest)
            .map_err(|e| Error::Other(format!("copy {}: {e}", src.display())))?;
        files.push(PkgFile {
            path: name,
            sha256: sha256_file(&dest)?,
            bytes: file_size(&dest),
        });
        if i % 10 == 0 {
            progress(&format!("打包附件 {}/{}…", i + 1, attachments.len()), 30);
        }
    }

    let manifest = PkgManifest {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        created_at: crate::db::now_ms(),
        db_sha256: db_sha,
        embedding_dim,
        counts: c.clone(),
        files,
        orphans: orphans.clone(),
    };
    std::fs::write(
        stage.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| Error::Other(e.to_string()))?,
    )
    .map_err(|e| Error::Other(format!("write manifest: {e}")))?;

    progress("压缩归档…", 80);
    {
        let out_file = std::fs::File::create(out)
            .map_err(|e| Error::Other(format!("create {}: {e}", out.display())))?;
        let enc = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        add_path_to_tar(&mut tar, "manifest.json", &stage.join("manifest.json"))?;
        add_path_to_tar(&mut tar, "cortex.db", &db_staged)?;
        for f in &manifest.files {
            add_path_to_tar(&mut tar, &f.path, &stage.join(&f.path))?;
        }
        let enc = tar
            .into_inner()
            .map_err(|e| Error::Other(format!("tar finish: {e}")))?;
        enc.finish()
            .map_err(|e| Error::Other(format!("gzip finish: {e}")))?;
    }
    let pkg_sha = sha256_file(out)?;
    let _ = std::fs::remove_dir_all(&stage);
    progress("完成", 100);
    Ok(PkgExportResult {
        out: out.to_string_lossy().into_owned(),
        files: manifest.files.len(),
        bytes: file_size(out),
        orphans,
        sha256: pkg_sha,
        counts: c,
    })
}

/// Read + hash-verify an archive. Never extracts.
pub fn verify(pkg: &Path) -> Result<PkgVerifyResult> {
    let file = std::fs::File::open(pkg)
        .map_err(|e| Error::Other(format!("open {}: {e}", pkg.display())))?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    let mut manifest: Option<PkgManifest> = None;
    let mut problems = Vec::new();
    let mut checked = 0usize;

    let entries = archive
        .entries()
        .map_err(|e| Error::Other(format!("read tar: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| Error::Other(format!("tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| Error::Other(format!("entry path: {e}")))?
            .to_string_lossy()
            .into_owned();
        if path == "manifest.json" {
            let mut buf = String::new();
            entry
                .read_to_string(&mut buf)
                .map_err(|e| Error::Other(format!("read manifest: {e}")))?;
            let m: PkgManifest = serde_json::from_str(&buf)
                .map_err(|e| Error::Other(format!("manifest 不是合法 JSON: {e}")))?;
            if m.format != FORMAT {
                return Err(Error::Other(format!(
                    "不是 Cortex 归档（format={}）",
                    m.format
                )));
            }
            if m.format_version > FORMAT_VERSION {
                return Err(Error::Other(format!(
                    "归档版本 {} 高于本程序支持的 {}，请升级 Cortex",
                    m.format_version, FORMAT_VERSION
                )));
            }
            manifest = Some(m);
            continue;
        }
        // Hash the entry stream directly — no extraction to disk.
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = entry
                .read(&mut buf)
                .map_err(|e| Error::Other(format!("read {path}: {e}")))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let got = format!("{:x}", hasher.finalize());
        if let Some(m) = &manifest {
            if path == "cortex.db" {
                if got != m.db_sha256 {
                    problems.push(format!("cortex.db 哈希不符（期望 {}，实际 {got}）", m.db_sha256));
                }
                checked += 1;
            } else if let Some(expect) = m.files.iter().find(|f| f.path == path) {
                if got != expect.sha256 {
                    problems.push(format!("{path} 哈希不符"));
                }
                checked += 1;
            } else {
                problems.push(format!("归档里有多余文件: {path}"));
            }
        }
    }

    let Some(m) = manifest else {
        return Err(Error::Other("归档缺少 manifest.json".into()));
    };

    Ok(PkgVerifyResult {
        ok: problems.is_empty(),
        files: checked,
        problems,
        counts: Some(m.counts),
        created_at: Some(m.created_at),
        app_version: Some(m.app_version),
    })
}

/// Verify, then extract into `target_dir`. Refuses a non-empty target unless
/// `force`. After extraction every source's absolute `stored_path` is
/// re-pointed against `<target>/sources`, so previews work immediately.
pub fn import(pkg: &Path, target_dir: &Path, force: bool) -> Result<PkgImportResult> {
    let v = verify(pkg)?;
    if !v.ok {
        return Err(Error::Other(format!(
            "归档校验失败，拒绝导入：{}",
            v.problems.join("；")
        )));
    }
    let db_dest = target_dir.join("cortex.db");
    if db_dest.exists() && !force {
        return Err(Error::Other(format!(
            "目标目录已有数据库（{}）——换一个空目录，或加 force 覆盖",
            db_dest.display()
        )));
    }
    std::fs::create_dir_all(target_dir.join("sources"))
        .map_err(|e| Error::Other(format!("create target: {e}")))?;

    let file = std::fs::File::open(pkg)
        .map_err(|e| Error::Other(format!("open {}: {e}", pkg.display())))?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    let entries = archive
        .entries()
        .map_err(|e| Error::Other(format!("read tar: {e}")))?;
    let mut files = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|e| Error::Other(format!("tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| Error::Other(format!("entry path: {e}")))?
            .to_string_lossy()
            .into_owned();
        if path == "manifest.json" {
            continue;
        }
        // Only two shapes are allowed: cortex.db and sources/<name>.
        let rel = if path == "cortex.db" {
            PathBuf::from("cortex.db")
        } else if let Some(name) = path.strip_prefix("sources/") {
            if name.contains('/') || name.contains("..") {
                return Err(Error::Other(format!("归档包含非法路径: {path}")));
            }
            PathBuf::from("sources").join(name)
        } else {
            return Err(Error::Other(format!("归档包含未知条目: {path}")));
        };
        let dest = target_dir.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut out = std::fs::File::create(&dest)
            .map_err(|e| Error::Other(format!("create {}: {e}", dest.display())))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| Error::Other(format!("extract {path}: {e}")))?;
        if path != "cortex.db" {
            files += 1;
        }
    }

    // Re-point absolute stored_paths against the new location.
    let conn = Connection::open(&db_dest)
        .map_err(|e| Error::Other(format!("open imported db: {e}")))?;
    crate::sync::repoint_source_files(&conn, &target_dir.join("sources"))?;
    let repointed = counts(&conn)?.sources as usize;
    let c = counts(&conn)?;
    Ok(PkgImportResult {
        files,
        counts: c,
        repointed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny library: db + one attachment + one orphanable source.
    fn seed_lib(dir: &Path, with_orphan: bool) {
        std::fs::create_dir_all(dir.join("sources")).unwrap();
        let db = dir.join("cortex.db");
        {
            let st = crate::db::AppState::new(&db).unwrap();
            let c = st.db.lock().unwrap();
            c.execute_batch(
                "INSERT INTO subjects (id, name, created_at, updated_at) VALUES ('s1','线代',1,1);",
            )
            .unwrap();
            std::fs::write(dir.join("sources/src1.md"), "正文").unwrap();
            c.execute(
                "INSERT INTO sources (id, subject_id, name, kind, status, content, stored_path, created_at, updated_at) \
                 VALUES ('src1','s1','讲义.md','md','ready','正文',?1,1,1)",
                [dir.join("sources/src1.md").to_string_lossy().to_string()],
            )
            .unwrap();
            if with_orphan {
                c.execute(
                    "INSERT INTO sources (id, subject_id, name, kind, status, stored_path, created_at, updated_at) \
                     VALUES ('src2','s1','丢了.pdf','pdf','ready',?1,1,1)",
                    [dir.join("sources/missing.pdf").to_string_lossy().to_string()],
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn export_verify_import_roundtrip() {
        let base = std::env::temp_dir().join(format!("cortex-pkg-t1-{}", std::process::id()));
        let src = base.join("src");
        seed_lib(&src, false);
        let pkg = base.join("lib.cortex-pkg.tar.gz");
        let res = export(&src, &pkg, false, &mut |_, _| {}).unwrap();
        assert_eq!(res.files, 1);
        assert_eq!(res.counts.sources, 1);
        assert_eq!(res.sha256.len(), 64);

        let v = verify(&pkg).unwrap();
        assert!(v.ok, "problems: {:?}", v.problems);
        assert_eq!(v.files, 2, "cortex.db + one attachment");

        // Import into a fresh dir → db + attachment + repointed stored_path.
        let target = base.join("target");
        let imp = import(&pkg, &target, false).unwrap();
        assert_eq!(imp.files, 1);
        assert_eq!(imp.counts.sources, 1);
        assert!(target.join("sources/src1.md").is_file());
        let c = Connection::open(target.join("cortex.db")).unwrap();
        let sp: String = c
            .query_row("SELECT stored_path FROM sources WHERE id='src1'", [], |r| r.get(0))
            .unwrap();
        assert!(
            sp.contains("target") && sp.contains("sources"),
            "stored_path should point into the import target, got {sp}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn orphan_aborts_without_force_and_records_with_force() {
        let base = std::env::temp_dir().join(format!("cortex-pkg-t2-{}", std::process::id()));
        let src = base.join("src");
        seed_lib(&src, true);
        let pkg = base.join("o.tar.gz");
        let err = export(&src, &pkg, false, &mut |_, _| {}).unwrap_err().to_string();
        assert!(err.contains("附件缺失"), "got: {err}");
        let ok = export(&src, &pkg, true, &mut |_, _| {}).unwrap();
        assert_eq!(ok.orphans, vec!["src2".to_string()]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tampered_archive_fails_verification() {
        let base = std::env::temp_dir().join(format!("cortex-pkg-t3-{}", std::process::id()));
        let src = base.join("src");
        seed_lib(&src, false);
        let pkg = base.join("t.tar.gz");
        export(&src, &pkg, false, &mut |_, _| {}).unwrap();
        // Flip a byte in the middle of the compressed payload — the deflate
        // stream or the per-file hash must catch it (trailer bytes are not
        // read by the decoder, so flipping there proves nothing).
        let mut bytes = std::fs::read(&pkg).unwrap();
        let n = bytes.len();
        bytes[n / 2] ^= 0xFF;
        std::fs::write(&pkg, &bytes).unwrap();
        let v = verify(&pkg);
        // Either the gzip layer rejects it or the hash check does — both are
        // honest failures, neither may report ok.
        match v {
            Ok(r) => assert!(!r.ok, "tampered archive must not verify"),
            Err(_) => {}
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn manifest_has_no_absolute_paths() {
        let base = std::env::temp_dir().join(format!("cortex-pkg-t4-{}", std::process::id()));
        let src = base.join("src");
        seed_lib(&src, false);
        let pkg = base.join("p.tar.gz");
        export(&src, &pkg, false, &mut |_, _| {}).unwrap();
        let f = std::fs::File::open(&pkg).unwrap();
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
        let mut manifest = String::new();
        for e in ar.entries().unwrap() {
            let mut e = e.unwrap();
            if e.path().unwrap().to_string_lossy() == "manifest.json" {
                e.read_to_string(&mut manifest).unwrap();
            }
        }
        assert!(manifest.contains("cortex-pkg"));
        assert!(!manifest.contains(":\\\\"), "no Windows absolute paths");
        assert!(!manifest.contains("/home/"), "no unix absolute paths");
        assert!(manifest.contains("sources/src1.md"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
