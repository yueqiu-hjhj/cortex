//! Ad-free YouTube (and arbitrary URL) audio playback via a headless `mpv`
//! sidecar.
//!
//! Why mpv: it plays both normal videos and livestreams, handles the HLS that
//! YouTube livestreams resolve to, and — fed through `yt-dlp` — bypasses the
//! ad-insertion that an embedded YouTube player would show. We run ONE long-
//! lived `mpv --idle` process with no video output and drive it over its JSON
//! IPC socket (load a URL, pause/resume, set volume). The process outlives
//! individual commands so playback is continuous across the app.
//!
//! `yt-dlp` is auto-downloaded into the app data dir on first use (all
//! platforms), and so is `mpv` on Windows (shinchiro build — GitHub API
//! resolves the latest `mpv-x86_64-*.7z` release asset, sidecars::ensure
//! extracts `mpv.exe`). On Linux/macOS mpv and ffmpeg are expected on PATH
//! (detected + surfaced in Settings). IPC is a Unix socket on Linux/macOS
//! and mpv's named pipe on Windows.

use crate::error::{Error, Result};
use crate::models::MediaTools;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// The single long-lived mpv child process, if running.
fn mpv_holder() -> &'static Mutex<Option<Child>> {
    static MPV: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    MPV.get_or_init(|| Mutex::new(None))
}

/// Kill the mpv sidecar — called on app exit so no headless mpv process lingers
/// (on Linux a child isn't auto-reaped when the parent exits).
pub fn shutdown() {
    if let Ok(mut guard) = mpv_holder().lock() {
        if let Some(child) = guard.as_mut() {
            // mpv is spawned as its own process-group leader (see ensure_mpv), so
            // for a YouTube livestream the continuously-running `yt-dlp` feeder
            // shares mpv's group. SIGKILL the whole group so the downloader can't
            // linger (and keep streaming) after the app closes — killing mpv alone
            // would orphan yt-dlp.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        *guard = None;
    }
}

fn data_dir(app: &AppHandle) -> Result<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| Error::Other(e.to_string()))
}

/// Quit any mpv orphaned by a previous session. If the app crashed, shutdown()
/// never ran — the old mpv (own process group) keeps streaming forever, and a
/// fresh launch used to delete its socket and spawn a SECOND mpv next to it.
/// A live listener on the leftover socket can only be that orphan: tell it to
/// quit before clearing the socket. Called once at app startup.
pub fn cleanup_stale(app: &AppHandle) {
    let Ok(dir) = data_dir(app) else { return };
    let socket = socket_path(&dir);
    if socket_ready(&socket) {
        let _ = ipc(&socket, &json!({ "command": ["quit"] }));
    }
    #[cfg(not(target_os = "windows"))]
    let _ = std::fs::remove_file(&socket);
}

/// The IPC client endpoint: a Unix socket file on Linux/macOS; on Windows
/// mpv's --input-ipc-server creates a named pipe at `\\.\pipe\<name>` and the
/// std fs API can open that path directly as a duplex file.
fn socket_path(dir: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let _ = dir;
        PathBuf::from(r"\\.\pipe\cortex-mpv")
    }
    #[cfg(not(target_os = "windows"))]
    {
        dir.join("mpv.sock")
    }
}

/// The value handed to mpv's --input-ipc-server. On Windows this is the bare
/// pipe name (mpv prefixes \\.\pipe\ itself); elsewhere the socket file path.
fn socket_arg(dir: &Path) -> String {
    #[cfg(target_os = "windows")]
    {
        let _ = dir;
        "cortex-mpv".to_string()
    }
    #[cfg(not(target_os = "windows"))]
    {
        dir.join("mpv.sock").display().to_string()
    }
}

/// Is the IPC endpoint live? Unix: the socket file exists. Windows: the named
/// pipe accepts a client connection (a real open — also proves mpv serves it).
fn socket_ready(socket: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(socket)
            .is_ok()
    }
    #[cfg(not(target_os = "windows"))]
    {
        socket.exists()
    }
}

/// Resolve the mpv binary. Lookup order:
/// 1. `<data>/bin/mpv(.exe)` — runtime-fetched earlier, or a bundled sidecar,
///    or PATH (ingest::tool_in's chain).
/// 2. Windows only: download the shinchiro build once — GitHub API resolves
///    the latest `mpv-x86_64-*.7z` release asset and sidecars::ensure extracts
///    `mpv.exe` into `<data>/bin/`. Runs WITHOUT the db lock held (short lock
///    reads the url override only) and OFF the main thread (see youtube_play).
/// 3. Linux/macOS: expect the distro/brew package; clear error if missing.
fn resolve_mpv(app: &AppHandle, dir: &Path) -> Result<PathBuf> {
    if let Some(p) = crate::ingest::tool_in("mpv", Some(dir)) {
        return Ok(PathBuf::from(p));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Err(Error::Other(
            "couldn't find mpv — install it (e.g. `brew install mpv` / `sudo pacman -S mpv`) and retry."
                .into(),
        ))
    }
    #[cfg(target_os = "windows")]
    {
        let url_override = {
            let state = app.state::<crate::db::AppState>();
            let c = state.db.lock().unwrap();
            crate::repo::get_setting(&c, "sidecar_mpv_url").ok().flatten()
        };
        let url: String = match url_override.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => u.to_string(),
            None => crate::sidecars::latest_mpv_url().map_err(|e| Error::Other(format!(
                "{e}\n（无法自动获取 mpv —— 可在设置里加 sidecar_mpv_url 指向 shinchiro mpv 构建的 7z 直链镜像。）"
            )))?,
        };
        let spec = crate::sidecars::BinSpec {
            name: "mpv",
            url: Box::leak(url.into_boxed_str()),
            kind: crate::sidecars::ArchiveKind::SevenZ,
            inner_name: "mpv",
        };
        crate::sidecars::ensure(dir, &spec)
    }
}

/// Resolve yt-dlp for real playback. Lookup order:
/// 1. Bundled Tauri sidecar (legacy bundle.externalBin path).
/// 2. Runtime-fetched copy in `<data>/bin/yt-dlp` (sidecars::ensure downloads
///    on first miss; cross-platform).
/// 3. A copy already on `$PATH`.
/// Idempotent. The possible download runs WITHOUT the db lock held (short lock
/// reads the url override only) and OFF the main thread — see youtube_play.
fn ensure_ytdlp_unlocked(app: &AppHandle, dir: &Path) -> Result<PathBuf> {
    if let Some(p) = crate::ingest::tool_in("yt-dlp", Some(dir)) {
        return Ok(PathBuf::from(p));
    }
    let url_override = {
        let state = app.state::<crate::db::AppState>();
        let c = state.db.lock().unwrap();
        crate::repo::get_setting(&c, "sidecar_ytdlp_url").ok().flatten()
    };
    let mut spec = crate::sidecars::BinSpec::for_target("yt-dlp");
    if let Some(u) = url_override.as_deref() {
        if !u.trim().is_empty() {
            spec = spec.with_url(u);
        }
    }
    crate::sidecars::ensure(dir, &spec)
}

/// Start mpv (idle, no video) if it isn't already running, listening on the IPC
/// endpoint and configured to find our yt-dlp. Waits briefly for the endpoint.
/// `client_socket` is the endpoint we drive IPC through; `server_arg` is the
/// value passed to --input-ipc-server (bare pipe name on Windows, file path
/// elsewhere).
fn ensure_mpv(
    client_socket: &Path,
    server_arg: &str,
    mpv_bin: &Path,
    ytdlp: &Path,
    volume: u8,
) -> Result<()> {
    let holder = mpv_holder();
    let mut guard = holder.lock().unwrap();
    if let Some(child) = guard.as_mut() {
        // Still alive? Reuse it.
        if matches!(child.try_wait(), Ok(None)) {
            return Ok(());
        }
        *guard = None;
    }
    // A leftover endpoint with a live listener is an orphan from a crashed
    // session — quit it so two mpvs never play at once, then clear the socket.
    if socket_ready(client_socket) {
        let _ = ipc(client_socket, &json!({ "command": ["quit"] }));
        #[cfg(not(target_os = "windows"))]
        let _ = std::fs::remove_file(client_socket);
    }
    let mut cmd = crate::ingest::new_cmd(mpv_bin);
    cmd.arg("--no-video")
        .arg("--idle=yes")
        .arg("--no-terminal")
        .arg("--really-quiet")
        // Buffer ahead so a resolved direct stream starts (and stays) smooth.
        .arg("--cache=yes")
        .arg("--demuxer-readahead-secs=10")
        .arg(format!("--volume={volume}"))
        .arg(format!("--input-ipc-server={server_arg}"))
        .arg(format!(
            "--script-opts=ytdl_hook-ytdl_path={}",
            ytdlp.display()
        ));
    // Put mpv in its own process group so any `yt-dlp` feeder it spawns shares the
    // group and can be torn down together on shutdown() (see there).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .map_err(|e| Error::Other(format!("couldn't start mpv ({}): {e}", mpv_bin.display())))?;
    *guard = Some(child);
    drop(guard);
    for _ in 0..60 {
        if socket_ready(client_socket) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(Error::Other(format!(
        "mpv started but its IPC endpoint never appeared ({})",
        client_socket.display()
    )))
}

/// Send one JSON command line to mpv's IPC socket.
#[cfg(unix)]
fn ipc(socket: &Path, cmd: &Value) -> Result<()> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket)
        .map_err(|e| Error::Other(format!("mpv not reachable: {e}")))?;
    let mut line = serde_json::to_vec(cmd)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    Ok(())
}

/// Windows named-pipe client: mpv creates `\\.\pipe\<name>` and the std fs
/// API opens that path as a duplex file — no extra Win32 bindings needed.
#[cfg(windows)]
fn ipc(socket: &Path, cmd: &Value) -> Result<()> {
    use std::io::Write;
    let mut stream = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(socket)
        .map_err(|e| Error::Other(format!("mpv not reachable: {e}")))?;
    let mut line = serde_json::to_vec(cmd)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn ipc(_socket: &Path, _cmd: &Value) -> Result<()> {
    Err(Error::Unsupported(
        "URL audio streaming requires Unix-socket or named-pipe IPC.".into(),
    ))
}

/// Best-effort IPC (used for pause/stop/volume): if mpv isn't running, there's
/// nothing to control, so a connect failure is silently ignored.
fn ipc_soft(socket: &Path, cmd: &Value) {
    if socket_ready(socket) {
        let _ = ipc(socket, cmd);
    }
}

// ---- commands ----------------------------------------------------------

/// Detect the external tools the YouTube-audio engine relies on.
#[tauri::command]
pub fn media_tools_status(app: AppHandle) -> Result<MediaTools> {
    let dir = data_dir(&app)?;
    let downloaded = dir.join("bin").join("yt-dlp");
    Ok(MediaTools {
        mpv: crate::ingest::tool_in("mpv", Some(dir.as_path())).is_some(),
        ffmpeg: crate::ingest::tool_in("ffmpeg", Some(dir.as_path())).is_some(),
        ytdlp: crate::ingest::tool_in("yt-dlp", Some(dir.as_path())).is_some(),
        ytdlp_path: downloaded.display().to_string(),
    })
}

/// Stream a URL's audio (YouTube video/livestream or any mpv-playable URL),
/// ad-free, starting playback at `volume` (0–100). Downloads yt-dlp on first use.
///
/// ASYNC + spawn_blocking on purpose: sync commands execute INLINE on the
/// WebView2 UI thread, so the possible yt-dlp download here (up to the 180s
/// client timeout on a bad network) used to freeze the whole window — the
/// "opened the app and it says 未响应" bug. The heavy work runs on a blocking
/// pool thread and the download happens WITHOUT the db lock (a short lock only
/// reads the url override setting).
#[tauri::command]
pub async fn youtube_play(app: AppHandle, url: String, volume: u8) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || youtube_play_blocking(app, url, volume))
        .await
        .map_err(|e| Error::Other(format!("youtube_play join: {e}")))?
}

fn youtube_play_blocking(app: AppHandle, url: String, volume: u8) -> Result<()> {
    let dir = data_dir(&app)?;
    let socket = socket_path(&dir);
    let ytdlp = ensure_ytdlp_unlocked(&app, &dir)?;
    let mpv_bin = resolve_mpv(&app, &dir)?;
    ensure_mpv(&socket, &socket_arg(&dir), &mpv_bin, &ytdlp, volume)?;
    // Stations are YouTube watch URLs; mpv's ytdl hook normally re-runs yt-dlp on
    // every load to resolve the direct googlevideo stream (several seconds). We
    // cache that resolved URL per station so repeat plays start near-instantly.
    // Playlist/radio URLs are skipped (they need the hook for playlist behaviour).
    let loaded_url = match cache_lookup_fresh(&dir, &url) {
        // FRESH hit: hand mpv the direct stream — no yt-dlp on the play path.
        Some(direct) => {
            ipc(
                &socket,
                &json!({ "command": ["loadfile", direct, "replace"] }),
            )?;
            // Re-resolve in the background so the cached URL stays fresh for next time.
            spawn_resolve_and_cache(dir.clone(), ytdlp.clone(), url.clone());
            true
        }
        None => false,
    };
    if !loaded_url {
        // Miss: fall back to the original URL (mpv's ytdl hook resolves it — the
        // user waits no longer than before) and prime the cache so the SECOND play
        // is instant. This also auto-covers user-added stations on first play.
        ipc(
            &socket,
            &json!({ "command": ["loadfile", url, "replace"] }),
        )?;
        spawn_resolve_and_cache(dir.clone(), ytdlp.clone(), url.clone());
    }
    ipc(&socket, &json!({ "command": ["set_property", "pause", false] }))?;
    ipc(
        &socket,
        &json!({ "command": ["set_property", "volume", volume as i64] }),
    )?;
    Ok(())
}

#[tauri::command]
pub fn youtube_pause(app: AppHandle) -> Result<()> {
    let dir = data_dir(&app)?;
    ipc_soft(&socket_path(&dir), &json!({ "command": ["set_property", "pause", true] }));
    Ok(())
}

#[tauri::command]
pub fn youtube_resume(app: AppHandle) -> Result<()> {
    let dir = data_dir(&app)?;
    ipc_soft(&socket_path(&dir), &json!({ "command": ["set_property", "pause", false] }));
    Ok(())
}

/// Stop playback but keep mpv idle (ready for the next station).
#[tauri::command]
pub fn youtube_stop(app: AppHandle) -> Result<()> {
    let dir = data_dir(&app)?;
    ipc_soft(&socket_path(&dir), &json!({ "command": ["stop"] }));
    Ok(())
}

#[tauri::command]
pub fn youtube_set_volume(app: AppHandle, volume: u8) -> Result<()> {
    let dir = data_dir(&app)?;
    ipc_soft(
        &socket_path(&dir),
        &json!({ "command": ["set_property", "volume", volume as i64] }),
    );
    Ok(())
}

/// Pre-resolve a batch of station URLs in the background so their first play is
/// instant. Returns immediately; ONE detached thread walks the list, resolving
/// only stale/missing non-playlist entries (cheap no-op when all are fresh).
/// Best-effort: any failure (no yt-dlp, no network) just leaves the cache as-is.
///
/// The frontend calls this at STARTUP, so it must never trigger the yt-dlp
/// download — a fresh install used to download 18 MB here ON THE MAIN THREAD
/// (sync command) while holding the db lock: the whole window showed 未响应
/// for minutes. If yt-dlp isn't resolvable yet, skip silently; the first
/// user-initiated play fetches it off-thread instead.
#[tauri::command]
pub fn youtube_prewarm(app: AppHandle, urls: Vec<String>) -> Result<()> {
    let dir = data_dir(&app)?;
    // Effect-free lookup only (bundled sidecar → bin/ → PATH). NEVER download.
    let Some(ytdlp) = crate::ingest::tool_in("yt-dlp", Some(dir.as_path())).map(PathBuf::from)
    else {
        return Ok(());
    };
    std::thread::spawn(move || {
        for url in urls {
            if is_playlist_url(&url) {
                continue; // playlists use mpv's ytdl hook; never cached
            }
            if cache_lookup_fresh(&dir, &url).is_some() {
                continue; // already fresh — nothing to do
            }
            if let Some(direct) = resolve_direct(&ytdlp, &url) {
                cache_store(&dir, &url, &direct);
            }
        }
    });
    Ok(())
}

// ---- resolved-stream cache --------------------------------------------------
//
// Each station URL is a YouTube watch page; mpv's ytdl hook shells out to yt-dlp
// to turn it into a direct googlevideo stream on every load (several seconds).
// We persist that resolved URL so repeat plays skip yt-dlp. Direct URLs expire
// (~6h) and can be IP-bound, so entries are only trusted for 3 hours.

/// How long a cached direct URL is trusted before we re-resolve it.
const CACHE_FRESH_MS: i64 = 3 * 60 * 60 * 1000; // 3 hours

/// Playlist/radio URLs (`list=`) must keep going through mpv's ytdl hook so
/// playlist/next-track behaviour works — never cache or resolve these.
fn is_playlist_url(url: &str) -> bool {
    url.contains("list=")
}

fn stream_cache_path(dir: &Path) -> PathBuf {
    dir.join("stream-cache.json")
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One cached entry: a resolved direct stream URL and when it was resolved.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct CacheEntry {
    direct: String,
    resolved_at_ms: i64,
}

/// Load the whole cache. A missing or corrupt file yields an empty map — the
/// cache is a pure optimisation and must never be a failure path.
fn cache_load(dir: &Path) -> std::collections::HashMap<String, CacheEntry> {
    std::fs::read_to_string(stream_cache_path(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the cache best-effort; write errors are intentionally ignored.
fn cache_save(dir: &Path, map: &std::collections::HashMap<String, CacheEntry>) {
    if let Ok(s) = serde_json::to_string(map) {
        let _ = std::fs::write(stream_cache_path(dir), s);
    }
}

/// Return the cached direct URL for `url` only if it's a non-playlist entry that
/// resolved within the freshness window; otherwise `None`.
fn cache_lookup_fresh(dir: &Path, url: &str) -> Option<String> {
    if is_playlist_url(url) {
        return None;
    }
    let entry = cache_load(dir).remove(url)?;
    if now_ms().saturating_sub(entry.resolved_at_ms) < CACHE_FRESH_MS {
        Some(entry.direct)
    } else {
        None
    }
}

/// Insert/refresh one entry (skipping playlist URLs), preserving the rest.
fn cache_store(dir: &Path, url: &str, direct: &str) {
    if is_playlist_url(url) {
        return;
    }
    let mut map = cache_load(dir);
    map.insert(
        url.to_string(),
        CacheEntry {
            direct: direct.to_string(),
            resolved_at_ms: now_ms(),
        },
    );
    cache_save(dir, &map);
}

/// Resolve a YouTube/URL to its direct best-audio stream via yt-dlp's `-g`
/// (get-url). Returns the first stdout line, or `None` on any failure. Playlist
/// URLs return `None` (they must not be flattened to a single stream).
fn resolve_direct(ytdlp: &Path, url: &str) -> Option<String> {
    if is_playlist_url(url) {
        return None;
    }
    let out = crate::ingest::new_cmd(ytdlp)
        .arg("-g")
        .arg("-f")
        .arg("bestaudio")
        .arg("--no-playlist")
        .arg(url)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// Resolve `url` in a detached thread and cache the result. Used to prime the
/// cache after a miss and to refresh it after a fresh hit. Best-effort.
fn spawn_resolve_and_cache(dir: PathBuf, ytdlp: PathBuf, url: String) {
    if is_playlist_url(&url) {
        return;
    }
    std::thread::spawn(move || {
        if let Some(direct) = resolve_direct(&ytdlp, &url) {
            cache_store(&dir, &url, &direct);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_urls_are_never_cached() {
        assert!(is_playlist_url(
            "https://www.youtube.com/watch?v=x&list=PLabc"
        ));
        assert!(is_playlist_url(
            "https://www.youtube.com/watch?v=y&list=RDy&start_radio=1"
        ));
        assert!(!is_playlist_url("https://www.youtube.com/watch?v=z"));
    }

    #[test]
    fn store_then_fresh_lookup_roundtrips() {
        let dir = std::env::temp_dir().join(format!("cortex-stream-cache-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = "https://www.youtube.com/watch?v=roundtrip";
        cache_store(&dir, url, "https://direct.example/stream.m4a");
        assert_eq!(
            cache_lookup_fresh(&dir, url).as_deref(),
            Some("https://direct.example/stream.m4a")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_entries_are_not_returned() {
        let dir = std::env::temp_dir().join(format!("cortex-stream-stale-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = "https://www.youtube.com/watch?v=stale";
        let mut map = std::collections::HashMap::new();
        map.insert(
            url.to_string(),
            CacheEntry {
                direct: "https://direct.example/old.m4a".into(),
                resolved_at_ms: now_ms() - CACHE_FRESH_MS - 1,
            },
        );
        cache_save(&dir, &map);
        assert_eq!(cache_lookup_fresh(&dir, url), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_cache_file_yields_empty_map() {
        let dir = std::env::temp_dir().join(format!("cortex-stream-corrupt-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(stream_cache_path(&dir), "{ not json").unwrap();
        assert!(cache_load(&dir).is_empty());
        assert_eq!(cache_lookup_fresh(&dir, "https://x/watch?v=a"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn playlist_store_is_a_noop() {
        let dir = std::env::temp_dir().join(format!("cortex-stream-pl-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = "https://www.youtube.com/watch?v=p&list=PLx";
        cache_store(&dir, url, "https://direct.example/should-not-store");
        assert!(cache_load(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
