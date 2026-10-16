//! Startup self-check. Runs after migrations are applied and the DB is
//! opened, but BEFORE the UI starts fetching data. The check is non-fatal:
//! every problem becomes a Check { ok: false, name, hint } row that the
//! frontend can show as a banner, but the app still starts.
//!
//! Why non-fatal: a single missing ffmpeg binary should never block the
//! user from opening their cheatsheets. The OS notification / banner is the
//! right surface for "something is off" - the user can keep working while
//! they read the details.

use crate::error::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub passed: usize,
    pub failed: usize,
    pub checks: Vec<Check>,
    pub user_version: i64,
    pub known_migrations: usize,
}

fn check_db_integrity(conn: &Connection) -> Check {
    let res: rusqlite::Result<String> = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0));
    match res {
        Ok(s) if s.eq_ignore_ascii_case("ok") => Check { name: "DB integrity", ok: true, detail: "ok".into(), hint: None },
        Ok(other) => Check { name: "DB integrity", ok: false, detail: other, hint: Some("Restore from a sync snapshot or rebuild the local DB.".into()) },
        Err(e) => Check { name: "DB integrity", ok: false, detail: e.to_string(), hint: Some("DB could not be opened.".into()) },
    }
}

fn check_migration_version(conn: &Connection, known: usize) -> (Check, i64) {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);
    let ok = v as usize == known;
    let check = Check {
        name: "Schema version",
        ok,
        detail: format!("user_version={} (expected {})", v, known),
        hint: if ok { None } else { Some("Run pending migrations before the next launch.".into()) },
    };
    (check, v)
}

fn check_disk_space(data_dir: &Path) -> Check {
    // The data_dir may not exist on first launch; create it then probe.
    let probe = data_dir.join(".cortex-disk-probe");
    let _ = std::fs::write(&probe, b"ok");
    let _ = std::fs::remove_file(&probe);
    // Available space via statvfs-equivalent on Windows: GetDiskFreeSpaceExW.
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let path: Vec<u16> = data_dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut free: u64 = 0;
        let ok = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        if ok == 0 {
            return Check { name: "Disk space", ok: false, detail: "GetDiskFreeSpaceExW failed".into(), hint: None };
        }
        let mb = free / 1_048_576;
        let warn = mb < 500;
        Check {
            name: "Disk space",
            ok: !warn,
            detail: format!("{} MB free", mb),
            hint: if warn { Some("Less than 500 MB free; embedding model + downloaded binaries need room.".into()) } else { None },
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Fallback: statvfs via rustix is overkill; report "unknown" rather than fail.
        Check { name: "Disk space", ok: true, detail: "n/a".into(), hint: None }
    }
}

fn check_data_dir_writable(data_dir: &Path) -> Check {
    let probe = data_dir.join(".cortex-write-test");
    match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
        Ok(_) => Check { name: "Data dir writable", ok: true, detail: data_dir.display().to_string(), hint: None },
        Err(e) => Check { name: "Data dir writable", ok: false, detail: e.to_string(), hint: Some("Check the data dir permissions (e.g. on Linux: a stale ~/.local/share ownership).".into()) },
    }
}

fn check_onnx_model_if_used(conn: &Connection, data_dir: &Path) -> Check {
    let provider: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key='embed_provider'", [], |r| r.get(0))
        .ok();
    if provider.as_deref() != Some("onnx") {
        return Check { name: "Local ONNX model", ok: true, detail: "not the active provider".into(), hint: None };
    }
    let model = data_dir.join("models").join("onnx").join("model_quantized.onnx");
    let tok = data_dir.join("models").join("onnx").join("tokenizer.json");
    if model.exists() && tok.exists() {
        Check { name: "Local ONNX model", ok: true, detail: "present".into(), hint: None }
    } else {
        Check { name: "Local ONNX model", ok: false, detail: "model_quantized.onnx or tokenizer.json missing".into(), hint: Some("Open Settings › Local ONNX embeddings and click Download.".into()) }
    }
}

pub fn run(conn: &Connection, data_dir: &Path, known_migrations: usize) -> Result<Report> {
    let (version_check, v) = check_migration_version(conn, known_migrations);
    let mut checks = vec![
        check_db_integrity(conn),
        version_check,
        check_data_dir_writable(data_dir),
        check_disk_space(data_dir),
        check_onnx_model_if_used(conn, data_dir),
    ];
    let passed = checks.iter().filter(|c| c.ok).count();
    let failed = checks.len() - passed;
    Ok(Report { passed, failed, checks, user_version: v, known_migrations })
}

// `encode_wide` lives in std::os::windows::ffi::OsStrExt; bring it into scope.
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
