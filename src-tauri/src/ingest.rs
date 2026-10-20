//! Source ingestion: detect → parse → chunk → embed → store.
//! Parsers are real for txt/md/html (+ web fetch) and use the installed
//! LibreOffice headless converter for pdf/docx/pptx. audio/youtube return a
//! graceful placeholder (Whisper / yt-dlp wiring is a later slice).

use crate::embed::Embedder;
use crate::error::{Error, Result};
use crate::models::AddSourceInput;
use calamine::Reader;
use std::path::Path;

/// PDF 解析引擎配置（设置项 `pdf_engine` / `docling_bin`）。
///
/// - `auto`（默认）：装有 docling 就用 docling（IBM，版面感知，复杂双栏/表格/公式
///   远好于 pdftotext 的原始文本流），失败或空输出静默回退 poppler 旧链
/// - `docling`：强制 docling，失败回退 poppler 并附 warning
/// - `poppler`：跳过 docling，直接走 pdftotext → pdf-extract 旧链（最快）
#[derive(Debug, Clone, PartialEq)]
pub struct PdfEngineCfg {
    pub engine: String,
    /// docling 可执行文件路径覆盖（不在 PATH 时手动指定）
    pub docling_bin: Option<String>,
}
impl Default for PdfEngineCfg {
    fn default() -> Self {
        Self { engine: "auto".into(), docling_bin: None }
    }
}

/// Resolve the docling executable for this parse, honoring the engine switch:
/// `poppler` force-disables; an explicit `docling_bin` wins; otherwise probe
/// sidecar / data-dir bin / PATH (like every other ingest tool).
pub(crate) fn resolve_docling_bin(cfg: &PdfEngineCfg) -> Option<String> {
    if cfg.engine == "poppler" {
        return None;
    }
    if let Some(b) = cfg.docling_bin.as_deref() {
        let b = b.trim();
        if !b.is_empty() {
            return Some(b.to_string());
        }
    }
    if cfg!(windows) {
        tool("docling.exe").or_else(|| tool("docling"))
    } else {
        tool("docling")
    }
}

/// Detect the source kind from explicit input, then path/url extension.
pub fn detect_kind(input: &AddSourceInput) -> String {
    if let Some(k) = &input.kind {
        if !k.is_empty() {
            return k.clone();
        }
    }
    if input.url.is_some() {
        let u = input.url.as_deref().unwrap_or("");
        if u.contains("youtube.com") || u.contains("youtu.be") {
            return "yt".into();
        }
        return "web".into();
    }
    if let Some(p) = &input.path {
        let ext = Path::new(p)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        return match ext.as_str() {
            "pdf" => "pdf",
            "docx" | "doc" => "docx",
            "pptx" | "ppt" => "pptx",
            "epub" => "epub",
            "md" | "markdown" => "md",
            "txt" | "text" | "" => "txt",
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "tiff" | "tif" | "bmp" => "image",
            "xlsx" | "xlsm" | "xls" | "ods" => "xlsx",
            "csv" | "tsv" => "csv",
            "m4a" | "mp3" | "wav" | "ogg" | "opus" => "audio",
            other => other,
        }
        .to_string();
    }
    "txt".into()
}

/// Extract plaintext for a source. Returns `(text, optional_warning)`.
pub fn parse(kind: &str, input: &AddSourceInput, pdf: &PdfEngineCfg) -> Result<(String, Option<String>)> {
    match kind {
        "txt" | "md" => {
            if let Some(t) = &input.text {
                Ok((t.clone(), None))
            } else if let Some(p) = &input.path {
                Ok((std::fs::read_to_string(p)?, None))
            } else {
                Err(Error::Other("no text or path provided".into()))
            }
        }
        "web" => {
            let url = input
                .url
                .as_deref()
                .ok_or_else(|| Error::Other("web source needs a url".into()))?;
            let client = crate::commands::http_client(30);
            let html = client.get(url).send()?.text()?;
            Ok((html_to_text(&html), None))
        }
        "pdf" => {
            let p = input
                .path
                .as_deref()
                .ok_or_else(|| Error::Other("pdf source needs a file path".into()))?;
            pdf_to_text(p, pdf)
        }
        "docx" | "pptx" => {
            let p = input
                .path
                .as_deref()
                .ok_or_else(|| Error::Other(format!("{kind} source needs a file path")))?;
            // Native OOXML extraction first (no external tools). Fall back to
            // LibreOffice only for legacy binary .doc/.ppt (not zip-based) or if
            // the native pass yields nothing.
            match ooxml_to_text(p) {
                Ok(t) if !t.trim().is_empty() => Ok((t, None)),
                _ => libreoffice_to_text(p),
            }
        }
        "epub" => {
            let p = input
                .path
                .as_deref()
                .ok_or_else(|| Error::Other("epub source needs a file path".into()))?;
            epub_to_text(p)
        }
        "xlsx" => {
            let p = input
                .path
                .as_deref()
                .ok_or_else(|| Error::Other("xlsx source needs a file path".into()))?;
            spreadsheet_to_text(p)
        }
        "csv" => {
            let p = input
                .path
                .as_deref()
                .ok_or_else(|| Error::Other("csv source needs a file path".into()))?;
            Ok((csv_to_text(p)?, None))
        }
        "yt" => {
            let url = input
                .url
                .as_deref()
                .ok_or_else(|| Error::Other("youtube source needs a url".into()))?;
            youtube_to_text(url)
        }
        // Enriched at the command layer: audio → Whisper, image → vision OCR.
        "audio" | "image" => Ok((String::new(), None)),
        other => Err(Error::Unsupported(format!("unknown source kind: {other}"))),
    }
}

/// Strip HTML to readable text: drop script/style, remove tags, decode a few
/// common entities, collapse whitespace. Dependency-light on purpose.
pub fn html_to_text(html: &str) -> String {
    // The `regex` crate has no backreferences, so drop each non-content element
    // with its own pattern rather than a captured-group close tag.
    // Compile the strip patterns once (was recompiled on every call — html_to_text
    // runs per web page and per EPUB chapter).
    static BLOCK_RES: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    static TAG_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static WS_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // The `regex` crate has no backreferences, so drop each non-content element
    // with its own pattern rather than a captured-group close tag.
    let block_res = BLOCK_RES.get_or_init(|| {
        ["script", "style", "head", "nav", "footer"]
            .iter()
            .map(|tag| regex::Regex::new(&format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}>")).unwrap())
            .collect()
    });
    let mut stripped = html.to_string();
    for re in block_res {
        stripped = re.replace_all(&stripped, " ").into_owned();
    }
    let re_tag = TAG_RE.get_or_init(|| regex::Regex::new(r"(?s)<[^>]+>").unwrap());
    let text = re_tag.replace_all(&stripped, " ").into_owned();
    let text = decode_entities(&text);
    // Drop zero-width / BOM characters that make extracted web text look garbled.
    let text: String = text
        .chars()
        .filter(|&c| !matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' | '\u{ad}'))
        .collect();
    let re_ws = WS_RE.get_or_init(|| regex::Regex::new(r"\s+").unwrap());
    re_ws.replace_all(text.trim(), " ").to_string()
}

/// Reduce a fetched HTML page to readable content for the in-app reader view:
/// `(title, body_text, links)` where links are `(absolute_href, anchor_text)`.
/// No JS executes — safe to render. Dependency-light (regex only).
pub fn readable_page(html: &str, base_url: &str) -> (String, String, Vec<(String, String)>) {
    // Title: <title>…</title>, falling back to the page host.
    let title = regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>")
        .unwrap()
        .captures(html)
        .map(|c| decode_entities(c[1].trim()).trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| base_url.to_string());

    let text = html_to_text(html);

    // Outbound links: <a href="…">text</a>, resolved to absolute http(s) URLs.
    let re_a = regex::Regex::new(r#"(?is)<a\b[^>]*href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap();
    let mut links: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for caps in re_a.captures_iter(html) {
        let href_raw = caps[1].trim();
        if href_raw.is_empty()
            || href_raw.starts_with('#')
            || href_raw.starts_with("javascript:")
            || href_raw.starts_with("mailto:")
        {
            continue;
        }
        let abs = resolve_url(base_url, href_raw);
        if !abs.starts_with("http://") && !abs.starts_with("https://") {
            continue;
        }
        let label = html_to_text(&caps[2]);
        if label.is_empty() || !seen.insert(abs.clone()) {
            continue;
        }
        links.push((abs, label));
        if links.len() >= 120 {
            break;
        }
    }
    (title, text, links)
}

/// Resolve a possibly-relative href against a base URL (handles absolute,
/// scheme-relative `//`, root-relative `/path`, and simple relative paths).
fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    let after = &base[scheme_end..];
    let scheme = &base[..scheme_end]; // includes "://"
    let host = after.split(['/', '?', '#']).next().unwrap_or(after);
    if let Some(rest) = href.strip_prefix("//") {
        let s = if scheme.is_empty() { "https://" } else { scheme };
        return format!("{s}{rest}");
    }
    if let Some(path) = href.strip_prefix('/') {
        return format!("{scheme}{host}/{path}");
    }
    // relative to the current directory of the base path
    let base_no_query = base.split(['?', '#']).next().unwrap_or(base);
    let dir = base_no_query.rsplit_once('/').map(|(d, _)| d).unwrap_or(base_no_query);
    format!("{dir}/{href}")
}

/// Decode the common named entities plus numeric (`&#8203;`) and hex (`&#x27;`) refs.
fn decode_entities(s: &str) -> String {
    let named = s
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&rsquo;", "’")
        .replace("&lsquo;", "‘")
        .replace("&ldquo;", "“")
        .replace("&rdquo;", "”");
    static NUM_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re_num = NUM_RE.get_or_init(|| regex::Regex::new(r"&#(x?[0-9A-Fa-f]+);").unwrap());
    re_num
        .replace_all(&named, |caps: &regex::Captures| {
            let raw = &caps[1];
            let code = if let Some(hex) = raw.strip_prefix('x').or_else(|| raw.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                raw.parse::<u32>().ok()
            };
            code.and_then(char::from_u32).map(|c| c.to_string()).unwrap_or_default()
        })
        .into_owned()
}

/// Extract text from a PDF via `pdftotext -layout -enc UTF-8 <path> -` (poppler),
/// streaming output to stdout. PDFs are wrong for LibreOffice's txt converter
/// (it opens them in Draw and produces no .txt), so we use poppler instead.
///
/// This is forgiving on purpose: a scanned/image-only PDF has no extractable
/// text, but we still want it to ingest because the original bytes are copied
/// and remain previewable. So a missing `pdftotext` binary OR empty/whitespace
/// output returns `Ok((empty, Some(warning)))` rather than an error. Only a real
/// non-zero exit with stderr is a hard error.
fn pdf_to_text(path: &str, cfg: &PdfEngineCfg) -> Result<(String, Option<String>)> {
    let src = Path::new(path);
    if !src.exists() {
        return Err(Error::NotFound(format!("file not found: {path}")));
    }
    const NO_TEXT: &str = "warning: no extractable text (scanned PDF?); preview still available";

    // B1 (2.12): Docling head. IBM's layout-aware parser turns a PDF into
    // structured Markdown — headings, tables and math survive instead of
    // pdftotext's interleaved text stream. Any failure or empty output falls
    // through to the classic chain below (never blocks an ingest).
    let mut docling_failed = false;
    if let Some(bin) = resolve_docling_bin(cfg) {
        match docling_to_text(src, &bin) {
            Ok(t) if !t.trim().is_empty() => return Ok((t, None)),
            Ok(_) => docling_failed = true,
            Err(_) => docling_failed = true,
        }
    }

    // Desktop: poppler's `pdftotext -layout` is fastest + highest fidelity. On a hard
    // failure with a real message, surface it; an empty/soft result falls through to
    // the pure-Rust pass below (some PDFs pdftotext can't read but pdf-extract can).
    if which("pdftotext").is_some() {
        match new_cmd("pdftotext")
            .args(["-layout", "-enc", "UTF-8"])
            .arg(src)
            .arg("-")
            .output()
        {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout).into_owned();
                if !text.trim().is_empty() {
                    let warn = docling_failed
                        .then_some("docling returned no text; used pdftotext fallback".to_string());
                    return Ok((text, warn));
                }
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                if !stderr.is_empty() {
                    return Err(Error::Other(format!("pdftotext failed for {path}: {stderr}")));
                }
            }
            Err(_) => {} // binary vanished between which() and exec → try pure-Rust
        }
    }

    // No poppler (iOS/Android, or a desktop without it) — extract the text layer in
    // pure Rust, on-device. Text-layer PDFs (lecture slides, papers, exported notes)
    // come through fully; a scanned/image-only PDF yields nothing here and degrades to
    // the "scanned?" warning (OCR is the homelab ingest service's job).
    match pdf_extract_text_layer(src) {
        Some(t) if !t.trim().is_empty() => {
            let warn = docling_failed
                .then_some("docling returned no text; used pdftotext fallback".to_string());
            Ok((t, warn))
        }
        _ => Ok((String::new(), Some(NO_TEXT.into()))),
    }
}

/// Run `docling --to md --output <tmp> <pdf>` and return the produced Markdown.
///
/// Docling (IBM) is layout-aware: two-column papers, tables, section headers and
/// inline math come out as structured Markdown that the RichText renderer already
/// understands. First run downloads models and can take minutes — same blocking
/// model as the LibreOffice/Whisper paths. Errors are soft: callers fall back.
fn docling_to_text(path: &Path, bin: &str) -> Result<String> {
    let out = std::env::temp_dir().join(format!(
        "cortex-docling-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&out)?;
    let res = new_cmd(bin)
        .args(["--to", "md", "--output"])
        .arg(&out)
        .arg(path)
        .output();
    let text = match res {
        Ok(o) if o.status.success() => find_md_file(&out)
            .and_then(|md| std::fs::read_to_string(&md).ok())
            .unwrap_or_default(),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let _ = std::fs::remove_dir_all(&out);
            return Err(Error::Other(format!(
                "docling failed for {}: {}",
                path.display(),
                if stderr.is_empty() { "unknown error".to_string() } else { stderr }
            )));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&out);
            return Err(Error::Other(format!("docling exec failed ({bin}): {e}")));
        }
    };
    let _ = std::fs::remove_dir_all(&out);
    Ok(text)
}

/// Recursively find the first `.md` file docling wrote under `dir` (alphabetical
/// order for determinism). Docling writes `<stem>.md` at the top level, but the
/// output layout has shifted between versions — a walk is version-proof.
fn find_md_file(dir: &Path) -> Option<std::path::PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<std::fs::DirEntry> =
            std::fs::read_dir(&d).ok()?.collect::<std::io::Result<_>>().ok()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().map(|x| x == "md").unwrap_or(false) {
                return Some(p);
            }
        }
    }
    None
}

/// Extract a PDF's embedded text layer with the pure-Rust `pdf-extract` crate.
/// Returns None on failure. `pdf-extract` can panic on some malformed PDFs, so it
/// runs inside `catch_unwind` (the build keeps `panic = unwind`) — a bad PDF must
/// degrade to a warning, never crash the ingest worker.
fn pdf_extract_text_layer(path: &Path) -> Option<String> {
    let owned = path.to_path_buf();
    std::panic::catch_unwind(move || pdf_extract::extract_text(&owned).ok())
        .ok()
        .flatten()
}

/// Convert docx/pptx to text via `libreoffice --headless --convert-to txt`.
/// Unlike before, a missing binary or failed conversion is a hard `Err` (with
/// actionable detail) rather than an `Ok(empty)` that silently makes an empty draft.
fn libreoffice_to_text(path: &str) -> Result<(String, Option<String>)> {
    let src = Path::new(path);
    if !src.exists() {
        return Err(Error::NotFound(format!("file not found: {path}")));
    }

    // LibreOffice Impress (pptx/ppt) cannot export the Writer "Text" filter —
    // `--convert-to txt:Text` fails with an Io/Write error. Render the deck to a
    // temporary PDF (Impress CAN do that) and pull the text out with pdftotext.
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if matches!(ext.as_str(), "pptx" | "ppt") {
        let tmp = std::env::temp_dir().join(format!("cortex-ppt-{}", crate::db::new_id()));
        std::fs::create_dir_all(&tmp)?;
        let pdf = tmp.join("deck.pdf");
        let res = libreoffice_to_pdf(path, &pdf).and_then(|_| {
            // Temp render of a converted deck — skip the docling head (slow, and
            // the deck PDF is machine-generated text anyway); poppler chain only.
            let poppler = PdfEngineCfg { engine: "poppler".into(), docling_bin: None };
            pdf_to_text(pdf.to_str().ok_or_else(|| Error::Other("bad temp path".into()))?, &poppler)
        });
        let _ = std::fs::remove_dir_all(&tmp);
        return res;
    }

    let outdir = std::env::temp_dir().join(format!("cortex-ingest-{}", crate::db::new_id()));
    std::fs::create_dir_all(&outdir)?;

    let bin = libreoffice_bin();
    let output = new_cmd(&bin)
        .arg(lo_profile_arg(&outdir))
        .args(["--headless", "--convert-to", "txt:Text", "--outdir"])
        .arg(&outdir)
        .arg(src)
        .output();

    let result = match output {
        Ok(o) if o.status.success() => {
            let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
            let txt_path = outdir.join(format!("{stem}.txt"));
            match std::fs::read_to_string(&txt_path) {
                Ok(t) => Ok((t, None)),
                Err(e) => Err(Error::Other(format!(
                    "libreoffice produced no text output for {path}: {e}"
                ))),
            }
        }
        Ok(o) => Err(Error::Other(format!(
            "libreoffice conversion of {path} failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ))),
        Err(e) => Err(Error::Other(format!(
            "libreoffice is not runnable ({e}); install LibreOffice to ingest {path}"
        ))),
    };
    let _ = std::fs::remove_dir_all(&outdir);
    result
}

/// Render a docx/pptx (or any LibreOffice-openable doc) to PDF and copy the
/// result to `dest`. Returns a hard `Err` (with stderr) if conversion fails so
/// callers never persist a half-ingested source silently.
pub fn libreoffice_to_pdf(path: &str, dest: &Path) -> Result<()> {
    let src = Path::new(path);
    if !src.exists() {
        return Err(Error::NotFound(format!("file not found: {path}")));
    }
    let outdir = std::env::temp_dir().join(format!("cortex-pdf-{}", crate::db::new_id()));
    std::fs::create_dir_all(&outdir)?;

    let bin = libreoffice_bin();
    let output = new_cmd(&bin)
        .arg(lo_profile_arg(&outdir))
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
        .arg(&outdir)
        .arg(src)
        .output();

    let result: Result<()> = match output {
        Ok(o) if o.status.success() => {
            let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
            let pdf_path = outdir.join(format!("{stem}.pdf"));
            if pdf_path.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&pdf_path, dest)?;
                Ok(())
            } else {
                Err(Error::Other(format!(
                    "libreoffice did not produce a PDF for {path}"
                )))
            }
        }
        Ok(o) => Err(Error::Other(format!(
            "libreoffice PDF conversion of {path} failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ))),
        Err(e) => Err(Error::Other(format!(
            "libreoffice is not runnable ({e}); install LibreOffice to render {path}"
        ))),
    };
    let _ = std::fs::remove_dir_all(&outdir);
    result
}

fn libreoffice_bin() -> String {
    for c in ["libreoffice", "soffice"] {
        if which(c).is_some() {
            return c.to_string();
        }
    }
    "libreoffice".to_string()
}

/// Is an office→PDF converter (LibreOffice) available? Used to decide whether to
/// attempt a faithful slide-preview render — when it isn't, ingestion still
/// succeeds via native text extraction, just without the rendered PDF preview.
pub fn office_converter_available() -> bool {
    which("libreoffice").is_some() || which("soffice").is_some()
}

/// Extract text from a `.docx`/`.pptx` natively (they're just zip + XML) so
/// office documents ingest on every OS with **no external tool** — Windows and
/// macOS users don't need LibreOffice. Legacy binary `.doc`/`.ppt` aren't
/// zip-based, so this returns `Err` for them and the caller falls back to
/// LibreOffice. Paragraph (`</w:p>`, `</a:p>`) and slide boundaries become
/// newlines so the extracted text stays readable and chunkable.
pub fn ooxml_to_text(path: &str) -> Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| Error::Other(format!("{path} is not a valid OOXML (zip) file: {e}")))?;

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Which XML parts hold the body text, in reading order.
    let parts: Vec<String> = if ext == "docx" {
        vec!["word/document.xml".to_string()]
    } else {
        let mut slides: Vec<String> = (0..zip.len())
            .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .collect();
        // slide2.xml before slide10.xml — sort by the embedded number, not lexically.
        slides.sort_by_key(|n| {
            n.trim_start_matches("ppt/slides/slide")
                .trim_end_matches(".xml")
                .parse::<u32>()
                .unwrap_or(u32::MAX)
        });
        slides
    };

    // A run of text is <w:t>…</w:t> (Word) or <a:t>…</a:t> (PowerPoint); a
    // paragraph end (</w:p> / </a:p>) becomes a newline.
    let re = regex::Regex::new(r"(?s)<(?:w|a):t[^>]*>(.*?)</(?:w|a):t>|</(?:w|a):p>").unwrap();
    let mut out = String::new();
    for part in &parts {
        let mut xml = String::new();
        if let Ok(mut f) = zip.by_name(part) {
            if f.read_to_string(&mut xml).is_err() {
                continue;
            }
        } else {
            continue;
        }
        for cap in re.captures_iter(&xml) {
            match cap.get(1) {
                Some(m) => out.push_str(&decode_entities(m.as_str())),
                None => out.push('\n'),
            }
        }
        out.push_str("\n\n"); // blank line between slides / the document body
    }
    Ok(out.trim().to_string())
}

/// Extract reading-order text from an EPUB natively — it's a zip of XHTML
/// documents, so the same dependency-light approach as `ooxml_to_text` applies
/// (no epub crate, no external tool; works keyless on every OS incl. mobile).
/// We read `META-INF/container.xml` to locate the OPF package, follow its
/// `<spine>` for chapter order, resolve each chapter's href against the package
/// directory, and strip the XHTML to text with `html_to_text`. If the package
/// or spine can't be parsed we fall back to every `.xhtml`/`.html` entry in name
/// order, so even a malformed e-book yields its prose rather than nothing.
pub fn epub_to_text(path: &str) -> Result<(String, Option<String>)> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| Error::Other(format!("{path} is not a valid EPUB (zip) file: {e}")))?;

    // Read a zip entry to a String by name (None if absent/unreadable).
    fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
        let mut s = String::new();
        zip.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    }

    // Resolve the spine to an ordered list of XHTML entry paths via the manifest.
    let ordered: Vec<String> = (|| {
        let container = read_entry(&mut zip, "META-INF/container.xml")?;
        let opf_path = regex::Regex::new(r#"(?is)full-path\s*=\s*["']([^"']+)["']"#)
            .unwrap()
            .captures(&container)
            .map(|c| percent_decode(&c[1]))?;
        let opf = read_entry(&mut zip, &opf_path)?;
        let opf_dir = opf_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");

        // Pull a quoted attribute value out of a single start-tag. One shared regex,
        // compiled once — was recompiled per attribute per <item> (per-book hot loop).
        let attr = |tag: &str, want: &str| {
            static ATTR_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            ATTR_RE
                .get_or_init(|| regex::Regex::new(r#"(?is)([\w:-]+)\s*=\s*["']([^"']*)["']"#).unwrap())
                .captures_iter(tag)
                .find(|c| c[1].eq_ignore_ascii_case(want))
                .map(|c| c[2].to_string())
        };

        // manifest: id -> (href, media-type)
        let mut manifest: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        for m in regex::Regex::new(r"(?is)<item\b[^>]*>").unwrap().find_iter(&opf) {
            let tag = m.as_str();
            if let (Some(id), Some(href)) = (attr(tag, "id"), attr(tag, "href")) {
                manifest.insert(id, (href, attr(tag, "media-type").unwrap_or_default()));
            }
        }
        // spine: ordered idrefs → manifest hrefs (XHTML content documents only)
        let mut order = Vec::new();
        for m in regex::Regex::new(r"(?is)<itemref\b[^>]*>").unwrap().find_iter(&opf) {
            if let Some(idref) = attr(m.as_str(), "idref") {
                if let Some((href, mtype)) = manifest.get(&idref) {
                    if is_xhtml(href, mtype) {
                        order.push(zip_join(opf_dir, &percent_decode(href)));
                    }
                }
            }
        }
        if order.is_empty() {
            None
        } else {
            Some(order)
        }
    })()
    .unwrap_or_else(|| {
        // Fallback: every (x)html document in the archive, in name order.
        let mut names: Vec<String> = (0..zip.len())
            .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
            .filter(|n| is_xhtml(n, ""))
            .collect();
        names.sort();
        names
    });

    let mut out = String::new();
    for name in &ordered {
        if let Some(html) = read_entry(&mut zip, name) {
            let text = html_to_text(&html);
            if !text.trim().is_empty() {
                out.push_str(text.trim());
                out.push_str("\n\n");
            }
        }
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        Ok((String::new(), Some("warning: no readable text found in EPUB".into())))
    } else {
        Ok((out, None))
    }
}

/// Is this href / media-type an (X)HTML content document worth extracting?
fn is_xhtml(href: &str, media_type: &str) -> bool {
    let h = href.to_lowercase();
    media_type.contains("xhtml")
        || media_type == "text/html"
        || h.ends_with(".xhtml")
        || h.ends_with(".html")
        || h.ends_with(".htm")
}

/// Join an EPUB-internal href onto its package directory, resolving `.`/`..`
/// segments and dropping any `#fragment`/`?query`. Zip entry names always use
/// forward slashes, so this stays slash-based on every OS.
fn zip_join(base_dir: &str, href: &str) -> String {
    let href = href.split(['#', '?']).next().unwrap_or(href);
    let mut segs: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for part in href.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            p => segs.push(p),
        }
    }
    segs.join("/")
}

/// Decode `%XX` escapes in an OPF href so it matches the raw zip entry name
/// (a space is `%20` in the href but literal in the archive). Leaves a trailing
/// or malformed `%` untouched.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `-env:UserInstallation` arg pointing at a throwaway profile inside `outdir`.
/// Every headless conversion MUST use its own profile: otherwise a second
/// concurrent soffice — another source ingesting, a pptx/docx preview rendering,
/// or the user's own open LibreOffice — collides on the shared default profile
/// lock and silently fails (exit 1, empty stderr, no output file). That lock
/// collision is the real cause of "pptx/docx upload is broken / produced no
/// output". An isolated profile per call removes the contention entirely.
/// Fetch a YouTube transcript via yt-dlp (subtitles → text). Uses yt-dlp if it's
/// on PATH; otherwise returns an actionable error. Prefers manual subs, falls
/// back to auto-generated captions.
fn youtube_to_text(url: &str) -> Result<(String, Option<String>)> {
    // No yt-dlp (e.g. on mobile)? Fall back to a pure-HTTP caption fetch — no binary.
    let Some(bin) = tool("yt-dlp") else {
        return youtube_via_http(url);
    };
    let dir = std::env::temp_dir().join(format!("cortex-yt-{}", crate::db::new_id()));
    std::fs::create_dir_all(&dir)?;
    let out_tmpl = dir.join("%(id)s.%(ext)s");
    let out = new_cmd(&bin)
        .args([
            "--skip-download",
            "--write-subs",
            "--write-auto-subs",
            "--sub-langs",
            "en.*,en",
            "--sub-format",
            "vtt",
            "--no-playlist",
            "-o",
        ])
        .arg(&out_tmpl)
        .arg(url)
        .output();
    let res: Result<(String, Option<String>)> = match out {
        Ok(o) if o.status.success() => {
            let vtt = std::fs::read_dir(&dir)?
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().and_then(|x| x.to_str()) == Some("vtt"));
            match vtt {
                Some(p) => {
                    let text = vtt_to_text(&std::fs::read_to_string(&p)?);
                    if text.trim().is_empty() {
                        Err(Error::Other("the video's transcript was empty".into()))
                    } else {
                        Ok((text, None))
                    }
                }
                None => Err(Error::Other(
                    "no English captions are available for this video".into(),
                )),
            }
        }
        Ok(o) => Err(Error::Other(format!(
            "yt-dlp failed: {}",
            String::from_utf8_lossy(&o.stderr).trim().chars().take(200).collect::<String>()
        ))),
        Err(e) => Err(Error::Other(format!("yt-dlp not runnable: {e}"))),
    };
    let _ = std::fs::remove_dir_all(&dir);
    res
}

/// Pure-HTTP YouTube transcript fetch (no yt-dlp): pull the watch page, find a
/// caption track URL in the player response, fetch the timedtext, strip to prose.
/// Best-effort (YouTube can change this) — used as the mobile/binary-less fallback.
fn youtube_via_http(url: &str) -> Result<(String, Option<String>)> {
    let client = crate::commands::http_client(30);
    let html = client
        .get(url)
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()?
        .text()?;
    let tracks = regex::Regex::new(r#""captionTracks":(\[.*?\])"#)
        .unwrap()
        .captures(&html)
        .map(|c| c[1].to_string())
        .ok_or_else(|| Error::Other("no captions are available for this video".into()))?;
    let burl_re = regex::Regex::new(r#""baseUrl":"(.*?)""#).unwrap();
    let lang_re = regex::Regex::new(r#""languageCode":"(.*?)""#).unwrap();
    // Prefer an English track; otherwise take the first.
    let mut chosen: Option<String> = None;
    for chunk in tracks.split("},{") {
        let Some(burl) = burl_re.captures(chunk).map(|c| c[1].to_string()) else { continue };
        let lang = lang_re.captures(chunk).map(|c| c[1].to_string()).unwrap_or_default();
        if lang.starts_with("en") {
            chosen = Some(burl);
            break;
        }
        chosen.get_or_insert(burl);
    }
    let base = chosen
        .ok_or_else(|| Error::Other("no caption track URL found".into()))?
        .replace("\\u0026", "&")
        .replace("\\/", "/");
    let xml = client.get(&base).send()?.text()?;
    let text = timedtext_to_text(&xml);
    if text.trim().is_empty() {
        Err(Error::Other("the video's transcript was empty".into()))
    } else {
        Ok((text, None))
    }
}

/// Strip YouTube timedtext XML (`<text start=… dur=…>…</text>`) to plain prose.
fn timedtext_to_text(xml: &str) -> String {
    let text_re = regex::Regex::new(r"(?s)<text[^>]*>(.*?)</text>").unwrap();
    let tag_re = regex::Regex::new(r"<[^>]+>").unwrap();
    let mut parts: Vec<String> = Vec::new();
    for c in text_re.captures_iter(xml) {
        let inner = tag_re.replace_all(&c[1], " ").into_owned();
        let t = decode_entities(&inner)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !t.is_empty() {
            parts.push(t);
        }
    }
    parts.join(" ")
}

/// Strip a WebVTT subtitle file to plain deduped prose.
fn vtt_to_text(vtt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in vtt.lines() {
        let t = line.trim();
        if t.is_empty()
            || t == "WEBVTT"
            || t.starts_with("Kind:")
            || t.starts_with("Language:")
            || t.starts_with("NOTE")
            || t.contains("-->")
            || t.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        // Drop inline timing/style tags like <00:00:01.000> and <c>…</c>.
        let mut cleaned = String::with_capacity(t.len());
        let mut in_tag = false;
        for c in t.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => cleaned.push(c),
                _ => {}
            }
        }
        let cleaned = cleaned.trim().to_string();
        // Auto-captions roll the same line repeatedly — drop consecutive repeats.
        if cleaned.is_empty() || out.last() == Some(&cleaned) {
            continue;
        }
        out.push(cleaned);
    }
    out.join(" ")
}

/// MIME type for an image source path (for OCR data URLs).
pub fn image_mime(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/png",
    }
}

/// (mime, bytes) for the vision-OCR path. TIFF/BMP scans (typical scanner and
/// 文献 exports) are rejected by most vision APIs — decode and re-encode as
/// PNG first; everything else passes through unchanged.
pub fn image_for_ocr(path: &str) -> Result<(&'static str, Vec<u8>)> {
    let bytes = std::fs::read(path)?;
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if matches!(ext.as_str(), "tiff" | "tif" | "bmp") {
        Ok(("image/png", image_to_png_bytes(&bytes)?))
    } else {
        Ok((image_mime(path), bytes))
    }
}

/// Decode any image (TIFF/BMP/WebP/GIF…) and re-encode as PNG bytes, so the
/// stored original can render in a browser `<img>` and re-OCR cleanly.
pub fn image_to_png(path: &str) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    image_to_png_bytes(&bytes)
}

fn image_to_png_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| Error::Other(format!("unreadable image: {e}")))?;
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| Error::Other(format!("png encode failed: {e}")))?;
    Ok(png.into_inner())
}

/// Render one spreadsheet cell to display text: numbers lose their trailing
/// `.0`, Excel date serials decode to ISO dates (the 1900 date system with the
/// fake 1900-02-29 folded into 1899-12-30 as epoch), errors render as #REF!.
fn cell_str(d: &calamine::Data) -> String {
    match d {
        calamine::Data::Empty => String::new(),
        calamine::Data::Int(i) => i.to_string(),
        calamine::Data::Float(f) => fmt_num(*f),
        calamine::Data::String(s) => s.clone(),
        calamine::Data::Bool(b) => b.to_string(),
        calamine::Data::DateTime(dt) => {
            if dt.is_datetime() {
                date_from_serial(dt.as_f64())
            } else {
                fmt_num(dt.as_f64())
            }
        }
        calamine::Data::DateTimeIso(s) => s.clone(),
        calamine::Data::DurationIso(s) => s.clone(),
        calamine::Data::Error(e) => format!("#{e}"),
    }
}

fn fmt_num(f: f64) -> String {
    if f.is_finite() && (f - f.trunc()).abs() < 1e-9 && f.abs() < 1e15 {
        format!("{}", f.trunc() as i64)
    } else {
        format!("{f}")
    }
}

fn date_from_serial(serial: f64) -> String {
    let days = serial.trunc() as i64;
    let secs = ((serial - serial.trunc()) * 86_400.0).round() as i64;
    if let (Ok(base), false) = (
        time::Date::from_calendar_date(1899, time::Month::December, 30),
        !(0..=100_000).contains(&days),
    ) {
        if let Some(d) = base.checked_add(time::Duration::days(days)) {
            let mut out = d.to_string();
            if secs > 0 {
                out.push_str(&format!(" {:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60));
            }
            return out;
        }
    }
    fmt_num(serial)
}

/// Markdown-table cell: pipes and newlines would break the table shape.
fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").replace('\r', "")
}

fn md_table(headers: &[String], rows: &[Vec<String>], width: usize) -> String {
    let pad = |mut r: Vec<String>| {
        r.resize(width, String::new());
        r
    };
    let mut out = String::new();
    out.push_str("| ");
    out.push_str(&pad(headers.to_vec()).iter().take(width).map(|h| md_cell(h)).collect::<Vec<_>>().join(" | "));
    out.push_str(" |\n| ");
    out.push_str(&vec!["---"; width].join(" | "));
    out.push_str(" |\n");
    for r in rows {
        out.push_str("| ");
        out.push_str(&pad(r.clone()).iter().take(width).map(|c| md_cell(c)).collect::<Vec<_>>().join(" | "));
        out.push_str(" |\n");
    }
    out
}

/// Parse a spreadsheet (.xlsx/.xlsm/.xls/.ods via calamine) into Markdown
/// tables, one per sheet — chunkable, model-readable, and faithful to dates
/// and numbers. Rows are capped per sheet so a 100k-row grade book can't
/// monopolize the source (the cap is noted in the output, never silent).
pub fn spreadsheet_to_text(path: &str) -> Result<(String, Option<String>)> {
    const MAX_ROWS: usize = 5000;
    const MAX_COLS: usize = 64;
    let mut wb = calamine::open_workbook_auto(path)
        .map_err(|e| Error::Other(format!("cannot open spreadsheet: {e}")))?;
    let mut out = String::new();
    let mut warnings: Vec<String> = Vec::new();
    for name in wb.sheet_names() {
        let range = match wb.worksheet_range(&name) {
            Ok(r) => r,
            Err(e) => {
                warnings.push(format!("工作表 {name} 无法读取：{e}"));
                continue;
            }
        };
        let all: Vec<Vec<String>> = range
            .rows()
            .map(|r| r.iter().map(cell_str).collect())
            .collect();
        if all.iter().all(|r| r.iter().all(|c| c.trim().is_empty())) {
            continue;
        }
        let width = all.iter().map(|r| r.len()).max().unwrap_or(0).min(MAX_COLS);
        if width == 0 {
            continue;
        }
        out.push_str(&format!("## 表格：{name}\n\n"));
        let shown = all.len().min(MAX_ROWS);
        let headers = &all[0];
        let rows: Vec<Vec<String>> = all[1..shown].to_vec();
        out.push_str(&md_table(headers, &rows, width));
        if all.len() > MAX_ROWS {
            out.push_str(&format!(
                "\n（仅显示前 {MAX_ROWS} 行，共 {} 行）\n\n",
                all.len()
            ));
        } else {
            out.push('\n');
        }
    }
    let text = out.trim().to_string();
    if text.is_empty() {
        return Err(Error::Other("spreadsheet has no readable sheets".into()));
    }
    let warning = if warnings.is_empty() {
        None
    } else {
        Some(warnings.join("；"))
    };
    Ok((text, warning))
}

/// Parse a CSV/TSV into a Markdown table. Delimiter is sniffed from the first
/// line (tab → TSV, then comma, then semicolon for Excel-localized exports).
/// Non-UTF-8 bytes (GBK exports) decode lossily rather than failing ingest.
pub fn csv_to_text(path: &str) -> Result<String> {
    const MAX_ROWS: usize = 5000;
    const MAX_COLS: usize = 64;
    let raw = std::fs::read(path)?;
    let raw = String::from_utf8_lossy(&raw);
    let raw = raw.trim_start_matches('\u{feff}');
    let first_line = raw.lines().next().unwrap_or("");
    let delim = if first_line.contains('\t') {
        b'\t'
    } else if first_line.contains(',') {
        b','
    } else {
        b';'
    };
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(false)
        .flexible(true)
        .from_reader(raw.as_bytes());
    let mut all: Vec<Vec<String>> = Vec::new();
    for rec in rdr.records() {
        match rec {
            Ok(r) => all.push(r.iter().map(|c| c.to_string()).collect()),
            Err(_) => break, // malformed tail — keep what parsed
        }
    }
    if all.iter().all(|r| r.iter().all(|c| c.trim().is_empty())) {
        return Err(Error::Other("CSV has no data rows".into()));
    }
    let width = all.iter().map(|r| r.len()).max().unwrap_or(0).min(MAX_COLS);
    if width == 0 {
        return Err(Error::Other("CSV has no columns".into()));
    }
    let shown = all.len().min(MAX_ROWS);
    let text = format!(
        "{}{}",
        md_table(&all[0], &all[1..shown], width),
        if all.len() > MAX_ROWS {
            format!("\n（仅显示前 {MAX_ROWS} 行，共 {} 行）\n", all.len())
        } else {
            String::new()
        }
    );
    Ok(text)
}

/// Render up to `max_pages` of a PDF to PNG page images (bytes) via poppler's
/// `pdftoppm`. Used to OCR scanned/image-only PDFs through a vision model.
pub fn pdf_page_images(path: &str, max_pages: usize) -> Result<Vec<Vec<u8>>> {
    rasterize(path, None, max_pages)
}

/// Rasterize ONE page (1-based) — the figure-explain path (2.6) sends a single
/// page to the vision model instead of the first-N-pages OCR batch.
pub fn pdf_page_image(path: &str, page: usize) -> Result<Vec<u8>> {
    let mut imgs = rasterize(path, Some(page), page)?;
    match imgs.len() {
        1 => Ok(imgs.remove(0)),
        0 => Err(Error::Other(format!("无法渲染第 {page} 页（超出页数范围？）"))),
        _ => Err(Error::Other("渲染返回了多页".into())),
    }
}

/// Shared pdftoppm runner: pages `from..=to` (None from = 1). Returns PNG bytes.
fn rasterize(path: &str, from: Option<usize>, to: usize) -> Result<Vec<Vec<u8>>> {
    let Some(pdftoppm) = which("pdftoppm") else {
        return Err(Error::Other(
            "pdftoppm (poppler) not found — needed to OCR scanned PDFs".into(),
        ));
    };
    let dir = std::env::temp_dir().join(format!("cortex-ocr-{}", crate::db::new_id()));
    std::fs::create_dir_all(&dir)?;
    let prefix = dir.join("page");
    let mut args = vec!["-png".to_string(), "-r".to_string(), "150".to_string()];
    if let Some(f) = from {
        args.push("-f".into());
        args.push(f.to_string());
    }
    args.push("-l".into());
    args.push(to.to_string());
    let out = new_cmd(&pdftoppm)
        .args(&args)
        .arg(path)
        .arg(&prefix)
        .output();
    let result: Result<Vec<Vec<u8>>> = match out {
        Ok(o) if o.status.success() => {
            let mut paths: Vec<_> = std::fs::read_dir(&dir)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("png"))
                .collect();
            paths.sort();
            Ok(paths.iter().filter_map(|p| std::fs::read(p).ok()).collect())
        }
        Ok(o) => Err(Error::Other(format!(
            "pdftoppm failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ))),
        Err(e) => Err(Error::Other(format!("pdftoppm not runnable: {e}"))),
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn lo_profile_arg(outdir: &Path) -> String {
    let profile = outdir.join("lo-profile");
    format!("-env:UserInstallation=file://{}", profile.to_string_lossy())
}

fn chromium_bin() -> Option<String> {
    for c in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "brave",
    ] {
        if let Some(p) = which(c) {
            return Some(p);
        }
    }
    None
}

/// Render a self-contained HTML string to a PDF at `dest`. Prefers headless
/// Chromium (full CSS/markdown-table/callout fidelity) and falls back to
/// LibreOffice's HTML import (lower fidelity) when no Chromium is present.
/// The frontend builds the styled HTML and chooses `dest` via the save dialog,
/// so this is a pure render-to-file step. Errors are hard (never a silent
/// empty/zero-byte PDF) so the UI can surface a real failure.
pub fn html_to_pdf(html: &str, dest: &Path) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("cortex-export-{}", crate::db::new_id()));
    std::fs::create_dir_all(&tmp)?;
    let html_path = tmp.join("doc.html");
    std::fs::write(&html_path, html)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let result: Result<()> = if let Some(bin) = chromium_bin() {
        let profile = tmp.join("profile");
        let mut pdf_arg = std::ffi::OsString::from("--print-to-pdf=");
        pdf_arg.push(dest);
        let mut user_data = std::ffi::OsString::from("--user-data-dir=");
        user_data.push(&profile);
        let out = new_cmd(&bin)
            .arg("--headless")
            .arg("--no-sandbox")
            .arg("--disable-gpu")
            .arg("--no-pdf-header-footer")
            .arg("--virtual-time-budget=5000")
            .arg(user_data)
            .arg(pdf_arg)
            .arg(&html_path)
            .output();
        match out {
            Ok(o) if o.status.success() && dest.exists() => Ok(()),
            Ok(o) => Err(Error::Other(format!(
                "chromium PDF export failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))),
            Err(e) => Err(Error::Other(format!("chromium is not runnable: {e}"))),
        }
    } else {
        // Fallback: LibreOffice opens the .html and exports a PDF.
        libreoffice_to_pdf(
            html_path
                .to_str()
                .ok_or_else(|| Error::Other("bad temp path".into()))?,
            dest,
        )
    };
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

pub fn which(cmd: &str) -> Option<String> {
    // Windows PATHEXT: a bare `cmd` never matches `cmd.exe` via is_file, so
    // probe the .exe spelling explicitly (PATH + fallback dirs alike).
    #[cfg(windows)]
    let exe_name = format!("{cmd}.exe");
    let probe = |dir: &std::path::Path| -> Option<String> {
        let bare = dir.join(cmd);
        if bare.is_file() {
            return Some(bare.to_string_lossy().into_owned());
        }
        #[cfg(windows)]
        {
            let exe = dir.join(&exe_name);
            if exe.is_file() {
                return Some(exe.to_string_lossy().into_owned());
            }
        }
        None
    };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(full) = probe(&dir) {
                return Some(full);
            }
        }
    }
    // Desktop launchers often start the app with a minimal PATH that misses
    // the user's shell additions — probe the standard install dirs too.
    let mut cands: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(h) = std::env::var("HOME") {
        cands.push(std::path::Path::new(&h).join(".local/bin").join(cmd));
    }
    // Homebrew: Apple Silicon (/opt/homebrew) first, then Intel (/usr/local). macOS GUI
    // apps launched from Finder get a minimal PATH that omits both, so probe directly.
    cands.push(std::path::PathBuf::from("/opt/homebrew/bin"));
    cands.push(std::path::PathBuf::from("/opt/homebrew/sbin"));
    cands.push(std::path::PathBuf::from("/usr/local/bin"));
    cands.push(std::path::PathBuf::from("/usr/local/sbin"));
    cands.push(std::path::PathBuf::from("/usr/bin"));
    cands.iter().find_map(|p| probe(p.as_path()))
}

/// Resolve a binary bundled as a Tauri sidecar — shipped next to the app
/// executable (`bundle.externalBin`). Returns `None` in `tauri dev` or if the
/// sidecar isn't present, so callers fall back to PATH / runtime download.
pub fn bundled(name: &str) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let fname = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let p = dir.join(fname);
    if p.is_file() {
        Some(p.to_string_lossy().into_owned())
    } else {
        None
    }
}

/// A tool's path lookup, in priority order:
///   1. Bundled Tauri sidecar (bundle.externalBin legacy path)
///   2. Runtime-fetched copy in <app_data_dir>/bin/<name> (sidecars::ensure)
///   3. The user's $PATH and standard install dirs
///
/// `data_dir` is Some(<app_data_dir>) when the caller has it (mpv, backup);
/// passing None skips the runtime-fetched bin dir - e.g. for ingest code
/// paths that don't know the data dir, or on mobile where no sidecar is
/// possible.
pub fn tool_in(name: &str, data_dir: Option<&std::path::Path>) -> Option<String> {
    bundled(name)
        .or_else(|| runtime_fetched(name, data_dir))
        .or_else(|| which(name))
}

/// Spawn a child process WITHOUT the black console window Windows otherwise
/// pops for console-subsystem children (mpv, yt-dlp, pdftotext, docling, ...)
/// when the parent is a GUI app. Drop-in replacement for `Command::new` —
/// the returned Command chains exactly the same (arg/env/output/...).
pub(crate) fn new_cmd(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: GUI parent + console child = visible console
        // window unless suppressed. mpv even stays alive for the whole
        // playback session, so the window would linger forever.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Legacy single-arg lookup; equivalent to tool_in(name, None). Kept for
/// the many existing callers that don't have the data dir in scope. New
/// call sites that do should prefer tool_in.
pub fn tool(name: &str) -> Option<String> {
    tool_in(name, None)
}

fn runtime_fetched(name: &str, data_dir: Option<&std::path::Path>) -> Option<String> {
    let dir = data_dir?;
    let fname = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let p = dir.join("bin").join(fname);
    if p.is_file() {
        Some(p.to_string_lossy().into_owned())
    } else {
        None
    }
}


/// Split text into bounded, overlapping chunks on word boundaries.
pub fn chunk_text(text: &str, target: usize, overlap: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut start_idx = 0usize;
    let mut i = 0usize;
    while i < words.len() {
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(words[i]);
        i += 1;
        if cur.len() >= target {
            chunks.push(cur.clone());
            // step back `overlap` chars worth of words for context continuity
            let mut back = 0usize;
            let mut j = i;
            while j > start_idx && back < overlap {
                j -= 1;
                back += words[j].len() + 1;
            }
            start_idx = j;
            i = j;
            cur.clear();
            // rebuild cur from start_idx is unnecessary; loop will append from i
        }
    }
    if !cur.trim().is_empty() {
        chunks.push(cur.trim().to_string());
    }
    chunks
}

/// Embed a batch of chunk texts with the configured embedder.
pub fn embed_chunks(embedder: &dyn Embedder, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
    if chunks.is_empty() {
        return Ok(Vec::new());
    }
    embedder.embed(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunker_respects_bounds_and_covers_text() {
        let text = "word ".repeat(500);
        let chunks = chunk_text(&text, 200, 40);
        assert!(chunks.len() > 1);
        for c in &chunks {
            // allow one word of slop past target
            assert!(c.len() <= 200 + 12, "chunk too big: {}", c.len());
        }
    }

    #[test]
    fn chunker_empty_input() {
        assert!(chunk_text("   ", 100, 10).is_empty());
    }

    #[test]
    fn docling_engine_resolution_respects_switch_and_override() {
        // poppler switch hard-disables docling, even with an explicit path
        let cfg = PdfEngineCfg {
            engine: "poppler".into(),
            docling_bin: Some("C:/tools/docling.exe".into()),
        };
        assert_eq!(resolve_docling_bin(&cfg), None);
        // an explicit non-empty docling_bin wins over PATH probing (trimmed)
        let cfg = PdfEngineCfg {
            engine: "auto".into(),
            docling_bin: Some(" /custom/docling ".into()),
        };
        assert_eq!(resolve_docling_bin(&cfg), Some("/custom/docling".into()));
        // empty override falls back to probing — on a machine without docling
        // in PATH/sidecar this is None (parse then silently uses poppler)
        let cfg = PdfEngineCfg { engine: "auto".into(), docling_bin: Some("  ".into()) };
        let found = resolve_docling_bin(&cfg);
        assert!(found.is_none() || found.unwrap().contains("docling"));
    }

    #[test]
    fn docling_md_finder_walks_nested_dirs() {
        let base = std::env::temp_dir().join(format!("cortex-md-find-{}", crate::db::new_id()));
        let nested = base.join("out").join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("out").join("a.txt"), "not md").unwrap();
        std::fs::write(nested.join("paper.md"), "# heading").unwrap();
        assert_eq!(find_md_file(&base), Some(nested.join("paper.md")));
        assert_eq!(find_md_file(&base.join("out").join("a.txt")), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn spreadsheet_and_csv_ingest() {
        let dir = std::env::temp_dir().join(format!("cortex-xlsx-test-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Build a real grade-book xlsx with rust_xlsxwriter and parse it back.
        let xlsx = dir.join("grades.xlsx");
        {
            let mut wb = rust_xlsxwriter::Workbook::new();
            let ws = wb.add_worksheet().set_name("成绩").unwrap();
            ws.write_string(0, 0, "姓名").unwrap();
            ws.write_number(0, 1, 89.0).unwrap();
            ws.write_string(1, 0, "小明").unwrap();
            ws.write_number(1, 1, 92.5).unwrap();
            wb.save(&xlsx).unwrap();
        }
        let (text, warning) = spreadsheet_to_text(xlsx.to_str().unwrap()).unwrap();
        assert!(text.contains("## 表格：成绩"), "xlsx text: {text}");
        assert!(text.contains("小明"));
        assert!(text.contains("| 89 |"), "89.0 should render as 89: {text}");
        assert!(text.contains("92.5"));
        assert!(warning.is_none());

        // CSV: comma dialect round-trips through the markdown table.
        let csv = dir.join("t.csv");
        std::fs::write(&csv, "a,b\n1,x\n2,y\n").unwrap();
        let ctext = csv_to_text(csv.to_str().unwrap()).unwrap();
        assert!(ctext.starts_with("| a | b |"), "csv: {ctext}");
        assert!(ctext.contains("| 2 | y |"));

        // TSV: tab-delimited is sniffed from the first line.
        std::fs::write(&csv, "a\tb\n1\tx\n").unwrap();
        let ttext = csv_to_text(csv.to_str().unwrap()).unwrap();
        assert!(ttext.contains("| 1 | x |"), "tsv: {ttext}");

        // Pipes in cells must not break the table shape.
        std::fs::write(&csv, "a,b\n\"x|y\",1\n").unwrap();
        let ptext = csv_to_text(csv.to_str().unwrap()).unwrap();
        assert!(ptext.contains("x\\|y"), "escaped pipe: {ptext}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_kind_routes_new_formats() {
        let mk = |path: &str| AddSourceInput {
            subject_id: "s".into(),
            topic_id: None,
            name: None,
            kind: None,
            url: None,
            path: Some(path.to_string()),
            text: None,
            tags: vec![],
        };
        for (path, kind) in [
            ("C:\\t\\课程表.xlsx", "xlsx"),
            ("C:\\t\\data.csv", "csv"),
            ("C:\\t\\data.tsv", "csv"),
            ("C:\\t\\scan.tif", "image"),
            ("C:\\t\\scan.bmp", "image"),
            ("C:\\t\\notes.md", "md"),
        ] {
            let got = detect_kind(&mk(path));
            assert_eq!(got, kind, "{path} → {got}");
        }
    }

    #[test]
    fn tiff_and_bmp_convert_to_png_for_ocr() {
        let dir = std::env::temp_dir().join(format!("cortex-img-test-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            8,
            8,
            image::Rgb([220, 40, 40]),
        ));

        // BMP → PNG for the vision path.
        let mut bmp = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bmp, image::ImageFormat::Bmp).unwrap();
        let bmp_path = dir.join("scan.bmp");
        std::fs::write(&bmp_path, bmp.get_ref()).unwrap();
        let (mime, bytes) = image_for_ocr(bmp_path.to_str().unwrap()).unwrap();
        assert_eq!(mime, "image/png");
        assert!(bytes.starts_with(b"\x89PNG"), "should be a PNG now");

        // PNG passes through untouched.
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let png_path = dir.join("shot.png");
        std::fs::write(&png_path, png.get_ref()).unwrap();
        let (mime, bytes) = image_for_ocr(png_path.to_str().unwrap()).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, png.get_ref().clone());

        // The same conversion feeds the stored-original path (browser preview).
        let converted = image_to_png(bmp_path.to_str().unwrap()).unwrap();
        assert!(converted.starts_with(b"\x89PNG"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ooxml_text_extraction_docx_and_pptx_no_tools() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let dir = std::env::temp_dir().join(format!("cortex-ooxml-test-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Minimal .docx: two paragraphs with an escaped ampersand.
        let docx = dir.join("doc.docx");
        {
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&docx).unwrap());
            zw.start_file("word/document.xml", SimpleFileOptions::default()).unwrap();
            zw.write_all(
                br#"<?xml version="1.0"?><w:document><w:body>
                <w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p>
                <w:p><w:r><w:t>Cats &amp; dogs</w:t></w:r></w:p>
                </w:body></w:document>"#,
            ).unwrap();
            zw.finish().unwrap();
        }
        let dtext = ooxml_to_text(docx.to_str().unwrap()).unwrap();
        assert!(dtext.contains("Hello world"), "docx text: {dtext:?}");
        assert!(dtext.contains("Cats & dogs"), "entity decode: {dtext:?}");

        // Minimal .pptx: two slides, slide10 must sort after slide2.
        let pptx = dir.join("deck.pptx");
        {
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&pptx).unwrap());
            for (n, body) in [(2u32, "Second"), (10u32, "Tenth")] {
                zw.start_file(format!("ppt/slides/slide{n}.xml"), SimpleFileOptions::default()).unwrap();
                zw.write_all(format!(r#"<p:sld><a:p><a:r><a:t>{body}</a:t></a:r></a:p></p:sld>"#).as_bytes()).unwrap();
            }
            zw.finish().unwrap();
        }
        let ptext = ooxml_to_text(pptx.to_str().unwrap()).unwrap();
        assert!(ptext.contains("Second") && ptext.contains("Tenth"), "pptx text: {ptext:?}");
        assert!(ptext.find("Second").unwrap() < ptext.find("Tenth").unwrap(), "slide order: {ptext:?}");

        // A non-OOXML file (legacy binary) errors so the caller falls back to LibreOffice.
        let doc = dir.join("legacy.doc");
        std::fs::write(&doc, b"\xD0\xCF\x11\xE0 not a zip").unwrap();
        assert!(ooxml_to_text(doc.to_str().unwrap()).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn epub_text_extraction_no_tools() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let dir = std::env::temp_dir().join(format!("cortex-epub-test-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let epub = dir.join("book.epub");
        {
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&epub).unwrap());
            let opts = SimpleFileOptions::default();
            zw.start_file("META-INF/container.xml", opts).unwrap();
            zw.write_all(
                br#"<?xml version="1.0"?>
                <container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
                <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
                </container>"#,
            ).unwrap();
            zw.start_file("OEBPS/content.opf", opts).unwrap();
            zw.write_all(
                br#"<?xml version="1.0"?>
                <package xmlns="http://www.idpf.org/2007/opf" version="3.0">
                <manifest>
                <item id="c1" href="text/1.xhtml" media-type="application/xhtml+xml"/>
                <item id="c2" href="text/2.xhtml" media-type="application/xhtml+xml"/>
                <item id="css" href="style.css" media-type="text/css"/>
                </manifest>
                <spine><itemref idref="c1"/><itemref idref="c2"/></spine>
                </package>"#,
            ).unwrap();
            zw.start_file("OEBPS/text/1.xhtml", opts).unwrap();
            zw.write_all(br#"<html><body><h1>First chapter</h1><p>Cats &amp; dogs</p></body></html>"#).unwrap();
            zw.start_file("OEBPS/text/2.xhtml", opts).unwrap();
            zw.write_all(br#"<html><body><p>Second chapter</p></body></html>"#).unwrap();
            zw.finish().unwrap();
        }

        let (text, warn) = epub_to_text(epub.to_str().unwrap()).unwrap();
        assert!(warn.is_none(), "unexpected warning: {warn:?}");
        assert!(text.contains("First chapter"), "epub text: {text:?}");
        assert!(text.contains("Second chapter"), "epub text: {text:?}");
        assert!(text.contains("Cats & dogs"), "entity decode: {text:?}");
        // The CSS item is not a content document and must be skipped.
        assert!(!text.contains("style.css"), "non-content leaked: {text:?}");
        // Spine order (c1 before c2) must be honored.
        assert!(
            text.find("First").unwrap() < text.find("Second").unwrap(),
            "reading order: {text:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn html_strip_removes_tags_and_scripts() {
        let html = "<html><head><style>x{}</style></head><body><p>Hello <b>world</b></p><script>bad()</script></body></html>";
        let t = html_to_text(html);
        assert!(t.contains("Hello world"));
        assert!(!t.contains("bad()"));
        assert!(!t.contains('<'));
    }

    #[test]
    fn detect_kind_from_url_and_path() {
        let yt = AddSourceInput {
            subject_id: "s".into(), topic_id: None, name: None, kind: None,
            text: None, path: None, url: Some("https://youtu.be/x".into()), tags: vec![],
        };
        assert_eq!(detect_kind(&yt), "yt");
        let pdf = AddSourceInput {
            subject_id: "s".into(), topic_id: None, name: None, kind: None,
            text: None, path: Some("/tmp/lec.pdf".into()), url: None, tags: vec![],
        };
        assert_eq!(detect_kind(&pdf), "pdf");
    }
}
