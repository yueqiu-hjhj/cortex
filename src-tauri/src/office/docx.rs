//! docx output + tracked-changes write-back. `save_document` builds clean
//! .docx files (materials, research drafts) from neutral `Block`s; the second
//! half writes AI edits into EXISTING docx files as real Word tracked changes
//! (`w:ins`) and margin comments (`word/comments.xml`) — so reviewers
//! accept/reject in Word, not here. Both sides are hand-rolled OOXML over the
//! `zip` crate.

use super::{now_xsd, read_zip_parts, write_zip, xml_escape, xml_unescape};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------- creation

/// A document block, independent of OOXML.
pub enum Block {
    Title(String),
    Heading(String, u8),
    Para(String),
    /// Emphasized lead line (bold run inside a normal paragraph).
    Emph(String),
    Bullets(Vec<String>),
    Quote(String),
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Spacer,
}

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn run(text: &str, bold: bool, italic: bool) -> String {
    let mut rpr = String::new();
    if bold {
        rpr.push_str("<w:b/>");
    }
    if italic {
        rpr.push_str("<w:i/>");
    }
    let rpr = if rpr.is_empty() {
        String::new()
    } else {
        format!("<w:rPr>{rpr}</w:rPr>")
    };
    format!(
        r#"<w:r>{rpr}<w:t xml:space="preserve">{}</w:t></w:r>"#,
        xml_escape(text)
    )
}

fn para(style: Option<&str>, runs: &str) -> String {
    let ppr = style
        .map(|s| format!(r#"<w:pPr><w:pStyle w:val="{s}"/></w:pPr>"#))
        .unwrap_or_default();
    format!("<w:p>{ppr}{runs}</w:p>")
}

fn cell(text: &str, bold: bool, shade: bool) -> String {
    let shd = if shade {
        r#"<w:tcPr><w:shd w:val="clear" w:fill="EDEDF5"/></w:tcPr>"#
    } else {
        "<w:tcPr/>"
    };
    format!(
        "<w:tc>{shd}{}{}</w:tc>",
        para(None, ""),
        para(None, &run(text, bold, false))
    )
}

/// Render blocks to the `word/document.xml` body (without the body wrapper).
fn body_xml(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Title(t) => out.push_str(&para(Some("Title"), &run(t, true, false))),
            Block::Heading(h, lvl) => {
                let lvl = (*lvl).clamp(1, 3);
                out.push_str(&para(Some(format!("Heading{lvl}").as_str()), &run(h, true, false)));
            }
            Block::Para(t) => out.push_str(&para(None, &run(t, false, false))),
            Block::Emph(t) => out.push_str(&para(None, &run(t, true, false))),
            Block::Bullets(items) => {
                for it in items {
                    out.push_str(&format!(
                        "<w:p><w:pPr><w:ind w:left=\"425\"/></w:pPr>{}</w:p>",
                        run(&format!("• {it}"), false, false)
                    ));
                }
            }
            Block::Quote(t) => out.push_str(&format!(
                "<w:p><w:pPr><w:ind w:left=\"425\"/></w:pPr>{}</w:p>",
                run(t, false, true)
            )),
            Block::Table { headers, rows } => {
                let borders = r#"<w:tblPr><w:tblW w:w="0" w:type="auto"/><w:tblBorders><w:top w:val="single" w:sz="4" w:color="AAAAAA"/><w:left w:val="single" w:sz="4" w:color="AAAAAA"/><w:bottom w:val="single" w:sz="4" w:color="AAAAAA"/><w:right w:val="single" w:sz="4" w:color="AAAAAA"/><w:insideH w:val="single" w:sz="4" w:color="AAAAAA"/><w:insideV w:val="single" w:sz="4" w:color="AAAAAA"/></w:tblBorders></w:tblPr>"#;
                let mut tbl = format!("<w:tbl>{borders}");
                tbl.push_str("<w:tr>");
                for h in headers {
                    tbl.push_str(&cell(h, true, true));
                }
                tbl.push_str("</w:tr>");
                for r in rows {
                    tbl.push_str("<w:tr>");
                    for c in r {
                        tbl.push_str(&cell(c, false, false));
                    }
                    tbl.push_str("</w:tr>");
                }
                tbl.push_str("</w:tbl>");
                out.push_str(&tbl);
                out.push_str(&para(None, ""));
            }
            Block::Spacer => out.push_str(&para(None, "")),
        }
    }
    out
}

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="等线"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:styleId="Normal" w:default="1"><w:name w:val="Normal"/><w:qFormat/></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:after="240"/></w:pPr><w:rPr><w:b/><w:sz w:val="44"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:before="240" w:after="120"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:before="180" w:after="90"/></w:pPr><w:rPr><w:b/><w:sz w:val="28"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:before="140" w:after="70"/></w:pPr><w:rPr><w:b/><w:sz w:val="24"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="CommentReference"><w:name w:val="annotation reference"/><w:rPr><w:sz w:val="16"/></w:rPr></w:style>
</w:styles>"#;

/// Build the full docx package parts for a document.
pub fn document_parts(blocks: &[Block]) -> Vec<(String, Vec<u8>)> {
    let body = body_xml(blocks);
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}"><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    vec![
        (
            "[Content_Types].xml".into(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#.into(),
        ),
        (
            "_rels/.rels".into(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.into(),
        ),
        (
            "word/_rels/document.xml.rels".into(),
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.into(),
        ),
        ("word/styles.xml".into(), STYLES.into()),
        ("word/document.xml".into(), document.into_bytes()),
    ]
}

/// Build a .docx file from blocks and write it to `path`.
pub fn save_document(path: &Path, blocks: &[Block]) -> Result<()> {
    write_zip(path, &document_parts(blocks))
}

// ------------------------------------------------- content → docx renderers

fn s(v: Option<&serde_json::Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn arr<'a>(v: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
    v.get(key).and_then(|x| x.as_array()).map(|a| a.as_slice()).unwrap_or(&[])
}

fn arr_owned(v: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    v.get(key).and_then(|x| x.as_array()).cloned().unwrap_or_default()
}

/// Render any generated material (quiz, flashcards, cheatsheet, matrix, guide,
/// slideshow, mindmap, infographic, audio) into document blocks.
pub fn material_blocks(kind: &str, title: &str, meta: Option<&str>, payload: &serde_json::Value) -> Vec<Block> {
    let mut b = vec![Block::Title(title.to_string())];
    if let Some(m) = meta.filter(|m| !m.is_empty()) {
        b.push(Block::Emph(m.to_string()));
    }
    match kind {
        "quiz" => {
            let items = payload.as_array().cloned().unwrap_or_default();
            for (i, it) in items.iter().enumerate() {
                b.push(Block::Heading(format!("{}. {}", i + 1, s(it.get("q"))), 3));
                let options = arr_owned(it, "options")
                    .iter()
                    .map(|o| s(Some(o)))
                    .collect::<Vec<_>>();
                let letters = ["A", "B", "C", "D", "E", "F"];
                let opts = options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| format!("{}. {}", letters.get(i).unwrap_or(&"?"), o))
                    .collect::<Vec<_>>();
                if !opts.is_empty() {
                    b.push(Block::Bullets(opts));
                }
                let ans = it
                    .get("answer")
                    .and_then(|a| a.as_u64())
                    .map(|a| letters.get(a as usize).map(|x| x.to_string()).unwrap_or_else(|| a.to_string()))
                    .unwrap_or_else(|| s(it.get("answer")));
                let explain = s(it.get("explain"));
                b.push(Block::Para(format!(
                    "答案：{}{}",
                    ans,
                    if explain.is_empty() { String::new() } else { format!(" — {explain}") }
                )));
            }
        }
        "flashcards" => {
            let items = payload.as_array().cloned().unwrap_or_default();
            for (i, it) in items.iter().enumerate() {
                b.push(Block::Heading(format!("卡 {}", i + 1), 3));
                b.push(Block::Emph(s(it.get("q"))));
                b.push(Block::Para(s(it.get("a"))));
            }
        }
        "cheatsheet" => {
            for sec in arr(payload, "sections") {
                b.push(Block::Heading(s(sec.get("title")), 2));
                let mut lines = Vec::new();
                for it in arr(sec, "items") {
                    let t = s(it.get("t"));
                    let d = s(it.get("d"));
                    lines.push(if d.is_empty() { t } else { format!("{t} — {d}") });
                }
                if lines.is_empty() {
                    // Generic fallback: any "points"/"text" shape.
                    lines = arr_owned(sec, "points").iter().map(|p| s(Some(p))).collect();
                    let t = s(sec.get("text"));
                    if !t.is_empty() {
                        lines.push(t);
                    }
                }
                b.push(Block::Bullets(lines));
            }
        }
        "matrix" => {
            let headers: Vec<String> = std::iter::once("项目".to_string())
                .chain(arr_owned(payload, "columns").iter().map(|c| s(Some(c))))
                .collect();
            let rows = arr_owned(payload, "rows")
                .iter()
                .map(|r| {
                    std::iter::once(s(r.get("label")))
                        .chain(arr_owned(r, "cells").iter().map(|c| s(Some(c))))
                        .collect::<Vec<_>>()
                })
                .collect();
            b.push(Block::Table { headers, rows });
        }
        "guide" => {
            let overview = s(payload.get("overview"));
            if !overview.is_empty() {
                b.push(Block::Para(overview));
            }
            for sec in arr(payload, "sections") {
                b.push(Block::Heading(s(sec.get("heading")), 2));
                b.push(Block::Bullets(arr_owned(sec, "points").iter().map(|p| s(Some(p))).collect()));
            }
            let pitfalls = arr_owned(payload, "pitfalls").iter().map(|p| s(Some(p))).collect::<Vec<_>>();
            if !pitfalls.is_empty() {
                b.push(Block::Heading("易错点".into(), 2));
                b.push(Block::Bullets(pitfalls));
            }
            let st = arr_owned(payload, "selftest");
            if !st.is_empty() {
                b.push(Block::Heading("自测".into(), 2));
                for (i, it) in st.iter().enumerate() {
                    b.push(Block::Emph(format!("{}. {}", i + 1, s(it.get("q")))));
                    b.push(Block::Para(s(it.get("a"))));
                }
            }
        }
        "slideshow" => {
            for (i, sl) in arr(payload, "slides").iter().enumerate() {
                b.push(Block::Heading(format!("幻灯片 {} — {}", i + 1, s(sl.get("title"))), 2));
                b.push(Block::Bullets(arr_owned(sl, "bullets").iter().map(|p| s(Some(p))).collect()));
                let notes = s(sl.get("notes"));
                if !notes.is_empty() {
                    b.push(Block::Quote(format!("备注：{notes}")));
                }
            }
        }
        "mindmap" => {
            b.push(Block::Emph(s(payload.get("central"))));
            for br in arr(payload, "branches") {
                b.push(Block::Heading(s(br.get("label")), 2));
                let mut lines = Vec::new();
                for ch in arr(br, "children") {
                    let label = s(ch.get("label"));
                    let subs = arr_owned(ch, "children").iter().map(|c| s(Some(c))).collect::<Vec<_>>();
                    if subs.is_empty() {
                        lines.push(label);
                    } else {
                        lines.push(format!("{label}（{}）", subs.join("、")));
                    }
                }
                b.push(Block::Bullets(lines));
            }
        }
        "infographic" => {
            let subtitle = s(payload.get("subtitle"));
            if !subtitle.is_empty() {
                b.push(Block::Emph(subtitle));
            }
            for sec in arr(payload, "sections") {
                let emoji = s(sec.get("emoji"));
                let heading = s(sec.get("heading"));
                b.push(Block::Heading(format!("{emoji} {heading}").trim().to_string(), 2));
                b.push(Block::Bullets(arr_owned(sec, "points").iter().map(|p| s(Some(p))).collect()));
                if let Some(stat) = sec.get("stat") {
                    let v = s(stat.get("value"));
                    let l = s(stat.get("label"));
                    if !v.is_empty() {
                        b.push(Block::Quote(format!("关键数字：{v} {l}")));
                    }
                }
            }
            for tl in arr(payload, "timeline") {
                b.push(Block::Para(format!("• {} — {}：{}", s(tl.get("date")), s(tl.get("title")), s(tl.get("detail")))));
            }
            let takeaway = s(payload.get("takeaway"));
            if !takeaway.is_empty() {
                b.push(Block::Emph(format!("要点：{takeaway}")));
            }
        }
        "audio" => {
            for seg in arr(payload, "segments") {
                b.push(Block::Emph(s(seg.get("speaker"))));
                b.push(Block::Para(s(seg.get("text"))));
            }
        }
        _ => {
            // Unknown shape: dump readable JSON so nothing is silently lost.
            b.push(Block::Para(
                serde_json::to_string_pretty(payload).unwrap_or_default(),
            ));
        }
    }
    b
}

/// Render a research draft (B5) into document blocks, citation numbers intact.
pub fn draft_blocks(title: &str, meta: Option<&str>, payload: &serde_json::Value) -> Vec<Block> {
    let mut b = vec![Block::Title(title.to_string())];
    if let Some(m) = meta.filter(|m| !m.is_empty()) {
        b.push(Block::Emph(m.to_string()));
    }
    let thesis = s(payload.get("thesis"));
    if !thesis.is_empty() {
        b.push(Block::Emph(thesis));
    }
    // Multi-section drafts (D-写): each section becomes a real Heading.
    for sec in arr(payload, "sections") {
        let heading = s(sec.get("heading"));
        if !heading.is_empty() {
            b.push(Block::Heading(heading, 2));
        }
        for p in arr(&sec, "paragraphs") {
            b.push(Block::Para(s(p.get("text"))));
        }
    }
    // Legacy flat drafts keep rendering their paragraphs.
    for p in arr(payload, "paragraphs") {
        b.push(Block::Para(s(p.get("text"))));
    }
    let refs = arr_owned(payload, "references");
    if !refs.is_empty() {
        b.push(Block::Heading("参考文献".into(), 2));
        let rows = refs
            .iter()
            .map(|r| {
                let n = r.get("n").and_then(|x| x.as_i64()).unwrap_or(0);
                let authors = s(r.get("authors"));
                let year = r.get("year").and_then(|x| x.as_i64());
                let page = r.get("page").and_then(|x| x.as_i64());
                let title_ = s(r.get("title"));
                format!(
                    "[{}] {}{}. {}.{}",
                    n,
                    if authors.is_empty() { "佚名".to_string() } else { authors },
                    year.map(|y| format!(" ({y})")).unwrap_or_default(),
                    title_,
                    page.map(|p| format!(" p.{p} 摘录")).unwrap_or_default()
                )
            })
            .map(|t| vec![t])
            .collect();
        b.push(Block::Table { headers: vec!["文献".into()], rows });
    }
    b
}

// ------------------------------------------------------- tracked-changes edit

/// One write-back operation against an existing .docx. `anchor` is a substring
/// matched (case-insensitive) against a paragraph's visible text; `op` is
/// "insert_after" (tracked insertion of `text`) or "comment" (margin comment).
/// For "insert_after", `comment` (if set) becomes a comment attached to the
/// inserted text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocxEdit {
    pub anchor: String,
    pub op: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocxEditOutcome {
    pub applied: usize,
    pub skipped: Vec<String>,
    pub output: String,
}

struct Slot {
    xml: String,
    text: String,
    inserts: Vec<String>,
    comment_id: Option<u32>,
}

fn strip_tags(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut in_tag = false;
    for c in xml.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Split document.xml body into paragraph chunks. Paragraphs never nest, so a
/// linear scan suffices; `<w:p/>` self-closing and `<w:pPr>`-style prefixes are
/// disambiguated by requiring exactly `<w:p>` or `<w:p ` at the tag open.
fn split_paragraphs(doc: &str) -> Result<(String, Vec<String>, String)> {
    let body_open = doc
        .find("<w:body>")
        .ok_or_else(|| Error::Other("document.xml has no <w:body> — not a Word document".into()))?
        + "<w:body>".len();
    let body_close = doc
        .rfind("</w:body>")
        .ok_or_else(|| Error::Other("document.xml has no </w:body>".into()))?;
    let prefix = &doc[..body_open];
    let suffix = &doc[body_close..];
    let body = &doc[body_open..body_close];

    let b = body.as_bytes();
    let mut chunks: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let rest = &body[i..];
        // Take the EARLIEST of the open forms — or_else would miss a `<w:p …>`
        // paragraph preceding the first plain `<w:p>`; `<w:pPr>`/`<w:pict>` etc.
        // never match because they lack the exact `<w:p>`/`<w:p ` opener.
        let open = match (rest.find("<w:p>"), rest.find("<w:p "), rest.find("<w:p/>")) {
            (Some(a), Some(b2), Some(c)) => Some(a.min(b2).min(c)),
            (a, b2, c) => a.or(b2).or(c),
        };
        let Some(rel) = open else {
            // No further paragraphs — the remainder (sectPr etc.) is trailing
            // junk that must be kept verbatim for a faithful rebuild.
            chunks.push(rest.to_string());
            break;
        };
        if rel > 0 {
            chunks.push(rest[..rel].to_string());
        }
        let start = i + rel;
        if body[start..].starts_with("<w:p/>") {
            chunks.push("<w:p/>".to_string());
            i = start + "<w:p/>".len();
        } else {
            // Paragraphs don't nest: the first `</w:p>` closes this one.
            let close = body[start..]
                .find("</w:p>")
                .ok_or_else(|| Error::Other("unbalanced <w:p> in document.xml".into()))?;
            let end = start + close + "</w:p>".len();
            chunks.push(body[start..end].to_string());
            i = end;
        }
    }
    Ok((prefix.to_string(), chunks, suffix.to_string()))
}

/// A brand-new paragraph whose text is a tracked insertion by `author` (and
/// whose paragraph mark is likewise an insertion), optionally carrying a
/// comment anchor. Word renders this exactly like a human-typed suggestion.
fn tracked_insert_par(id: u32, comment_id: Option<u32>, author: &str, date: &str, text: &str) -> String {
    let mut out = format!(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="{id}" w:author="{}" w:date="{}"/></w:rPr></w:pPr>"#,
        xml_escape(author),
        xml_escape(date)
    );
    out.push_str(&format!(
        r#"<w:ins w:id="{}" w:author="{}" w:date="{}"><w:r><w:t xml:space="preserve">{}</w:t></w:r></w:ins>"#,
        id + 1,
        xml_escape(author),
        xml_escape(date),
        xml_escape(text)
    ));
    if let Some(cid) = comment_id {
        out.push_str(&format!(
            r#"<w:commentRangeStart w:id="{cid}"/><w:commentRangeEnd w:id="{cid}"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="{cid}"/></w:r>"#
        ));
    }
    out.push_str("</w:p>");
    out
}

fn comment_body(id: u32, author: &str, initials: &str, date: &str, text: &str) -> String {
    format!(
        r#"<w:comment w:id="{id}" w:author="{}" w:initials="{}" w:date="{}"><w:p><w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p></w:comment>"#,
        xml_escape(author),
        xml_escape(initials),
        xml_escape(date),
        xml_escape(text)
    )
}

/// Attach a comment to an existing paragraph: the range opens right after
/// `<w:pPr>` (pPr must stay the first child) and closes before `</w:p>`.
fn annotate_paragraph(slot: &mut Slot, cid: u32) {
    let end_marker = format!(
        r#"<w:commentRangeEnd w:id="{cid}"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="{cid}"/></w:r>"#
    );
    if let Some(pos) = slot.xml.rfind("</w:p>") {
        slot.xml.insert_str(pos, &end_marker);
    }
    let open_end = slot.xml.find('>').map(|p| p + 1).unwrap_or(0);
    let insert_at = if slot.xml[open_end..].starts_with("<w:pPr") {
        slot.xml[open_end..]
            .find("</w:pPr>")
            .map(|r| open_end + r + "</w:pPr>".len())
            .unwrap_or(open_end)
    } else {
        open_end
    };
    slot.xml.insert_str(insert_at, &format!(r#"<w:commentRangeStart w:id="{cid}"/>"#));
}

/// Append freshly-generated `<w:comment>` bodies into `word/comments.xml`,
/// creating (and wiring up content-types + rels for) the part if missing.
fn ensure_comment_part(parts: &mut Vec<(String, Vec<u8>)>, comments_xml: &str) -> Result<()> {
    match parts.iter_mut().find(|(n, _)| n == "word/comments.xml") {
        Some((_, bytes)) => {
            let s = std::str::from_utf8(bytes)?;
            let pos = s
                .rfind("</w:comments>")
                .ok_or_else(|| Error::Other("corrupt word/comments.xml".into()))?;
            let mut out = String::with_capacity(s.len() + comments_xml.len());
            out.push_str(&s[..pos]);
            out.push_str(comments_xml);
            out.push_str(&s[pos..]);
            *bytes = out.into_bytes();
        }
        None => {
            // [Content_Types].xml override.
            let ct_idx = parts
                .iter()
                .position(|(n, _)| n == "[Content_Types].xml")
                .ok_or_else(|| Error::Other("docx package has no [Content_Types].xml".into()))?;
            let ct = std::str::from_utf8(&parts[ct_idx].1)?;
            let override_decl = r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#;
            let ct = match ct.rfind("</Types>") {
                Some(p) => format!("{}{override_decl}{}", &ct[..p], &ct[p..]),
                None => return Err(Error::Other("corrupt [Content_Types].xml".into())),
            };
            parts[ct_idx].1 = ct.into_bytes();
            // document.xml.rels relationship.
            let rels_idx = parts
                .iter()
                .position(|(n, _)| n == "word/_rels/document.xml.rels")
                .ok_or_else(|| Error::Other("docx package has no document.xml.rels".into()))?;
            let rels = std::str::from_utf8(&parts[rels_idx].1)?;
            let rel_decl = r#"<Relationship Id="rIdC1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>"#;
            let rels = match rels.rfind("</Relationships>") {
                Some(p) => format!("{}{rel_decl}{}", &rels[..p], &rels[p..]),
                None => return Err(Error::Other("corrupt document.xml.rels".into())),
            };
            parts[rels_idx].1 = rels.into_bytes();
            parts.push((
                "word/comments.xml".into(),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="{W_NS}">{comments_xml}</w:comments>"#
                )
                .into_bytes(),
            ));
        }
    }
    Ok(())
}

/// Visible paragraph texts of an existing .docx, in document order (non-empty
/// only). This is the input the revision helper feeds the model when proposing
/// tracked-change edits — anchors must match against these strings.
pub fn paragraph_texts(path: &Path) -> Result<Vec<String>> {
    let parts = read_zip_parts(path)?;
    let doc = parts
        .iter()
        .find(|(n, _)| n == "word/document.xml")
        .ok_or_else(|| Error::Other("not a .docx package (no word/document.xml)".into()))?;
    let xml = std::str::from_utf8(&doc.1)?;
    let (_, chunks, _) = split_paragraphs(xml)?;
    Ok(chunks
        .into_iter()
        .filter(|c| c.starts_with("<w:p>") || c.starts_with("<w:p ") || c.starts_with("<w:p/>"))
        .map(|c| xml_unescape(&strip_tags(&c)))
        .filter(|t| !t.trim().is_empty())
        .collect())
}

/// Apply edits to an existing .docx, writing the result to `output`.
/// Unmatched anchors are reported as skipped, never silently dropped.
pub fn apply_edits(
    input: &Path,
    output: &Path,
    author: &str,
    edits: &[DocxEdit],
) -> Result<DocxEditOutcome> {
    if input == output {
        return Err(Error::Other("输出文件需与原文件不同（Word 修订需另存新文件）".into()));
    }
    let mut parts = read_zip_parts(input)?;
    let doc_idx = parts
        .iter()
        .position(|(n, _)| n == "word/document.xml")
        .ok_or_else(|| Error::Other("not a .docx package (no word/document.xml)".into()))?;
    let doc_xml = std::str::from_utf8(&parts[doc_idx].1)?
        .to_string();
    let (prefix, chunks, suffix) = split_paragraphs(&doc_xml)?;

    // Group into slots: contiguous junk chunks stay untouched; paragraph chunks
    // become slots that can receive comments/insertions.
    let mut slots: Vec<Option<Slot>> = Vec::with_capacity(chunks.len());
    for c in &chunks {
        if c.starts_with("<w:p>") || c.starts_with("<w:p ") || c.starts_with("<w:p/") {
            slots.push(Some(Slot {
                xml: c.clone(),
                text: xml_unescape(&strip_tags(c)),
                inserts: Vec::new(),
                comment_id: None,
            }));
        } else {
            slots.push(None);
        }
    }

    let date = now_xsd();
    let mut next_id = 1001u32;
    let mut comment_bodies = String::new();
    let mut applied = 0usize;
    let mut skipped: Vec<String> = Vec::new();

    for edit in edits {
        let anchor = edit.anchor.trim();
        if anchor.is_empty() {
            skipped.push("(空 anchor)".into());
            continue;
        }
        let needle = anchor.to_lowercase();
        let hit = slots.iter_mut().flatten().find(|s| {
            !s.text.is_empty() && s.text.to_lowercase().contains(&needle)
        });
        match edit.op.as_str() {
            "comment" => {
                if let Some(slot) = hit {
                    let cid = next_id;
                    next_id += 1;
                    slot.comment_id = Some(cid);
                    comment_bodies.push_str(&comment_body(
                        cid,
                        author,
                        "AI",
                        &date,
                        if edit.comment.is_empty() { edit.text.as_str() } else { edit.comment.as_str() },
                    ));
                    applied += 1;
                } else {
                    skipped.push(anchor.chars().take(40).collect());
                }
            }
            "insert_after" | _ => {
                if edit.text.trim().is_empty() {
                    skipped.push(anchor.chars().take(40).collect());
                    continue;
                }
                if let Some(slot) = hit {
                    // An optional comment rides on the inserted paragraph.
                    let cid = if edit.comment.trim().is_empty() {
                        None
                    } else {
                        let cid = next_id;
                        next_id += 1;
                        comment_bodies.push_str(&comment_body(cid, author, "AI", &date, &edit.comment));
                        Some(cid)
                    };
                    slot.inserts.push(tracked_insert_par(next_id, cid, author, &date, &edit.text));
                    next_id += 2;
                    applied += 1;
                } else {
                    skipped.push(anchor.chars().take(40).collect());
                }
            }
        }
    }

    // Rebuild document.xml: prefix + (annotated paragraphs, kept junk, fresh
    // insertions) + suffix — byte-faithful everywhere we didn't touch.
    let mut doc = String::with_capacity(doc_xml.len() + 8192);
    doc.push_str(&prefix);
    for (i, slot) in slots.iter().enumerate() {
        match slot {
            Some(s) => {
                // Late-bound comment markers: comment bodies were allocated in
                // the edit loop, so decorate now that ids are final.
                let mut xml = s.xml.clone();
                if let Some(cid) = s.comment_id {
                    let end_marker = format!(
                        r#"<w:commentRangeEnd w:id="{cid}"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="{cid}"/></w:r>"#
                    );
                    if let Some(pos) = xml.rfind("</w:p>") {
                        xml.insert_str(pos, &end_marker);
                    }
                    let open_end = xml.find('>').map(|p| p + 1).unwrap_or(0);
                    let insert_at = if xml[open_end..].starts_with("<w:pPr") {
                        xml[open_end..]
                            .find("</w:pPr>")
                            .map(|r| open_end + r + "</w:pPr>".len())
                            .unwrap_or(open_end)
                    } else {
                        open_end
                    };
                    xml.insert_str(insert_at, &format!(r#"<w:commentRangeStart w:id="{cid}"/>"#));
                }
                doc.push_str(&xml);
                for ins in &s.inserts {
                    doc.push_str(ins);
                }
            }
            None => doc.push_str(&chunks[i]),
        }
    }
    doc.push_str(&suffix);
    parts[doc_idx].1 = doc.into_bytes();

    if !comment_bodies.is_empty() {
        ensure_comment_part(&mut parts, &comment_bodies)?;
    }
    write_zip(output, &parts)?;
    Ok(DocxEditOutcome {
        applied,
        skipped,
        output: output.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_roundtrip_and_material_render() {
        let dir = std::env::temp_dir().join(format!("cortex-docx-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("m.docx");
        let payload = serde_json::json!([
            {"q": "什么是栈?", "options": ["LIFO", "FIFO"], "answer": 0, "explain": "后进先出"}
        ]);
        let blocks = material_blocks("quiz", "数据结构<测验>", Some("第一章"), &payload);
        save_document(&p, &blocks).unwrap();
        let parts = read_zip_parts(&p).unwrap();
        assert!(parts.iter().any(|(n, _)| n == "word/document.xml"));
        assert!(parts.iter().any(|(n, _)| n == "word/styles.xml"));
        // Escaped title survives in the XML.
        let doc = parts
            .iter()
            .find(|(n, _)| n == "word/document.xml")
            .map(|(_, b)| String::from_utf8_lossy(b).to_string())
            .unwrap();
        assert!(doc.contains("数据结构&lt;测验&gt;"), "doc: {doc}");
        // Natively extractable by the ingest path.
        let text = crate::ingest::ooxml_to_text(p.to_str().unwrap()).unwrap();
        assert!(text.contains("什么是栈?"), "text: {text}");
        assert!(text.contains("答案：A"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracked_edits_land_as_wins_and_comments() {
        let dir = std::env::temp_dir().join(format!("cortex-docx-edit-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.docx");
        let out = dir.join("out.docx");
        save_document(
            &src,
            &[
                Block::Title("综述".into()),
                Block::Para("本研究关注多模态大模型的检索增强生成。".into()),
                Block::Para("实验表明该方法优于基线。".into()),
            ],
        )
        .unwrap();

        let edits = vec![
            DocxEdit {
                anchor: "检索增强生成".into(),
                op: "comment".into(),
                text: "建议补充 RAG 的引用来源".into(),
                comment: String::new(),
            },
            DocxEdit {
                anchor: "优于基线".into(),
                op: "insert_after".into(),
                text: "具体提升幅度见第三章的消融实验。".into(),
                comment: "数据待确认".into(),
            },
            // Missing anchor must be reported, never silently dropped.
            DocxEdit {
                anchor: "不存在的段落".into(),
                op: "comment".into(),
                text: "x".into(),
                comment: String::new(),
            },
        ];
        let outcome = apply_edits(&src, &out, "AI 审阅", &edits).unwrap();
        assert_eq!(outcome.applied, 2);
        assert_eq!(outcome.skipped.len(), 1);

        let parts = read_zip_parts(&out).unwrap();
        let doc = parts
            .iter()
            .find(|(n, _)| n == "word/document.xml")
            .map(|(_, b)| String::from_utf8_lossy(b).to_string())
            .unwrap();
        assert!(doc.contains("<w:ins "), "tracked insertion missing: {doc}");
        assert!(doc.contains("w:commentRangeStart"), "comment range missing");
        assert!(doc.contains("具体提升幅度见第三章的消融实验。"));
        let comments = parts
            .iter()
            .find(|(n, _)| n == "word/comments.xml")
            .map(|(_, b)| String::from_utf8_lossy(b).to_string())
            .expect("comments part");
        assert!(comments.contains("建议补充 RAG 的引用来源"));
        assert!(comments.contains("数据待确认"));
        // Content types + rels wired for the new part.
        let ct = parts
            .iter()
            .find(|(n, _)| n == "[Content_Types].xml")
            .map(|(_, b)| String::from_utf8_lossy(b).to_string())
            .unwrap();
        assert!(ct.contains("/word/comments.xml"));
        let rels = parts
            .iter()
            .find(|(n, _)| n == "word/_rels/document.xml.rels")
            .map(|(_, b)| String::from_utf8_lossy(b).to_string())
            .unwrap();
        assert!(rels.contains("relationships/comments"));
        // The edited file still extracts cleanly (structure intact).
        let text = crate::ingest::ooxml_to_text(out.to_str().unwrap()).unwrap();
        assert!(text.contains("本研究关注多模态大模型"), "text: {text}");
        // Paragraph extraction feeds the model the doc's visible text.
        let paras = paragraph_texts(&out).unwrap();
        assert!(paras.iter().any(|p| p.contains("优于基线")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
