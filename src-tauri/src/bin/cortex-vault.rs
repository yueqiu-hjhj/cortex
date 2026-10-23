//! cortex-vault — encrypt the Cortex database at rest while the app is closed.
//!
//! The dive's last open gap (§5B #5) was "no encryption at rest". Full-disk
//! encryption (BitLocker / FileVault) is the primary answer and covers every
//! file; this tool covers the case where the disk is *not* encrypted but the
//! laptop leaves the house:
//!
//!   cortex-vault seal     cortex.db -> cortex.db.vault  (plaintext removed)
//!   cortex-vault open     cortex.db.vault -> cortex.db  (app can start)
//!   cortex-vault status   what exists right now
//!
//! Crypto: ChaCha20-Poly1305 with a key derived by Argon2id (19 MiB, t=2, p=1)
//! from a passphrase. The header (magic + salt + nonce) is authenticated as
//! AAD, so a tampered salt or nonce fails closed rather than decrypting to
//! garbage.
//!
//! Safety rails, because losing a passphrase loses the library:
//!   - seal verifies by decrypting its own output and comparing SHA-256
//!     before it deletes anything;
//!   - seal refuses while the database is in use (Cortex must be closed);
//!   - open refuses to overwrite an existing cortex.db unless --force;
//!   - open checks the SQLite header before replacing the database.
//!
//! Passphrase: `$CORTEX_VAULT_PASSPHRASE`, or piped on stdin. A terminal is
//! never prompted (so a stray invocation cannot hang a script).

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand_core::{OsRng, RngCore};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// 12 bytes: "CORTEXVAULT1".
const MAGIC: &[u8; 12] = b"CORTEXVAULT1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = 12 + SALT_LEN + NONCE_LEN;
const TAG_LEN: usize = 16;

#[derive(Default)]
struct Args {
    db: Option<String>,
    force: bool,
    remove_vault: bool,
    help: bool,
    action: Option<Action>,
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Seal,
    Open,
    Status,
}

fn print_usage() {
    println!(
        "cortex-vault — 关闭应用后对数据库做静态加密\n\n\
用法:\n  \
cortex-vault seal     把 cortex.db 加密成 cortex.db.vault，并删除明文\n  \
cortex-vault open     把 cortex.db.vault 还原成 cortex.db\n  \
cortex-vault status   查看当前有哪些文件\n\n\
选项:\n  \
--db <path>          指定 cortex.db（默认：应用数据目录）\n  \
--force              seal 覆盖已有 .vault / open 覆盖已有 cortex.db\n  \
--remove-vault       open 成功后删除 .vault（默认保留，作为备份）\n  \
--help               显示本帮助\n\n\
口令:\n  \
$CORTEX_VAULT_PASSPHRASE，或从 stdin 传入：\n  \
read -s PW; printf '%s' \"$PW\" | cortex-vault seal\n\n\
注意:\n  \
- seal 前必须关闭 Cortex（数据库不能被占用）。\n  \
- 口令丢失 = 数据无法恢复。先确认能 open 再删任何备份。\n  \
- 全盘加密（BitLocker / FileVault）仍然是首选；本工具用于未加密磁盘的补充保护。"
    );
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("✖ {e}");
            std::process::exit(2);
        }
    };
    if args.help || args.action.is_none() {
        print_usage();
        return;
    }
    if let Err(e) = run(&args) {
        eprintln!("✖ {e}");
        std::process::exit(1);
    }
}

fn parse_args(argv: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = argv;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "seal" => a.action = Some(Action::Seal),
            "open" => a.action = Some(Action::Open),
            "status" => a.action = Some(Action::Status),
            "--db" => a.db = Some(it.next().ok_or("--db 缺少值")?),
            "--force" => a.force = true,
            "--remove-vault" => a.remove_vault = true,
            "--help" | "-h" => a.help = true,
            other => return Err(format!("未知参数: {other}（--help 查看用法）")),
        }
    }
    Ok(a)
}

fn run(args: &Args) -> Result<(), String> {
    let db = resolve_db_path(args);
    let vault = vault_path(&db);
    match args.action {
        Some(Action::Seal) => seal(&db, &vault, args.force),
        Some(Action::Open) => open(&db, &vault, args.force, args.remove_vault),
        Some(Action::Status) => {
            status(&db, &vault);
            Ok(())
        }
        None => Ok(()),
    }
}

fn seal(db: &Path, vault: &Path, force: bool) -> Result<(), String> {
    if !db.is_file() {
        return Err(format!("找不到数据库: {}", db.display()));
    }
    if vault.exists() && !force {
        return Err(format!(
            "{} 已存在。确认要覆盖请加 --force。",
            vault.display()
        ));
    }
    checkpoint(db)?;
    let plain = std::fs::read(db).map_err(|e| format!("读取数据库失败: {e}"))?;
    let pass = passphrase()?;
    let blob = seal_bytes(&plain, &pass)?;

    // Verify before destroying anything: a wrong cipher or a truncated write
    // must never cost the library.
    let back = open_bytes(&blob, &pass)?;
    if sha256(&back) != sha256(&plain) {
        return Err("自校验失败（解密结果与原文不一致），已中止，未删除任何文件。".into());
    }

    write_atomic(vault, &blob)?;
    for suffix in ["-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", db.display()));
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| format!("删除 {} 失败: {e}", p.display()))?;
        }
    }
    std::fs::remove_file(db).map_err(|e| format!("删除明文数据库失败: {e}"))?;

    println!(
        "✔ 已加密：{} → {}（{} KB → {} KB）",
        db.display(),
        vault.display(),
        plain.len() / 1024,
        blob.len() / 1024
    );
    println!("  明文已删除。下次使用前运行：cortex-vault open");
    println!("  ⚠ 口令丢失将无法恢复，请确认你记住了它。");
    Ok(())
}

fn open(db: &Path, vault: &Path, force: bool, remove_vault: bool) -> Result<(), String> {
    if !vault.is_file() {
        return Err(format!("找不到 {}（先运行 cortex-vault seal）", vault.display()));
    }
    if db.exists() && !force {
        return Err(format!(
            "{} 已存在。确认要覆盖请加 --force。",
            db.display()
        ));
    }
    let blob = std::fs::read(vault).map_err(|e| format!("读取 {} 失败: {e}", vault.display()))?;
    let pass = passphrase()?;
    let plain = open_bytes(&blob, &pass)?;
    if !plain.starts_with(b"SQLite format 3\0") {
        return Err("解密成功但不是 SQLite 数据库（文件可能不是 Cortex 的库）。".into());
    }
    write_atomic(db, &plain)?;
    if remove_vault {
        std::fs::remove_file(vault).map_err(|e| format!("删除 .vault 失败: {e}"))?;
    }
    println!(
        "✔ 已还原：{} → {}（{} KB）",
        vault.display(),
        db.display(),
        plain.len() / 1024
    );
    if !remove_vault {
        println!("  .vault 已保留作为备份；确认无误后可手动删除。");
    }
    Ok(())
}

fn status(db: &Path, vault: &Path) {
    println!("数据库目录: {}", db.parent().unwrap_or(Path::new(".")).display());
    for (label, p) in [
        ("cortex.db", db.to_path_buf()),
        ("cortex.db-wal", PathBuf::from(format!("{}-wal", db.display()))),
        ("cortex.db-shm", PathBuf::from(format!("{}-shm", db.display()))),
        ("cortex.db.vault", vault.to_path_buf()),
    ] {
        match std::fs::metadata(&p) {
            Ok(m) => println!("  {label:<16} {} KB", m.len() / 1024),
            Err(_) => println!("  {label:<16} —"),
        }
    }
}

/// Checkpoint and prove nobody else holds the database. A running Cortex
/// would make the checkpoint return busy; we refuse rather than copy a
/// half-written database.
fn checkpoint(db: &Path) -> Result<(), String> {
    let conn = Connection::open(db).map_err(|e| format!("打开数据库失败: {e}"))?;
    conn.busy_timeout(Duration::from_millis(0))
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
        .map_err(|e| {
            format!("无法检查点数据库（Cortex 是否正在运行？请先关闭应用）: {e}")
        })?;
    conn.close()
        .map_err(|(_, e)| format!("关闭数据库失败: {e}"))?;
    Ok(())
}

fn passphrase() -> Result<String, String> {
    if let Ok(p) = std::env::var("CORTEX_VAULT_PASSPHRASE") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err(
            "未提供口令。用 $CORTEX_VAULT_PASSPHRASE 或从 stdin 传入：\n  \
             read -s PW; printf '%s' \"$PW\" | cortex-vault <seal|open>"
                .into(),
        );
    }
    let mut buf = String::new();
    stdin
        .read_to_string(&mut buf)
        .map_err(|e| format!("读取口令失败: {e}"))?;
    let p = buf.trim_end_matches(['\r', '\n']).to_string();
    if p.is_empty() {
        return Err("口令为空。".into());
    }
    Ok(p)
}

fn derive_key(pass: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    // 19 MiB, 2 passes, 1 lane — interactive-grade Argon2id.
    let params = Params::new(19 * 1024, 2, 1, Some(32)).map_err(|e| e.to_string())?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    a2.hash_password_into(pass.as_bytes(), salt, &mut key)
        .map_err(|e| format!("派生密钥失败: {e}"))?;
    Ok(key)
}

fn seal_bytes(plain: &[u8], pass: &str) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_b = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce_b);
    let key = derive_key(pass, &salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;

    let mut aad = Vec::with_capacity(HEADER_LEN);
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(&salt);
    aad.extend_from_slice(&nonce_b);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_b),
            Payload {
                msg: plain,
                aad: &aad,
            },
        )
        .map_err(|_| "加密失败".to_string())?;
    let mut out = aad;
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open_bytes(blob: &[u8], pass: &str) -> Result<Vec<u8>, String> {
    if blob.len() < HEADER_LEN + TAG_LEN {
        return Err("不是 cortex-vault 文件（长度不足）".into());
    }
    if blob[..12] != MAGIC[..] {
        return Err("不是 cortex-vault 文件（magic 不匹配）".into());
    }
    let salt = &blob[12..12 + SALT_LEN];
    let nonce_b = &blob[12 + SALT_LEN..HEADER_LEN];
    let ct = &blob[HEADER_LEN..];
    let key = derive_key(pass, salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|e| e.to_string())?;

    let mut aad = Vec::with_capacity(HEADER_LEN);
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(salt);
    aad.extend_from_slice(nonce_b);
    cipher
        .decrypt(
            Nonce::from_slice(nonce_b),
            Payload { msg: ct, aad: &aad },
        )
        .map_err(|_| "解密失败：口令错误或文件已损坏".to_string())
}

fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.tmp", dest.display()));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| format!("创建 {} 失败: {e}", tmp.display()))?;
        f.write_all(bytes)
            .map_err(|e| format!("写入 {} 失败: {e}", tmp.display()))?;
        f.sync_all().map_err(|e| format!("落盘失败: {e}"))?;
    }
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换 {} 失败: {e}", dest.display())
    })
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

fn vault_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}.vault", db.display()))
}

fn resolve_db_path(args: &Args) -> PathBuf {
    if let Some(d) = &args.db {
        return PathBuf::from(d);
    }
    if let Ok(d) = std::env::var("CORTEX_DB") {
        if !d.trim().is_empty() {
            return PathBuf::from(d.trim());
        }
    }
    if let Ok(d) = std::env::var("CORTEX_DATA_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d.trim()).join("cortex.db");
        }
    }
    platform_default_db()
}

#[cfg(target_os = "windows")]
fn platform_default_db() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let base = std::env::var("APPDATA").unwrap_or_else(|_| format!("{home}\\AppData\\Roaming"));
    PathBuf::from(base).join("app.study.cortex").join("cortex.db")
}

#[cfg(target_os = "macos")]
fn platform_default_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("app.study.cortex")
        .join("cortex.db")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_default_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let base = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    PathBuf::from(base).join("app.study.cortex").join("cortex.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_recovers_the_exact_bytes() {
        let plain = b"SQLite format 3\0 pretend this is a database".to_vec();
        let blob = seal_bytes(&plain, "correct horse battery staple").unwrap();
        assert!(blob.starts_with(MAGIC));
        let back = open_bytes(&blob, "correct horse battery staple").unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn wrong_passphrase_fails_closed() {
        let blob = seal_bytes(b"secret", "right").unwrap();
        assert!(open_bytes(&blob, "wrong").is_err());
    }

    #[test]
    fn tampered_header_fails_authentication() {
        let mut blob = seal_bytes(b"secret", "pw").unwrap();
        blob[12] ^= 0x01; // flip a salt bit
        assert!(open_bytes(&blob, "pw").is_err());
    }

    #[test]
    fn non_vault_input_is_rejected() {
        assert!(open_bytes(b"not a vault at all, definitely not", "pw").is_err());
    }

    #[test]
    fn each_seal_uses_a_fresh_salt_and_nonce() {
        let a = seal_bytes(b"same", "pw").unwrap();
        let b = seal_bytes(b"same", "pw").unwrap();
        assert_ne!(a[12..28], b[12..28], "salt must differ");
        assert_ne!(a[28..40], b[28..40], "nonce must differ");
        assert_ne!(a[40..], b[40..], "ciphertext must differ");
    }

    #[test]
    fn vault_path_is_a_sibling() {
        let p = vault_path(Path::new("/tmp/cortex.db"));
        assert_eq!(p.to_string_lossy(), "/tmp/cortex.db.vault");
    }
}
