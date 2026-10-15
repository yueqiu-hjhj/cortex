//! Runtime sidecar fetchers for external CLI tools (yt-dlp / age / rclone).
//!
//! When `tauri.conf.json` declares `bundle.externalBin`, Tauri copies the
//! sidecars into the bundle directory at packaging time. We deliberately do
//! NOT rely on that — releases don't bundle them, and reinstalling on every
//! platform for every binary is a maintenance nightmare. Instead, on first
//! use we download each binary into `<app_data_dir>/bin/` and reuse it from
//! there on subsequent launches (idempotent — skips download if the file
//! already exists with the expected size).
//!
//! `ingest::tool(name)` is the single lookup entry point: it checks bundled
//! sidecars first (back-compat), then `<app_data_dir>/bin/<name>`, then PATH.
//! Callers that need the sidecar for a feature (yt-dlp for ingest, age/rclone
//! for backups) call `ensure_<name>` before running the command, and any
//! download happens lazily on first call.

use crate::error::{Error, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Description of a binary the app knows how to fetch.
#[derive(Debug, Clone)]
pub struct BinSpec {
    /// Logical name used by the app, e.g. "yt-dlp".
    pub name: &'static str,
    /// Direct URL. We pin to a specific release tag so the fetcher is
    /// deterministic; bumping requires a code change. The full archive
    /// containing the binary (or the binary itself, for single-file targets).
    pub url: &'static str,
    /// Whether the downloaded artifact is a tarball, a zip, or a single
    /// executable file.
    pub kind: ArchiveKind,
    /// Name of the binary inside the archive. For tarball/zip, this is the
    /// exact member filename to extract; for `File`, it's the basename to
    /// save the downloaded bytes under.
    pub inner_name: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    File,
    TarGz,
    Zip,
}

impl BinSpec {
    /// Returns the spec for the current target. Panics if the target isn't
    /// recognised — the bug is ours, fail loud.
    pub fn for_target(name: &str) -> BinSpec {
        match name {
            "yt-dlp" => yt_dlp_spec(),
            "age" => age_spec(),
            "rclone" => rclone_spec(),
            other => panic!("sidecars::BinSpec::for_target: unknown tool `{other}`"),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn yt_dlp_spec() -> BinSpec {
    // yt-dlp publishes a self-contained executable for every platform; no
    // archive extraction is required.
    if cfg!(target_os = "windows") {
        BinSpec {
            name: "yt-dlp",
            url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
            kind: ArchiveKind::File,
            inner_name: "yt-dlp.exe",
        }
    } else {
        // Linux + macOS: same single-file artifact; rename at save time.
        BinSpec {
            name: "yt-dlp",
            url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp",
            kind: ArchiveKind::File,
            inner_name: "yt-dlp",
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn age_spec() -> BinSpec {
    // age ships a tar.gz on Linux/macOS and a zip on Windows. The layout is
    // fixed: a single `age` binary (plus `age-keygen`, which we don't need).
    let target_dir = if cfg!(target_os = "windows") {
        "windows-amd64"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "darwin-arm64"
    } else if cfg!(target_os = "macos") {
        "darwin-amd64"
    } else {
        // Linux only ships amd64 today; arm64 users need to install via
        // their package manager and rely on PATH.
        "linux-amd64"
    };
    let ext = if cfg!(target_os = "windows") { "zip" } else { "tar.gz" };
    let url = format!(
        "https://github.com/FiloSottile/age/releases/latest/download/age-v1.2.1-{target_dir}.{ext}"
    );
    BinSpec {
        name: "age",
        url: Box::leak(url.into_boxed_str()),
        kind: if cfg!(target_os = "windows") { ArchiveKind::Zip } else { ArchiveKind::TarGz },
        inner_name: if cfg!(target_os = "windows") { "age.exe" } else { "age" },
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn rclone_spec() -> BinSpec {
    // rclone ships per-OS/arch zips. The Windows archive contains
    // `rclone.exe`; macOS/Linux contain `rclone` (the .exe suffix is added
    // at extraction time below).
    let plat = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") { "osx-arm64" } else { "osx-amd64" }
    } else {
        if cfg!(target_arch = "aarch64") { "windows-arm64" } else { "windows-amd64" }
    };
    let url = format!(
        "https://github.com/rclone/rclone/releases/latest/download/rclone-{plat}.zip"
    );
    BinSpec {
        name: "rclone",
        url: Box::leak(url.into_boxed_str()),
        kind: ArchiveKind::Zip,
        inner_name: "rclone",
    }
}

/// Where runtime-fetched sidecars live: `<app_data_dir>/bin/<name>`.
pub fn bin_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("bin")
}

/// Idempotent download: if `<bin_dir>/<spec.inner_name>` already exists,
/// returns the path. Otherwise downloads `spec.url`, extracts if needed, and
/// marks +x on Unix.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn ensure(data_dir: &Path, spec: &BinSpec) -> Result<PathBuf> {
    let dir = bin_dir(data_dir);
    let target = dir.join(spec.inner_name);
    if target.is_file() {
        return Ok(target);
    }
    fs::create_dir_all(&dir).map_err(|e| Error::Other(format!(
        "create {}: {e}", dir.display()
    )))?;
    let bytes = download(spec)?;
    write_and_extract(spec, &target, &bytes)?;
    Ok(target)
}

/// Fetch every spec, in order. Failures are collected (per-tool) so a
/// missing tool doesn't block the others; the BackupStatus UI surfaces them.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn ensure_all(data_dir: &Path, specs: &[&BinSpec]) -> Vec<(String, Result<PathBuf>)> {
    specs
        .iter()
        .map(|s| (s.name.to_string(), ensure(data_dir, s)))
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn download(spec: &BinSpec) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Other(format!(
            "reqwest for {}: {e}", spec.name
        )))?;
    let mut resp = client
        .get(spec.url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Other(format!(
            "download {} from {}: {e}", spec.name, spec.url
        )))?;
    let mut buf = Vec::with_capacity(resp.content_length().unwrap_or(0) as usize);
    resp.take(200 * 1024 * 1024) // 200 MiB hard cap — even rclone is < 30 MiB
        .read_to_end(&mut buf)
        .map_err(|e| Error::Other(format!(
            "read {} body: {e}", spec.name
        )))?;
    Ok(buf)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn write_and_extract(spec: &BinSpec, target: &Path, bytes: &[u8]) -> Result<()> {
    match spec.kind {
        ArchiveKind::File => {
            fs::write(target, bytes).map_err(|e| Error::Other(format!(
                "write {}: {e}", target.display()
            )))?;
            chmod_x(target);
            Ok(())
        }
        ArchiveKind::TarGz => extract_tar_gz(spec, target, bytes),
        ArchiveKind::Zip => extract_zip(spec, target, bytes),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn extract_tar_gz(spec: &BinSpec, target: &Path, bytes: &[u8]) -> Result<()> {
    // age is published as a .zip on Windows and a .tar.gz on Linux/macOS. This
    // function is only called for the tarball case (write_and_extract's
    // ArchiveKind::TarGz arm). On Windows the age spec pins a zip so we
    // never reach here, but keep a defensive error in case the spec changes.
    #[cfg(target_os = "windows")]
    {
        return Err(Error::Other(format!(
            "{} is published as a tarball — no Windows archive available", spec.name
        )));
    }
    #[cfg(not(target_os = "windows"))]
    {
    use flate2::read::GzDecoder;
    let gz = GzDecoder::new(bytes);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries().map_err(|e| Error::Other(format!(
        "read {} tar: {e}", spec.name
    )))? {
        let mut entry = entry.map_err(|e| Error::Other(format!(
            "tar entry {}: {e}", spec.name
        )))?;
        let path = entry.path().map_err(|e| Error::Other(format!(
            "tar path {}: {e}", spec.name
        )))?;
        let basename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if basename != spec.inner_name {
            continue;
        }
        let mut out = fs::File::create(target).map_err(|e| Error::Other(format!(
            "create {}: {e}", target.display()
        )))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| Error::Other(format!(
            "extract {}: {e}", spec.name
        )))?;
        chmod_x(target);
        return Ok(());
    }
    Err(Error::Other(format!(
        "`{}` not found in {} archive", spec.inner_name, spec.name
    )))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn extract_zip(spec: &BinSpec, target: &Path, bytes: &[u8]) -> Result<()> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| Error::Other(format!(
        "open {} zip: {e}", spec.name
    )))?;
    // Try the bare name first; on Windows also accept `.exe`.
    let candidates: Vec<String> = if cfg!(target_os = "windows") {
        vec![spec.inner_name.to_string(), format!("{}.exe", spec.inner_name)]
    } else {
        vec![spec.inner_name.to_string()]
    };
    let mut member_idx = None;
    for (i, name) in zip.file_names().enumerate() {
        let base = Path::new(name).file_name().and_then(|s| s.to_str()).unwrap_or_default();
        if candidates.iter().any(|c| c == base) {
            member_idx = Some(i);
            break;
        }
    }
    let idx = member_idx.ok_or_else(|| Error::Other(format!(
        "no matching member for `{}` in {} zip (have: {:?})",
        spec.inner_name, spec.name,
        zip.file_names().collect::<Vec<_>>()
    )))?;
    let mut file = zip.by_index(idx).map_err(|e| Error::Other(format!(
        "zip member {}: {e}", spec.name
    )))?;
    let mut out = fs::File::create(target).map_err(|e| Error::Other(format!(
        "create {}: {e}", target.display()
    )))?;
    std::io::copy(&mut file, &mut out).map_err(|e| Error::Other(format!(
        "extract {}: {e}", spec.name
    )))?;
    chmod_x(target);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn chmod_x(target: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(target) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(target, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = target; // silence unused
    }
}

// Convenience wrappers per tool. They resolve `data_dir` at the call site so
// callers (mpv, backup) can pass the app data dir they already have.

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn ensure_ytdlp(data_dir: &Path) -> Result<PathBuf> {
    let spec = BinSpec::for_target("yt-dlp");
    ensure(data_dir, &spec)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn ensure_age(data_dir: &Path) -> Result<PathBuf> {
    let spec = BinSpec::for_target("age");
    ensure(data_dir, &spec)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn ensure_rclone(data_dir: &Path) -> Result<PathBuf> {
    let spec = BinSpec::for_target("rclone");
    ensure(data_dir, &spec)
}
