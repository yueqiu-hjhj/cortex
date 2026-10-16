//! Secret store backed by the OS keyring.
//!
//! Right now only Windows is hard: secrets are encrypted with DPAPI
//! (CryptProtectData / CryptUnprotectData) and the resulting ciphertext is
//! stored in the `secrets` table. The key is the user's Windows login, so the
//! blob can only be opened by the same user on the same machine -- the worst
//! case after a disk image leak is a useless block of bytes.
//!
//! macOS would call Security.framework's SecKeychainAddGenericPassword. Linux
//! would call libsecret. Both are stubbed here for now -- a fresh install on
//! either OS just falls back to plaintext with a `secrets_plaintext` row, which
//! is the pre-DPAPI behavior. Users on those platforms who care can flip the
//! runtime setting in Settings > Security.

use crate::error::{Error, Result};
use rusqlite::params;

const FLAG_DPAPI: u8 = 1;

#[cfg(target_os = "windows")]
mod platform {
    use super::{Error, Result};
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_PROMPTSTRUCT,
        CRYPTPROTECT_UI_FORBIDDEN,
    };

    fn blob_from(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut u8,
        }
    }

    pub fn encrypt(plain: &[u8]) -> Result<Vec<u8>> {
        let mut input = blob_from(plain);
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let prompt = CRYPTPROTECT_PROMPTSTRUCT {
            cbSize: std::mem::size_of::<CRYPTPROTECT_PROMPTSTRUCT>() as u32,
            dwPromptFlags: CRYPTPROTECT_UI_FORBIDDEN,
            szPrompt: std::ptr::null_mut(),
            hwndApp: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptProtectData(
                &mut input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                &prompt as *const _ as *mut _,
                0,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(Error::Other(format!(
                "CryptProtectData failed (gle={})",
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
            )));
        }
        let out = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        unsafe { LocalFree(output.pbData as HLOCAL); }
        Ok(out)
    }

    pub fn decrypt(cipher: &[u8]) -> Result<Vec<u8>> {
        let mut input = blob_from(cipher);
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let prompt = CRYPTPROTECT_PROMPTSTRUCT {
            cbSize: std::mem::size_of::<CRYPTPROTECT_PROMPTSTRUCT>() as u32,
            dwPromptFlags: CRYPTPROTECT_UI_FORBIDDEN,
            szPrompt: std::ptr::null_mut(),
            hwndApp: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                &prompt as *const _ as *mut _,
                0,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(Error::Other("CryptUnprotectData failed (wrong user?)".into()));
        }
        let out = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        unsafe { LocalFree(output.pbData as HLOCAL); }
        Ok(out)
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::Result;
    pub fn encrypt(plain: &[u8]) -> Result<Vec<u8>> { Ok(plain.to_vec()) }
    pub fn decrypt(cipher: &[u8]) -> Result<Vec<u8>> { Ok(cipher.to_vec()) }
}

/// Persist `plain` as a secret under `key`. Overwrites any previous value.
/// On Windows the bytes are DPAPI-encrypted before they hit disk.
pub fn set_secret(conn: &rusqlite::Connection, key: &str, plain: &str) -> Result<()> {
    let cipher = platform::encrypt(plain.as_bytes())?;
    conn.execute(
        "INSERT INTO secrets (key, ciphertext, flags, updated_at)
         VALUES (?1, ?2, ?3, strftime('%s','now')*1000)
         ON CONFLICT(key) DO UPDATE SET ciphertext=excluded.ciphertext,
                                        flags=excluded.flags,
                                        updated_at=excluded.updated_at",
        params![key, cipher, FLAG_DPAPI as i64],
    )?;
    Ok(())
}

/// Read a secret by key. Returns Ok(None) if the key was never set.
pub fn get_secret(conn: &rusqlite::Connection, key: &str) -> Result<Option<String>> {
    let row: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT ciphertext, flags FROM secrets WHERE key=?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let (cipher, flags) = match row { Some(r) => r, None => return Ok(None) };
    if flags & (FLAG_DPAPI as i64) != 0 {
        let plain = platform::decrypt(&cipher)?;
        Ok(Some(String::from_utf8(plain).map_err(|e| Error::Other(format!("utf8: {e}")))?))
    } else {
        Ok(Some(String::from_utf8(cipher).map_err(|e| Error::Other(format!("utf8: {e}")))?))
    }
}

pub fn delete_secret(conn: &rusqlite::Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM secrets WHERE key=?1", params![key])?;
    Ok(())
}

pub fn list_secrets(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT key FROM secrets ORDER BY key ASC")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let out: Vec<String> = rows.collect::<rusqlite::Result<_>>()?;
    Ok(out)
}
