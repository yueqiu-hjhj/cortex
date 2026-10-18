//! PDF editing via lopdf — the operations a study workflow actually needs:
//! merge handouts into one file, split readings per page, stamp drafts with a
//! diagonal watermark, and fill AcroForm fields (worksheets,申请表). Values are
//! written with `NeedAppearances` so the viewer regenerates field appearance —
//! we never draw widget appearance ourselves.

use crate::error::{Error, Result};
use std::f64::consts::FRAC_1_SQRT_2;

use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use std::collections::BTreeMap;
use std::path::Path;

/// A form field reported by `form_fields`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PdfField {
    pub name: String,
    /// text | checkbox | radio | choice | pushbutton | signature | other
    pub kind: String,
    pub value: String,
    pub options: Vec<String>,
}

fn load_pdf(path: &Path) -> Result<Document> {
    Document::load(path).map_err(|e| Error::Other(format!("PDF 读取失败: {e}")))
}

fn save_pdf(doc: &mut Document, dest: &Path) -> Result<()> {
    doc.save(dest).map(|_| ()).map_err(|e| Error::Other(format!("PDF 写入失败: {e}")))
}

// ------------------------------------------------------------ merge

/// Concatenate the pages of all `paths` into one document at `dest`, in order.
/// Follows the lopdf merge recipe: renumber each source to a disjoint id range,
/// hoist every object, then rebuild one shared Pages tree + Catalog.
pub fn merge(paths: &[String], dest: &Path) -> Result<()> {
    if paths.len() < 2 {
        return Err(Error::Other("合并至少需要两个 PDF".into()));
    }
    let mut document = Document::with_version("1.5");
    let mut pages_map: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut all_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut max_id = 1u32;

    for p in paths {
        let mut doc = load_pdf(Path::new(p))?;
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;
        for (_num, page_id) in doc.get_pages() {
            let obj = doc.objects.get(&page_id).cloned().unwrap_or(Object::Null);
            pages_map.insert(page_id, obj);
        }
        all_objects.extend(doc.objects);
    }

    let mut catalog_object: Option<(ObjectId, Object)> = None;
    let mut pages_object: Option<(ObjectId, Object)> = None;

    for (object_id, object) in all_objects {
        match object.type_name().unwrap_or(b"") {
            b"Catalog" => {
                // First catalog wins; later ones are ignored.
                catalog_object = Some((
                    catalog_object.map(|(id, _)| id).unwrap_or(object_id),
                    object,
                ));
            }
            b"Pages" => {
                if let Ok(dictionary) = object.as_dict() {
                    let mut dictionary = dictionary.clone();
                    // Merge successive Pages dicts (they mostly differ in nothing).
                if let Some((_, ref existing)) = pages_object {
                    if let Ok(old) = existing.as_dict() {
                        dictionary.extend(old);
                    }
                }
                    pages_object = Some((
                        pages_object.map(|(id, _)| id).unwrap_or(object_id),
                        Object::Dictionary(dictionary),
                    ));
                }
            }
            // Page objects come from pages_map; outlines are dropped.
            b"Page" | b"Outlines" | b"Outline" => {}
            _ => {
                document.objects.insert(object_id, object);
            }
        }
    }

    let Some((page_id, page_object)) = pages_object else {
        return Err(Error::Other("某个输入 PDF 没有 页对象（可能损坏）".into()));
    };
    let Some((catalog_id, catalog_object)) = catalog_object else {
        return Err(Error::Other("某个输入 PDF 没有 Catalog（可能损坏）".into()));
    };

    for (object_id, object) in pages_map.iter() {
        if let Ok(dictionary) = object.as_dict() {
            let mut dictionary = dictionary.clone();
            dictionary.set("Parent", page_id);
            document.objects.insert(*object_id, Object::Dictionary(dictionary));
        }
    }

    if let Ok(dictionary) = page_object.as_dict() {
        let mut dictionary = dictionary.clone();
        dictionary.set("Count", pages_map.len() as u32);
        dictionary.set(
            "Kids",
            pages_map.keys().map(|k| Object::Reference(*k)).collect::<Vec<_>>(),
        );
        document.objects.insert(page_id, Object::Dictionary(dictionary));
    }

    if let Ok(dictionary) = catalog_object.as_dict() {
        let mut dictionary = dictionary.clone();
        dictionary.set("Pages", page_id);
        dictionary.remove(b"Outlines");
        document.objects.insert(catalog_id, Object::Dictionary(dictionary));
    }

    document.trailer.set("Root", catalog_id);
    document.max_id = document.objects.len() as u32;
    document.renumber_objects();
    save_pdf(&mut document, dest)
}

// ------------------------------------------------------------ split

/// Deep-copy the reference closure rooted at `page_id`, but do NOT follow a
/// Page's `Parent` — the old pages tree would drag every sibling page in.
fn page_closure(doc: &Document, page_id: ObjectId) -> BTreeMap<ObjectId, Object> {
    fn collect(obj: &Object, stack: &mut Vec<ObjectId>, skip_parent: bool) {
        match obj {
            Object::Reference(id) => stack.push(*id),
            Object::Array(a) => {
                for o in a {
                    collect(o, stack, false);
                }
            }
            Object::Dictionary(d) => {
                for (k, v) in d.iter() {
                    if skip_parent && k.as_slice() == b"Parent" {
                        continue;
                    }
                    collect(v, stack, false);
                }
            }
            Object::Stream(s) => {
                for (k, v) in s.dict.iter() {
                    if skip_parent && k.as_slice() == b"Parent" {
                        continue;
                    }
                    collect(v, stack, false);
                }
            }
            _ => {}
        }
    }
    let mut seen = BTreeMap::new();
    let mut stack = vec![page_id];
    while let Some(id) = stack.pop() {
        if seen.contains_key(&id) {
            continue;
        }
        if let Some(obj) = doc.objects.get(&id) {
            seen.insert(id, obj.clone());
            collect(obj, &mut stack, true);
        }
    }
    seen
}

fn remap_object(obj: &Object, map: &BTreeMap<ObjectId, ObjectId>) -> Object {
    match obj {
        Object::Reference(id) => Object::Reference(map.get(id).copied().unwrap_or(*id)),
        Object::Array(a) => Object::Array(a.iter().map(|o| remap_object(o, map)).collect()),
        Object::Dictionary(d) => Object::Dictionary(
            d.iter()
                .map(|(k, v)| (k.clone(), remap_object(v, map)))
                .collect(),
        ),
        Object::Stream(s) => {
            let dict = s
                .dict
                .iter()
                .map(|(k, v)| (k.clone(), remap_object(v, map)))
                .collect();
            Object::Stream(Stream::new(dict, s.content.clone()))
        }
        _ => obj.clone(),
    }
}

/// Split into one PDF per page, written to `out_dir` as `{stem}-001.pdf` ….
/// Returns the created file paths in page order.
pub fn split(path: &Path, out_dir: &Path, stem: &str) -> Result<Vec<String>> {
    let doc = load_pdf(path)?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err(Error::Other("该 PDF 没有页面".into()));
    }
    std::fs::create_dir_all(out_dir)?;
    let mut outs = Vec::new();
    for (num, page_id) in &pages {
        let closure = page_closure(&doc, *page_id);
        let mut out = Document::with_version("1.5");
        let mut map: BTreeMap<ObjectId, ObjectId> = BTreeMap::new();
        for &old in closure.keys() {
            map.insert(old, out.add_object(Object::Null));
        }
        for (old, obj) in &closure {
            let new_obj = remap_object(obj, &map);
            out.set_object(map[old], new_obj);
        }
        let new_page = map[page_id];
        let pages_id = out.add_object(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(new_page)],
            "Count" => 1,
        });
        if let Ok(d) = out.get_object_mut(new_page).and_then(|o| o.as_dict_mut()) {
            d.set("Parent", pages_id);
        }
        let catalog_id = out.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        out.trailer.set("Root", catalog_id);
        out.max_id = out.objects.len() as u32;
        out.renumber_objects();

        let dest = out_dir.join(format!("{stem}-{num:03}.pdf"));
        out.save(&dest).map_err(|e| Error::Other(format!("PDF 写入失败: {e}")))?;
        outs.push(dest.to_string_lossy().into_owned());
    }
    Ok(outs)
}

// ------------------------------------------------------------ watermark

fn pdf_string_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

/// Add `{name: obj}` to `res[category]`, preserving whatever sub-dictionary was
/// already there (ExtGState/Font entries of the original page).
fn add_entry(res: &mut Dictionary, category: &[u8], name: &str, obj: Object) {
    let mut sub = match res.get(category) {
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };
    sub.set(name, obj);
    res.set(category.to_vec(), Object::Dictionary(sub));
}

fn page_size(doc: &Document, page_id: ObjectId) -> (f64, f64) {
    let w = |o: &Object| -> f64 {
        o.as_i64().map(|i| i as f64).unwrap_or_else(|_| o.as_float().map(|f| f as f64).unwrap_or(0.0))
    };
    let (mut width, mut height) = (595.0f64, 842.0f64);
    if let Ok(d) = doc.get_object(page_id).and_then(|o| o.as_dict()) {
        if let Ok(a) = d.get(b"MediaBox").and_then(|o| o.as_array()) {
            if a.len() >= 4 {
                width = w(&a[2]) - w(&a[0]);
                height = w(&a[3]) - w(&a[1]);
            }
        }
    }
    (width.abs().max(1.0), height.abs().max(1.0))
}

/// Stamp every page with a 45° diagonal translucent watermark. The standard-14
/// Helvetica font has no CJK glyphs, so non-Latin text is rejected rather than
/// rendered as garbage.
pub fn watermark(path: &Path, text: &str, dest: &Path) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Other("水印文字不能为空".into()));
    }
    if text.chars().any(|c| c as u32 > 0xFF) {
        return Err(Error::Other(
            "水印暂只支持拉丁字符（PDF 标准字体无中文字形），如 DRAFT、CONFIDENTIAL".into(),
        ));
    }
    let mut doc = load_pdf(path)?;
    let pages = doc.get_pages();

    let gs_id = doc.add_object(dictionary! {
        "Type" => "ExtGState",
        "ca" => 0.22,
        "CA" => 0.22,
    });
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Bold",
    });

    for page_id in pages.values() {
        let (w, h) = page_size(&doc, *page_id);
        let fs = (w.min(h) * 0.12).clamp(24.0, 96.0);
        let len = text.chars().count() as f64;
        let text_w = fs * 0.55 * len;
        let diag = text_w * FRAC_1_SQRT_2;
        let tx = (w - diag) / 2.0;
        let ty = (h + diag) / 2.0 - fs;
        let content = format!(
            "q /GSWM gs 0.55 0.55 0.55 rg BT /FWM {fs:.1} Tf {} {} -{} {} {tx:.1} {ty:.1} Tm ({}) Tj ET Q",
            FRAC_1_SQRT_2, FRAC_1_SQRT_2, FRAC_1_SQRT_2, FRAC_1_SQRT_2,
            pdf_string_escape(text)
        );
        let stream_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));

        // Resources: copy the page's resources to a fresh dict (never mutate a
        // shared one in place), then add our ExtGState/Font entries. Resolve
        // the old resources BEFORE the mutable borrow below.
        let mut res = match doc.get_object(*page_id).and_then(|o| o.as_dict())?.get(b"Resources") {
            Ok(Object::Reference(rid)) => doc
                .get_object(*rid)
                .and_then(|o| o.as_dict().cloned())
                .unwrap_or_else(|_| Dictionary::new()),
            Ok(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        };
        add_entry(&mut res, b"ExtGState", "GSWM", Object::Reference(gs_id));
        add_entry(&mut res, b"Font", "FWM", Object::Reference(font_id));
        let res_id = doc.add_object(res);
        let page_dict = doc.get_object_mut(*page_id).and_then(|o| o.as_dict_mut())?;
        page_dict.set("Resources", Object::Reference(res_id));

        // Contents: append our stream, preserving whatever was there.
        let old = page_dict.get(b"Contents").cloned().ok();
        match old {
            Some(Object::Array(a)) => {
                let mut v = a;
                v.push(Object::Reference(stream_id));
                page_dict.set("Contents", Object::Array(v));
            }
            Some(obj @ (Object::Reference(_) | Object::Stream(_))) => {
                page_dict.set("Contents", Object::Array(vec![obj, Object::Reference(stream_id)]));
            }
            _ => {
                page_dict.set("Contents", Object::Reference(stream_id));
            }
        }
    }
    save_pdf(&mut doc, dest)
}

// ------------------------------------------------------------ forms

fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn encode_pdf_string(value: &str) -> (Vec<u8>, StringFormat) {
    if value.is_ascii() {
        (value.as_bytes().to_vec(), StringFormat::Literal)
    } else {
        let mut out = vec![0xFE, 0xFF];
        for u in value.encode_utf16() {
            out.extend_from_slice(&u.to_be_bytes());
        }
        (out, StringFormat::Hexadecimal)
    }
}

/// Display text of a /V value (or any object) for `form_fields` reporting.
fn obj_to_text(doc: &Document, o: &Object, depth: usize) -> String {
    if depth > 4 {
        return String::new();
    }
    match o {
        Object::String(bytes, _) => decode_pdf_string(bytes),
        Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
        Object::Integer(i) => i.to_string(),
        Object::Real(f) => f.to_string(),
        Object::Boolean(b) => b.to_string(),
        Object::Reference(rid) => doc
            .get_object(*rid)
            .map(|o| obj_to_text(doc, o, depth + 1))
            .unwrap_or_default(),
        Object::Array(a) => a
            .iter()
            .map(|o| obj_to_text(doc, o, depth + 1))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

fn field_kind(d: &Dictionary) -> String {
    let ft = d
        .get(b"FT")
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    match ft.as_str() {
        "Tx" => "text",
        "Btn" => {
            // A radio group declares /Ff with the radio flag (bit 16).
            let radio = d
                .get(b"Ff")
                .ok()
                .and_then(|o| o.as_i64().ok())
                .map(|f| f & (1 << 16) != 0)
                .unwrap_or(false);
            if radio { "radio" } else { "checkbox" }
        }
        "Ch" => "choice",
        "Sig" => "signature",
        "" => "other",
        _ => "other",
    }
    .to_string()
}

/// Enumerate AcroForm fields. Any object carrying /T counts — both standalone
/// widget annotations and parent fields whose /FT the widgets inherit.
pub fn form_fields(path: &Path) -> Result<Vec<PdfField>> {
    let doc = load_pdf(path)?;
    let mut fields: Vec<PdfField> = Vec::new();
    for obj in doc.objects.values() {
        let Ok(d) = obj.as_dict() else { continue };
        if !d.has(b"T") {
            continue;
        }
        // Skip pure widgets without a field type on itself or inherited —
        // those appear when a parent holds /FT; the parent is listed anyway.
        let name = d
            .get(b"T")
            .map(|t| match t {
                Object::String(b, _) => decode_pdf_string(b),
                Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
                other => obj_to_text(&doc, other, 0),
            })
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let kind = field_kind(d);
        let value = d.get(b"V").map(|v| obj_to_text(&doc, v, 0)).unwrap_or_default();
        let mut options: Vec<String> = d
            .get(b"Opt")
            .ok()
            .and_then(|o| o.as_array().ok())
            .map(|a| a.iter().map(|o| obj_to_text(&doc, o, 0)).collect())
            .unwrap_or_default();
        if options.is_empty() && kind == "checkbox" {
            // States live in /AP /N keys ("Yes", "Off", …).
            if let Ok(Object::Dictionary(ap)) = d.get(b"AP") {
                if let Ok(Object::Dictionary(n)) = ap.get(b"N") {
                    options = n
                        .iter()
                        .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                        .filter(|k| k != "Off")
                        .collect();
                }
            }
        }
        fields.push(PdfField { name, kind, value, options });
    }
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields.dedup_by(|a, b| a.name == b.name);
    Ok(fields)
}

/// Fill fields by exact /T name match. Returns the number of field names that
/// were found and written; unknown names are reported via the count only.
pub fn fill_form(path: &Path, values: &[(String, String)], dest: &Path) -> Result<usize> {
    if values.is_empty() {
        return Err(Error::Other("没有提供要填写的字段".into()));
    }
    let mut doc = load_pdf(path)?;
    let mut applied = 0usize;
    for (name, value) in values {
        // PDF field names use PDFDocEncoding or UTF-16BE; match on the decoded form.
        let targets: Vec<ObjectId> = doc
            .objects
            .iter()
            .filter(|(_, o)| {
                o.as_dict()
                    .map(|d| {
                        d.get(b"T")
                            .map(|t| match t {
                                Object::String(b, _) => decode_pdf_string(b) == *name,
                                Object::Name(n) => String::from_utf8_lossy(n) == name.as_str(),
                                _ => false,
                            })
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
            })
            .map(|(id, _)| *id)
            .collect();
        if targets.is_empty() {
            continue;
        }
        for id in targets {
            let dict = doc.get_object_mut(id).and_then(|o| o.as_dict_mut())?;
            let kind = field_kind(dict);
            let is_name_value = matches!(kind.as_str(), "checkbox" | "radio" | "choice");
            if is_name_value {
                dict.set("V", Object::Name(value.clone().into_bytes()));
                // Keep /AS in sync so checkboxes render without a regen pass.
                let states: Vec<String> = dict
                    .get(b"AP")
                    .ok()
                    .and_then(|ap| ap.as_dict().ok())
                    .and_then(|ap| ap.get(b"N").ok())
                    .and_then(|n| n.as_dict().ok())
                    .map(|n| {
                        n.iter()
                            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                if states.iter().any(|s| s == value) {
                    dict.set("AS", Object::Name(value.clone().into_bytes()));
                }
            } else {
                let (bytes, fmt) = encode_pdf_string(value);
                dict.set("V", Object::String(bytes, fmt));
            }
        }
        applied += 1;
    }
    // Ask viewers to regenerate field appearance — we never draw widgets.
    let acro = doc
        .catalog()
        .and_then(|c| c.get(b"AcroForm").cloned());
    match acro {
        Ok(Object::Reference(rid)) => {
            if let Ok(d) = doc.get_object_mut(rid).and_then(|o| o.as_dict_mut()) {
                d.set("NeedAppearances", Object::Boolean(true));
            }
        }
        Ok(Object::Dictionary(_)) => {
            if let Ok(c) = doc.catalog_mut() {
                if let Ok(d) = c.get_mut(b"AcroForm").and_then(|o| o.as_dict_mut()) {
                    d.set("NeedAppearances", Object::Boolean(true));
                }
            }
        }
        _ => {}
    }
    save_pdf(&mut doc, dest)?;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Object;

    fn one_page_pdf(text: &str) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = format!("BT /F1 24 Tf 72 720 Td ({text}) Tj ET");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.renumber_objects();
        doc
    }

    fn write_tmp(dir: &std::path::Path, name: &str, doc: &mut Document) -> String {
        let p = dir.join(name);
        doc.save(&p).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn merge_then_split_roundtrip() {
        let dir = std::env::temp_dir().join(format!("cortex-pdf-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = write_tmp(&dir, "a.pdf", &mut one_page_pdf("Alpha"));
        let b = write_tmp(&dir, "b.pdf", &mut one_page_pdf("Beta"));

        let dest = dir.join("merged.pdf");
        merge(&[a, b], &dest).unwrap();
        let merged = Document::load(&dest).unwrap();
        assert_eq!(merged.get_pages().len(), 2);

        let out_dir = dir.join("parts");
        let files = split(&dest, &out_dir, "merged").unwrap();
        assert_eq!(files.len(), 2);
        for f in &files {
            let d = Document::load(f).unwrap();
            assert_eq!(d.get_pages().len(), 1, "{f}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watermark_adds_stream_and_rejects_cjk() {
        let dir = std::env::temp_dir().join(format!("cortex-pdf-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = write_tmp(&dir, "w.pdf", &mut one_page_pdf("Hello"));
        let dest = dir.join("watermarked.pdf");
        watermark(Path::new(&src), "DRAFT (v2)", &dest).unwrap();
        let doc = Document::load(&dest).unwrap();
        let (_, page_id) = doc.get_pages().pop_first().unwrap();
        let d = doc.get_object(page_id).unwrap().as_dict().unwrap();
        let contents = d.get(b"Contents").unwrap().as_array().unwrap();
        assert_eq!(contents.len(), 2, "original + watermark stream");
        let wm = contents[1].as_reference().unwrap();
        let wm_bytes = doc
            .get_object(wm)
            .unwrap()
            .as_stream()
            .unwrap()
            .content
            .clone();
        let wm_text = String::from_utf8_lossy(&wm_bytes);
        assert!(wm_text.contains("DRAFT \\(v2\\)"), "escaped: {wm_text}");
        assert!(wm_text.contains("/GSWM"));
        // Resources got our GSWM/FWM entries.
        let res = d.get(b"Resources").unwrap().as_reference().unwrap();
        let res_d = doc.get_object(res).unwrap().as_dict().unwrap();
        assert!(res_d.has(b"ExtGState") && res_d.has(b"Font"));

        let err = watermark(Path::new(&src), "水印", &dir.join("x.pdf")).unwrap_err();
        assert!(err.to_string().contains("拉丁"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn form_fields_and_fill() {
        let dir = std::env::temp_dir().join(format!("cortex-pdf-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut doc = one_page_pdf("Form");
        let fields_id = doc.add_object(dictionary! {
            "FT" => "Tx",
            "T" => "姓名",
            "V" => "",
        });
        // Dummy appearance stream so /AP /N has a state key to enumerate.
        let ap_yes = doc.add_object(Stream::new(dictionary! {}, Vec::new()));
        let check_id = doc.add_object(dictionary! {
            "FT" => "Btn",
            "T" => "同意",
            "V" => "Off",
            "AP" => dictionary! {
                "N" => dictionary! { "Yes" => Object::Reference(ap_yes) },
            },
        });
        let acro_id = doc.add_object(dictionary! {
            "Fields" => vec![Object::Reference(fields_id), Object::Reference(check_id)],
        });
        if let Ok(c) = doc.catalog_mut() {
            c.set("AcroForm", Object::Reference(acro_id));
        }
        let src = write_tmp(&dir, "form.pdf", &mut doc);

        let fields = form_fields(Path::new(&src)).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "同意");
        assert_eq!(fields[0].kind, "checkbox");
        assert!(fields[0].options.contains(&"Yes".to_string()));
        assert_eq!(fields[1].name, "姓名");

        let dest = dir.join("filled.pdf");
        let n = fill_form(
            Path::new(&src),
            &[("姓名".to_string(), "张三".to_string()), ("同意".to_string(), "Yes".to_string())],
            &dest,
        )
        .unwrap();
        assert_eq!(n, 2);

        let filled = Document::load(&dest).unwrap();
        // T can be a Name (dictionary! macro) or a TextString (real-world
        // exporters) — match both, exactly like fill_form's own lookup.
        let f_id = filled
            .objects
            .iter()
            .find(|(_, o)| {
                o.as_dict().map(|d| {
                    d.get(b"T").map(|t| match t {
                        Object::String(b, _) => decode_pdf_string(b) == "姓名",
                        Object::Name(n) => String::from_utf8_lossy(n) == "姓名",
                        _ => false,
                    }).unwrap_or(false)
                }).unwrap_or(false)
            })
            .map(|(id, _)| *id)
            .unwrap();
        let fd = filled.get_object(f_id).unwrap().as_dict().unwrap();
        assert_eq!(decode_pdf_string(fd.get(b"V").unwrap().as_str().unwrap()), "张三");
        let acro = filled
            .get_object(
                filled
                    .catalog()
                    .unwrap()
                    .get(b"AcroForm")
                    .unwrap()
                    .as_reference()
                    .unwrap(),
            )
            .unwrap();
        // lopdf::Error isn't PartialEq, so compare through map+unwrap_or.
        let need = acro
            .as_dict()
            .unwrap()
            .get(b"NeedAppearances")
            .map(|v| *v == Object::Boolean(true))
            .unwrap_or(false);
        assert!(need, "fill_form must set NeedAppearances");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
