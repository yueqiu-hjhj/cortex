//! Office document output and write-back (A2/A3).
//!
//! docx and pptx are hand-rolled OOXML over the existing `zip` crate: creating
//! clean documents needs only a handful of XML parts, and tracked changes /
//! comments are XML surgery on `word/document.xml` that no Rust crate does
//! well. xlsx (read+write) and pdf (edit) use calamine/rust_xlsxwriter/lopdf.

pub mod docx;
pub mod pdf;
pub mod pptx;
pub mod xlsx;

use crate::error::{Error, Result};
use std::io::{Read, Write};
use std::path::Path;

/// Escape text for XML element content and attribute values, dropping control
/// characters that are illegal in XML 1.0 (they otherwise corrupt the part).
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// Undo the common XML entities (inverse of `xml_escape`) — used when matching
/// anchors against a paragraph's visible text.
pub fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(semi) = s[i..].find(';') {
                let ent = &s[i + 1..i + semi];
                let rep = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => {
                        if let Some(num) = ent.strip_prefix('#').and_then(|n| {
                            let n = n.strip_prefix('x').map(|h| u32::from_str_radix(h, 16).ok());
                            n.unwrap_or_else(|| ent.trim_start_matches("#").parse::<u32>().ok())
                        }) {
                            char::from_u32(num)
                        } else {
                            None
                        }
                    }
                };
                if let Some(c) = rep {
                    out.push(c);
                    i += semi + 1;
                    continue;
                }
            }
        }
        out.push(s[i..].chars().next().unwrap());
        i += s[i..].chars().next().unwrap().len_utf8();
    }
    out
}

/// Write a ZIP archive of (name → bytes) with deflate — the container format
/// for docx/pptx.
pub fn write_zip(path: &Path, parts: &[(String, Vec<u8>)]) -> Result<()> {
    use zip::write::SimpleFileOptions;
    let file = std::fs::File::create(path)?;
    let mut w = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in parts {
        w.start_file(name.as_str(), opts)
            .map_err(|e| Error::Other(format!("zip part {name}: {e}")))?;
        w.write_all(bytes)?;
    }
    w.finish().map_err(|e| Error::Other(format!("zip finish: {e}")))?;
    Ok(())
}

/// Read every (name → bytes) entry of a zip archive, skipping directories.
pub fn read_zip_parts(path: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let f = std::fs::File::open(path)?;
    let mut ar = zip::ZipArchive::new(f)
        .map_err(|e| Error::Other(format!("not a valid Office file (zip): {e}")))?;
    let mut parts = Vec::with_capacity(ar.len());
    for i in 0..ar.len() {
        let mut e = ar.by_index(i)?;
        if e.is_dir() {
            continue;
        }
        let name = e.name().to_string();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf)?;
        parts.push((name, buf));
    }
    Ok(parts)
}

/// xsd:dateTime stamp for revision metadata (author/date on w:ins, comments).
pub fn now_xsd() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip_and_control_strip() {
        assert_eq!(xml_escape("a<b>&\"x\""), "a&lt;b&gt;&amp;&quot;x&quot;");
        assert_eq!(xml_unescape("a&lt;b&gt;&amp;&quot;x&quot;"), "a<b>&\"x\"");
        assert_eq!(xml_unescape("&#65;&#x42;"), "AB");
        assert_eq!(xml_escape("bad\u{0}ok"), "badok");
    }

    #[test]
    fn zip_roundtrip() {
        let dir = std::env::temp_dir().join(format!("cortex-office-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.zip");
        write_zip(&p, &[("a.xml".into(), b"<a/>".to_vec())]).unwrap();
        let parts = read_zip_parts(&p).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].0, "a.xml");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
