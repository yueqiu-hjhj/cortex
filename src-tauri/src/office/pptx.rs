//! PowerPoint (.pptx) creation — hand-rolled OOXML over the existing `zip`
//! crate (same approach as docx.rs). Slides are plain text boxes rather than
//! placeholders, which renders identically in PowerPoint / WPS / LibreOffice
//! without shipping a full placeholder-inheriting master. Material payloads
//! (quiz, cheatsheet, slideshow, …) map onto slides via `material_slides`.

use super::{write_zip, xml_escape};
use crate::error::{Error, Result};
use std::path::Path;

/// One content slide: a heading and a bullet list.
#[derive(Debug, Clone)]
pub struct DeckSlide {
    pub heading: String,
    pub bullets: Vec<String>,
}

const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const P_NS: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const OD_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

// 16:9 in EMU.
const SLIDE_W: i64 = 12_192_000;
const SLIDE_H: i64 = 6_858_000;
const MARGIN: i64 = 838_200;

const COLOR_INK: &str = "1F2430";
const COLOR_SUB: &str = "3B4252";
const COLOR_ACCENT: &str = "5E81AC";

fn sptree_open() -> String {
    r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#.into()
}

fn rpr(sz: u32, bold: bool, color: &str) -> String {
    let b = if bold { r#" b="1""# } else { "" };
    format!(
        r#"<a:rPr lang="zh-CN" altLang="en-US" sz="{sz}"{b} dirty="0"><a:solidFill><a:srgbClr val="{color}"/></a:solidFill></a:rPr>"#
    )
}

/// A plain text box with `paras` as its body paragraphs.
fn textbox(id: u32, name: &str, x: i64, y: i64, cx: i64, cy: i64, paras: &str) -> String {
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="{name}"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/></p:spPr><p:txBody><a:bodyPr wrap="square" rtlCol="0"><a:normAutofit/></a:bodyPr><a:lstStyle/>{paras}</p:txBody></p:sp>"#
    )
}

fn heading_par(text: &str) -> String {
    format!("<a:p><a:r>{}<a:t>{}</a:t></a:r></a:p>", rpr(3200, true, COLOR_INK), xml_escape(text))
}

fn bullet_par(text: &str) -> String {
    format!(
        r#"<a:p><a:pPr marL="342900" indent="-342900"><a:buFont typeface="Arial"/><a:buChar char="•"/></a:pPr><a:r>{}<a:t>{}</a:t></a:r></a:p>"#,
        rpr(2000, false, COLOR_SUB),
        xml_escape(text)
    )
}

fn empty_par() -> String {
    "<a:p><a:endParaRPr lang=\"zh-CN\" dirty=\"0\"/></a:p>".into()
}

const MASTER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle><a:lvl1pPr><a:defRPr sz="4400" b="1"/></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr marL="342900" indent="-342900"><a:buFont typeface="Arial"/><a:buChar char="•"/><a:defRPr sz="2400"/></a:lvl1pPr></p:bodyStyle><p:otherStyle><a:lvl1pPr><a:defRPr sz="1800"/></a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>"#;

const LAYOUT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="blank"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#;

const THEME_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Cortex"><a:themeElements><a:clrScheme name="Cortex"><a:dk1><a:srgbClr val="1F2430"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="3B4252"/></a:dk2><a:lt2><a:srgbClr val="ECEFF4"/></a:lt2><a:accent1><a:srgbClr val="5E81AC"/></a:accent1><a:accent2><a:srgbClr val="81A1C1"/></a:accent2><a:accent3><a:srgbClr val="88C0D0"/></a:accent3><a:accent4><a:srgbClr val="A3BE8C"/></a:accent4><a:accent5><a:srgbClr val="EBCB8B"/></a:accent5><a:accent6><a:srgbClr val="BF616A"/></a:accent6><a:hlink><a:srgbClr val="5E81AC"/></a:hlink><a:folHlink><a:srgbClr val="81A1C1"/></a:folHlink></a:clrScheme><a:fontScheme name="Cortex"><a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Cortex"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"><a:tint val="60000"/></a:schemeClr></a:solidFill><a:solidFill><a:schemeClr val="phClr"><a:tint val="80000"/></a:schemeClr></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="9525" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/><a:miter lim="800000"/></a:ln><a:ln w="19050" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/><a:miter lim="800000"/></a:ln><a:ln w="28575" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/><a:miter lim="800000"/></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"><a:tint val="60000"/></a:schemeClr></a:solidFill><a:solidFill><a:schemeClr val="phClr"><a:tint val="80000"/></a:schemeClr></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"#;

/// Build a .pptx deck (cover slide + `slides`) and write it to `path`.
pub fn build_deck(path: &Path, title: &str, subtitle: Option<&str>, slides: &[DeckSlide]) -> Result<()> {
    let n = slides.len();
    // Cover slide XML (index 0) is assembled here so the title text is embedded.
    let mut cover_paras = format!(
        r#"<a:p><a:pPr algn="ctr"/><a:r>{}<a:t>{}</a:t></a:r></a:p>"#,
        rpr(5400, true, COLOR_INK),
        xml_escape(title)
    );
    if let Some(sub) = subtitle.filter(|s| !s.trim().is_empty()) {
        cover_paras.push_str(&format!(
            r#"<a:p><a:pPr algn="ctr"/><a:r>{}<a:t>{}</a:t></a:r></a:p>"#,
            rpr(2400, false, COLOR_ACCENT),
            xml_escape(sub)
        ));
    }
    let cover = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="{A_NS}" xmlns:r="{R_NS}" xmlns:p="{P_NS}"><p:cSld><p:spTree>{}{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#,
        sptree_open(),
        textbox(2, "Cover", MARGIN, 2_400_000, SLIDE_W - 2 * MARGIN, 2_058_000, &cover_paras)
    );

    // slideN.xml for the content slides (slide2.xml onward — slide1 is the cover).
    let mut slide_xmls = vec![("ppt/slides/slide1.xml".to_string(), cover)];
    for (i, sl) in slides.iter().enumerate() {
        let mut paras = heading_par(&sl.heading);
        if sl.bullets.is_empty() {
            paras.push_str(&empty_par());
        }
        for b in &sl.bullets {
            paras.push_str(&bullet_par(b));
        }
        let heading = textbox(
            2,
            "Heading",
            MARGIN,
            685_800,
            SLIDE_W - 2 * MARGIN,
            1_143_000,
            &heading_par(&sl.heading),
        );
        let body = textbox(
            3,
            "Content",
            MARGIN,
            1_981_200,
            SLIDE_W - 2 * MARGIN,
            SLIDE_H - 1_981_200 - MARGIN,
            &paras,
        );
        let heading_body = format!("{heading}{body}", heading = heading, body = body);
        slide_xmls.push((
            format!("ppt/slides/slide{}.xml", i + 2),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="{A_NS}" xmlns:r="{R_NS}" xmlns:p="{P_NS}"><p:cSld><p:spTree>{}{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#,
                sptree_open(),
                heading_body,
            ),
        ));
    }

    let total = n + 1;
    let mut ct = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#,
    );
    for i in 1..=total {
        ct.push_str(&format!(
            r#"<Override PartName="/ppt/slides/slide{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#
        ));
    }
    ct.push_str("</Types>");

    let mut sld_ids = String::new();
    let mut pres_rels = format!(
        r#"<Relationship Id="rId1" Type="{OD_REL}/slideMaster" Target="slideMasters/slideMaster1.xml"/>"#
    );
    for (i, (name, _)) in slide_xmls.iter().enumerate() {
        sld_ids.push_str(&format!(
            r#"<p:sldId id="{}" r:id="rId{}"/>"#,
            256 + i as i64,
            i + 2
        ));
        pres_rels.push_str(&format!(
            r#"<Relationship Id="rId{}" Type="{OD_REL}/slide" Target="{}"/>"#,
            i + 2,
            name.trim_start_matches("ppt/")
        ));
    }

    let mut slide_rels: Vec<(String, String)> = Vec::new();
    for i in 1..=total {
        slide_rels.push((
            format!("ppt/slides/_rels/slide{i}.xml.rels"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{REL_NS}"><Relationship Id="rId1" Type="{OD_REL}/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#
            ),
        ));
    }

    let mut parts: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), ct.into_bytes()),
        (
            "_rels/.rels".into(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{REL_NS}"><Relationship Id="rId1" Type="{OD_REL}/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#
            )
            .into_bytes(),
        ),
        (
            "ppt/presentation.xml".into(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:presentation xmlns:a="{A_NS}" xmlns:r="{R_NS}" xmlns:p="{P_NS}"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{sld_ids}</p:sldIdLst><p:sldSz cx="{SLIDE_W}" cy="{SLIDE_H}"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
            )
            .into_bytes(),
        ),
        (
            "ppt/_rels/presentation.xml.rels".into(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{REL_NS}">{pres_rels}</Relationships>"#
            )
            .into_bytes(),
        ),
        ("ppt/slideMasters/slideMaster1.xml".into(), MASTER_XML.into()),
        (
            "ppt/slideMasters/_rels/slideMaster1.xml.rels".into(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{REL_NS}"><Relationship Id="rId1" Type="{OD_REL}/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="{OD_REL}/theme" Target="../theme/theme1.xml"/></Relationships>"#
            )
            .into_bytes(),
        ),
        ("ppt/slideLayouts/slideLayout1.xml".into(), LAYOUT_XML.into()),
        (
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels".into(),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{REL_NS}"><Relationship Id="rId1" Type="{OD_REL}/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#
            )
            .into_bytes(),
        ),
        ("ppt/theme/theme1.xml".into(), THEME_XML.into()),
    ];
    for (name, xml) in slide_xmls {
        parts.push((name, xml.into_bytes()));
    }
    for (name, xml) in slide_rels {
        parts.push((name, xml.into_bytes()));
    }
    write_zip(path, &parts)
}

// ------------------------------------------------- material → slides

fn s(v: Option<&serde_json::Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn arr(v: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    v.get(key)
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Item lists arrive either as a bare JSON array (quiz/flashcards) or wrapped
/// under "questions" — accept both.
fn items(v: &serde_json::Value) -> Vec<serde_json::Value> {
    if v.is_array() {
        v.as_array().cloned().unwrap_or_default()
    } else {
        arr(v, "questions")
    }
}

/// Keep slide headings scannable — a 200-char question would swallow the box.
fn head(text: &str) -> String {
    let t = text.trim();
    let mut out = String::new();
    for (n, c) in t.chars().enumerate() {
        if n >= 80 {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

/// Map a generated material payload onto deck slides. Returns content slides
/// only — the deck title becomes the cover. Kinds with no natural slide shape
/// are rejected so callers can offer docx/xlsx instead of silently flattening.
pub fn material_slides(kind: &str, payload: &serde_json::Value) -> Result<Vec<DeckSlide>> {
    let mut slides = Vec::new();
    match kind {
        "slideshow" => {
            for sl in arr(payload, "slides") {
                let mut bullets = arr(&sl, "bullets").iter().map(|b| s(Some(b))).collect::<Vec<_>>();
                let notes = s(sl.get("notes"));
                if !notes.is_empty() {
                    bullets.push(format!("备注：{notes}"));
                }
                slides.push(DeckSlide { heading: head(&s(sl.get("title"))), bullets });
            }
        }
        "cheatsheet" => {
            for sec in arr(payload, "sections") {
                let mut bullets = Vec::new();
                for it in arr(&sec, "items") {
                    let t = s(it.get("t"));
                    let d = s(it.get("d"));
                    bullets.push(if d.is_empty() { t } else { format!("{t} — {d}") });
                }
                if bullets.is_empty() {
                    bullets = arr(&sec, "points").iter().map(|p| s(Some(p))).collect();
                }
                slides.push(DeckSlide { heading: head(&s(sec.get("title"))), bullets });
            }
        }
        "quiz" => quiz_slides(payload, &mut slides),
        "flashcards" => {
            for (i, it) in items(payload).iter().enumerate() {
                slides.push(DeckSlide {
                    heading: head(&format!("卡 {}：{}", i + 1, s(it.get("q")))),
                    bullets: vec![s(it.get("a"))],
                });
            }
        }
        "guide" => {
            let overview = s(payload.get("overview"));
            if !overview.is_empty() {
                slides.push(DeckSlide { heading: "总览".into(), bullets: vec![overview] });
            }
            for sec in arr(payload, "sections") {
                slides.push(DeckSlide {
                    heading: head(&s(sec.get("heading"))),
                    bullets: arr(&sec, "points").iter().map(|p| s(Some(p))).collect(),
                });
            }
            let pitfalls = arr(payload, "pitfalls").iter().map(|p| s(Some(p))).collect::<Vec<_>>();
            if !pitfalls.is_empty() {
                slides.push(DeckSlide { heading: "易错点".into(), bullets: pitfalls });
            }
            let st = arr(payload, "selftest");
            if !st.is_empty() {
                slides.push(DeckSlide {
                    heading: "自测".into(),
                    bullets: st
                        .iter()
                        .enumerate()
                        .map(|(i, it)| format!("{}. {} — {}", i + 1, s(it.get("q")), s(it.get("a"))))
                        .collect(),
                });
            }
        }
        "matrix" => {
            let cols = arr(payload, "columns").iter().map(|c| s(Some(c))).collect::<Vec<_>>();
            slides.push(DeckSlide { heading: "对比维度".into(), bullets: cols.clone() });
            for r in arr(payload, "rows") {
                let label = s(r.get("label"));
                let cells = arr(&r, "cells").iter().map(|c| s(Some(c))).collect::<Vec<_>>();
                let bullets = cols
                    .iter()
                    .zip(cells.iter())
                    .map(|(c, v)| format!("{c}：{v}"))
                    .collect();
                slides.push(DeckSlide { heading: head(&label), bullets });
            }
        }
        "mindmap" => {
            slides.push(DeckSlide {
                heading: "中心主题".into(),
                bullets: vec![s(payload.get("central"))],
            });
            for br in arr(payload, "branches") {
                let mut bullets = Vec::new();
                for ch in arr(&br, "children") {
                    let label = s(ch.get("label"));
                    let subs = arr(&ch, "children").iter().map(|c| s(Some(c))).collect::<Vec<_>>();
                    bullets.push(if subs.is_empty() {
                        label
                    } else {
                        format!("{label}（{}）", subs.join("、"))
                    });
                }
                slides.push(DeckSlide { heading: head(&s(br.get("label"))), bullets });
            }
        }
        "infographic" => {
            for sec in arr(payload, "sections") {
                let mut bullets = arr(&sec, "points").iter().map(|p| s(Some(p))).collect::<Vec<_>>();
                if let Some(stat) = sec.get("stat") {
                    let v = s(stat.get("value"));
                    let l = s(stat.get("label"));
                    if !v.is_empty() {
                        bullets.push(format!("关键数字：{v} {l}"));
                    }
                }
                let emoji = s(sec.get("emoji"));
                let heading = s(sec.get("heading"));
                slides.push(DeckSlide { heading: head(format!("{emoji} {heading}").trim()), bullets });
            }
        }
        "audio" => {
            for seg in arr(payload, "segments") {
                slides.push(DeckSlide {
                    heading: head(&s(seg.get("speaker"))),
                    bullets: vec![s(seg.get("text"))],
                });
            }
        }
        other => {
            return Err(Error::Unsupported(format!(
                "{other} 没有幻灯片形态，请导出为 docx 或 xlsx"
            )));
        }
    }
    Ok(slides)
}

fn quiz_slides(payload: &serde_json::Value, slides: &mut Vec<DeckSlide>) {
    let letters = ["A", "B", "C", "D", "E", "F"];
    for (i, it) in items(payload).iter().enumerate() {
        let mut bullets = arr(it, "options")
            .iter()
            .enumerate()
            .map(|(j, o)| format!("{}. {}", letters.get(j).unwrap_or(&"?"), s(Some(o))))
            .collect::<Vec<_>>();
        let ans = it
            .get("answer")
            .and_then(|a| a.as_u64())
            .map(|a| letters.get(a as usize).map(|x| x.to_string()).unwrap_or_else(|| a.to_string()))
            .unwrap_or_else(|| s(it.get("answer")));
        let explain = s(it.get("explain"));
        bullets.push(format!(
            "答案：{}{}",
            ans,
            if explain.is_empty() { String::new() } else { format!(" — {explain}") }
        ));
        slides.push(DeckSlide { heading: head(&format!("{}. {}", i + 1, s(it.get("q")))), bullets });
    }
}

/// One-call export: material payload → deck file.
pub fn save_deck(
    path: &Path,
    kind: &str,
    title: &str,
    subtitle: Option<&str>,
    payload: &serde_json::Value,
) -> Result<()> {
    let mut slides = material_slides(kind, payload)?;
    // A 200-slide quiz deck is unusable and takes forever to open — cap and
    // tell the reader where the rest went.
    const MAX_SLIDES: usize = 60;
    let mut truncated = false;
    if slides.len() > MAX_SLIDES {
        slides.truncate(MAX_SLIDES);
        truncated = true;
    }
    if truncated {
        slides.push(DeckSlide {
            heading: "（已截断）".into(),
            bullets: vec!["内容超出 60 页上限，完整版请导出为 docx 或 xlsx".into()],
        });
    }
    build_deck(path, title, subtitle, &slides)
}

// ------------------------------------------ editing an existing deck

/// One find → replace pair applied to every slide of a deck.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TextEdit {
    pub find: String,
    #[serde(default)]
    pub replace: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PptxEditOutcome {
    pub output: String,
    /// Slides that changed, by their 1-based number.
    pub slides: Vec<usize>,
    /// Total number of replaced occurrences.
    pub replacements: usize,
    /// Paragraphs that had to be collapsed into one run because the phrase was
    /// split across runs. Reported, because it costs the inner formatting.
    pub merged: usize,
    pub skipped: Vec<String>,
}

/// Replace text in an existing deck, writing the result to `output`.
///
/// Two passes, in this order, because a deck stores text per RUN and an editor
/// splits runs at every formatting or spell-check boundary:
///
/// 1. **Inside a run** — the phrase sits wholly in one `<a:t>`. Replacing it
///    keeps every other run's formatting untouched, which is the outcome a
///    reader expects.
/// 2. **Across runs of one paragraph** — the phrase exists only in the
///    concatenation (`光合` + `作用`). The paragraph is collapsed into a single
///    run carrying the first run's properties. This is a real edit of the
///    layout, so it is counted separately instead of hidden.
///
/// Nothing else in the file is touched: the parts are copied byte for byte and
/// only `ppt/slides/slideN.xml` is rewritten.
pub fn replace_text(
    input: &Path,
    output: &Path,
    edits: &[TextEdit],
) -> Result<PptxEditOutcome> {
    if input == output {
        return Err(Error::Other("输出文件需与原文件不同（改稿在副本上做）".into()));
    }
    if edits.is_empty() {
        return Err(Error::Other("没有要替换的文字".into()));
    }
    for e in edits {
        if e.find.trim().is_empty() {
            return Err(Error::Other("「查找」不能为空".into()));
        }
    }
    let mut parts = super::read_zip_parts(input)?;
    let mut slides = Vec::new();
    let mut replacements = 0usize;
    let mut merged = 0usize;

    let slide_idxs: Vec<(usize, usize)> = parts
        .iter()
        .enumerate()
        .filter_map(|(i, (n, _))| slide_number(n).map(|num| (i, num)))
        .collect();
    for (idx, number) in slide_idxs {
        let name = parts[idx].0.clone();
        let mut xml = String::from_utf8(parts[idx].1.clone())
            .map_err(|_| Error::Other(format!("{name} 不是 UTF-8")))?;
        let mut touched = false;
        for e in edits {
            let (next, hits, merges) = replace_in_slide(&xml, &e.find, &e.replace);
            if hits + merges > 0 {
                xml = next;
                replacements += hits;
                merged += merges;
                touched = true;
            }
        }
        if touched {
            parts[idx].1 = xml.into_bytes();
            slides.push(number);
        }
    }

    if slides.is_empty() {
        let mut missing: Vec<String> = edits
            .iter()
            .map(|e| format!("没有找到「{}」", e.find))
            .collect();
        missing.retain(|_| true);
        super::write_zip(output, &parts)?;
        return Ok(PptxEditOutcome {
            output: output.display().to_string(),
            slides: Vec::new(),
            replacements: 0,
            merged: 0,
            skipped: missing,
        });
    }
    super::write_zip(output, &parts)?;
    Ok(PptxEditOutcome {
        output: output.display().to_string(),
        slides,
        replacements,
        merged,
        skipped: Vec::new(),
    })
}

/// Read-only view of a deck: one entry per slide, one string per paragraph.
/// The caller needs it to know what it is asking to replace — a find/replace
/// box with no preview is a request to edit blind.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SlideText {
    pub index: usize,
    pub paragraphs: Vec<String>,
}

pub fn slide_texts(path: &Path) -> Result<Vec<SlideText>> {
    let parts = super::read_zip_parts(path)?;
    let mut slides: Vec<(usize, SlideText)> = Vec::new();
    for (name, bytes) in &parts {
        let Some(index) = slide_number(name) else { continue };
        let xml = std::str::from_utf8(bytes)
            .map_err(|_| Error::Other(format!("{name} 不是 UTF-8")))?;
        let mut paragraphs = Vec::new();
        let mut cursor = 0;
        while let Some(open) = xml[cursor..].find("<a:p>").map(|i| i + cursor) {
            let Some(close_off) = xml[open..].find("</a:p>") else { break };
            let close = open + close_off;
            let body = &xml[open + "<a:p>".len()..close];
            let text = paragraph_text(body);
            if !text.trim().is_empty() {
                paragraphs.push(text);
            }
            cursor = close + "</a:p>".len();
        }
        slides.push((index, SlideText { index, paragraphs }));
    }
    slides.sort_by_key(|(i, _)| *i);
    Ok(slides.into_iter().map(|(_, s)| s).collect())
}

/// `ppt/slides/slide12.xml` → 12. Anything else is not a slide.
fn slide_number(name: &str) -> Option<usize> {
    let rest = name.strip_prefix("ppt/slides/slide")?;
    let num = rest.strip_suffix(".xml")?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    num.parse().ok()
}

/// Replace in one slide part. Returns (new xml, in-run hits, merged paragraphs).
fn replace_in_slide(xml: &str, find: &str, replace: &str) -> (String, usize, usize) {
    // Pass 1: straight substitution inside every <a:t> element.
    let mut out = String::with_capacity(xml.len());
    let mut hits = 0usize;
    let mut cursor = 0usize;
    while let Some(t) = find_tag(xml, "a:t", cursor) {
        out.push_str(&xml[cursor..t.content_start]);
        let text = &xml[t.content_start..t.content_end];
        let decoded = super::xml_unescape(text);
        if decoded.contains(find) {
            hits += decoded.matches(find).count();
            out.push_str(&xml_escape(&decoded.replace(find, replace)));
        } else {
            out.push_str(text);
        }
        out.push_str(&xml[t.content_end..t.end]);
        cursor = t.end;
    }
    out.push_str(&xml[cursor..]);

    // Pass 2: the phrase only exists in a paragraph's concatenated text.
    let (out, merged) = collapse_split_paragraphs(&out, find, replace);
    (out, hits, merged)
}

/// One `<a:t>…</a:t>` element (they are never nested).
struct TSpan {
    content_start: usize,
    content_end: usize,
    end: usize,
}

fn find_tag(s: &str, name: &str, from: usize) -> Option<TSpan> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let at = from + s[from..].find(&open)?;
    let content_start = at + open.len();
    let content_end = content_start + s[content_start..].find(&close)?;
    Some(TSpan { content_start, content_end, end: content_end + close.len() })
}

/// Collapse paragraphs whose runs together contain `find`.
fn collapse_split_paragraphs(xml: &str, find: &str, replace: &str) -> (String, usize) {
    let mut out = String::with_capacity(xml.len());
    let mut merged = 0usize;
    let mut cursor = 0usize;
    loop {
        let Some(open) = xml[cursor..].find("<a:p>").map(|i| i + cursor) else {
            out.push_str(&xml[cursor..]);
            break;
        };
        let Some(close_off) = xml[open..].find("</a:p>") else {
            out.push_str(&xml[cursor..]);
            break;
        };
        let close = open + close_off;
        out.push_str(&xml[cursor..open]);

        let body = &xml[open + "<a:p>".len()..close];
        let joined = paragraph_text(body);
        if joined.contains(find) {
            let new_text = joined.replace(find, replace);
            // Keep the first run's formatting: it is the one a reader associates
            // with this line, and it is what the merged run will render with.
            let first_run = body.find("<a:r>");
            let rpr = first_run.and_then(|r| {
                let rest = &body[r..];
                let start = rest.find("<a:rPr")?;
                let tail = &rest[start..];
                if let Some(self_close) = tail.find("/>") {
                    let gt = tail.find('>')?;
                    if self_close + 1 == gt {
                        return Some(tail[..=gt].to_string());
                    }
                }
                let end = tail.find("</a:rPr>")? + "</a:rPr>".len();
                Some(tail[..end].to_string())
            });
            let props = rpr.unwrap_or_default();
            let run = format!("<a:r>{props}<a:t>{}</a:t></a:r>", xml_escape(&new_text));
            out.push_str("<a:p>");
            match (body.find("<a:r>"), body.rfind("</a:r>")) {
                (Some(first), Some(last)) if first <= last => {
                    out.push_str(&body[..first]);
                    // Anything after the last run (e.g. `</a:p>`-level end
                    // markers) is preserved; the runs between are replaced.
                    out.push_str(&run);
                    out.push_str(&body[last + "</a:r>".len()..]);
                }
                _ => out.push_str(&run),
            }
            out.push_str("</a:p>");
            merged += 1;
        } else {
            out.push_str(&xml[open..close + "</a:p>".len()]);
        }
        cursor = close + "</a:p>".len();
    }
    (out, merged)
}

/// Visible text of a paragraph body: every `<a:t>` concatenated, decoded.
fn paragraph_text(body: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0usize;
    while let Some(t) = find_tag(body, "a:t", cursor) {
        out.push_str(&super::xml_unescape(&body[t.content_start..t.content_end]));
        cursor = t.end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;


    // ---- editing an existing deck ----

    /// A minimal deck: slide 1 with three separate runs, and a notes part that
    /// must never be touched (it is not a slide).
    fn deck_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("cortex-pptx-edit-{tag}-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("deck.pptx");
        let out = dir.join("改后.pptx");
        let slide = concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?><p:sld><p:cSld><p:spTree><p:sp><p:txBody>"#,
            r#"<a:p><a:r><a:rPr sz="3200" b="1"/><a:t>机器学习</a:t></a:r>"#,
            r#"<a:r><a:rPr sz="1800"/><a:t>复习大纲</a:t></a:r></a:p>"#,
            r#"<a:p><a:r><a:rPr sz="1400" i="1"/><a:t>重点：反向传播</a:t></a:r></a:p>"#,
            r#"</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
        );
        let notes = r#"<?xml version="1.0"?><p:notes><a:t>机器学习</a:t></p:notes>"#;
        super::super::write_zip(
            &src,
            &[
                ("ppt/slides/slide1.xml".into(), slide.as_bytes().to_vec()),
                ("ppt/notesSlides/notesSlide1.xml".into(), notes.as_bytes().to_vec()),
                ("docProps/app.xml".into(), b"<Properties/>".to_vec()),
            ],
        )
        .unwrap();
        (dir, src, out)
    }

    fn part(path: &std::path::Path, name: &str) -> String {
        let parts = super::super::read_zip_parts(path).unwrap();
        String::from_utf8(parts.iter().find(|(n, _)| n == name).unwrap().1.clone()).unwrap()
    }

    #[test]
    fn slide_texts_shows_what_a_replacement_would_match() {
        let (dir, src, _) = deck_fixture("texts");
        let texts = slide_texts(&src).unwrap();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].index, 1);
        assert_eq!(
            texts[0].paragraphs,
            vec!["机器学习复习大纲".to_string(), "重点：反向传播".to_string()],
            "runs of one paragraph are one line"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_inside_a_run_is_replaced_and_the_other_runs_survive() {
        let (dir, src, out) = deck_fixture("run");
        let before = part(&src, "ppt/slides/slide1.xml");
        let r = replace_text(&src, &out, &[TextEdit { find: "机器学习".into(), replace: "深度学习".into() }]).unwrap();
        assert_eq!(r.slides, vec![1]);
        assert_eq!(r.replacements, 1);
        assert_eq!(r.merged, 0, "one run already held the whole phrase");
        assert!(r.skipped.is_empty(), "{:?}", r.skipped);
        let xml = part(&out, "ppt/slides/slide1.xml");
        assert!(xml.contains("深度学习"), "{xml}");
        assert!(!xml.contains("机器学习"), "{xml}");
        // The sibling run keeps its own properties: this was an edit, not a rebuild.
        assert!(xml.contains(r#"<a:rPr sz="1800"/><a:t>复习大纲</a:t>"#), "{xml}");
        assert_eq!(part(&src, "ppt/slides/slide1.xml"), before, "the input is untouched");
        // Non-slide parts are copied, not rewritten.
        assert!(part(&out, "ppt/notesSlides/notesSlide1.xml").contains("机器学习"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_phrase_split_across_runs_is_merged_and_counted() {
        let (dir, src, out) = deck_fixture("merge");
        // Split the heading into two runs the way an editor does after a
        // spelling fix: the phrase no longer exists in any single <a:t>.
        let name = "ppt/slides/slide1.xml";
        let mut parts = super::super::read_zip_parts(&src).unwrap();
        let idx = parts.iter().position(|(n, _)| n == name).unwrap();
        let xml = String::from_utf8(parts[idx].1.clone())
            .unwrap()
            .replace(
                r#"<a:r><a:rPr sz="3200" b="1"/><a:t>机器学习</a:t></a:r>"#,
                r#"<a:r><a:rPr sz="3200" b="1"/><a:t>机器</a:t></a:r><a:r><a:t>学习</a:t></a:r>"#,
            );
        parts[idx].1 = xml.into_bytes();
        super::super::write_zip(&src, &parts).unwrap();

        let r = replace_text(&src, &out, &[TextEdit { find: "机器学习".into(), replace: "机器学习基础".into() }]).unwrap();
        assert_eq!(r.replacements, 0, "no single run held the phrase");
        assert_eq!(r.merged, 1, "the paragraph had to be collapsed");
        let xml = part(&out, name);
        assert!(xml.contains("机器学习基础"), "{xml}");
        assert!(xml.contains(r#"<a:rPr sz="3200" b="1"/>"#), "the first run's formatting is kept: {xml}");
        assert!(!xml.contains("<a:t>机器</a:t>"), "the split runs are gone: {xml}");
        // The second paragraph was not touched at all.
        assert!(xml.contains("重点：反向传播"), "{xml}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_phrase_that_appears_nowhere_is_reported_not_silently_dropped() {
        let (dir, src, out) = deck_fixture("missing");
        let r = replace_text(&src, &out, &[TextEdit { find: "不存在的句子".into(), replace: "x".into() }]).unwrap();
        assert!(r.slides.is_empty());
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].contains("不存在的句子"), "{:?}", r.skipped);
        assert!(out.exists(), "the copy is still written, so the caller can hand it on");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacing_into_the_input_is_refused() {
        let (dir, src, _) = deck_fixture("same");
        let err = replace_text(&src, &src, &[TextEdit { find: "机器".into(), replace: "x".into() }])
            .unwrap_err()
            .to_string();
        assert!(err.contains("副本"), "{err}");
        let err = replace_text(&src, &src.with_file_name("o.pptx"), &[TextEdit { find: " ".into(), replace: "" .into() }])
            .unwrap_err()
            .to_string();
        assert!(err.contains("不能为空"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deck_roundtrips_and_text_is_escaped() {
        let dir = std::env::temp_dir().join(format!("cortex-pptx-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("deck.pptx");
        build_deck(
            &p,
            "数据结构<T1>",
            Some("期末复习"),
            &[
                DeckSlide { heading: "链表 vs 数组".into(), bullets: vec!["O(1) 插入".into(), "随机访问 O(n)".into()] },
                DeckSlide { heading: "空slide".into(), bullets: vec![] },
            ],
        )
        .unwrap();
        let parts = super::super::read_zip_parts(&p).unwrap();
        let names: Vec<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
        for required in [
            "[Content_Types].xml",
            "_rels/.rels",
            "ppt/presentation.xml",
            "ppt/_rels/presentation.xml.rels",
            "ppt/slideMasters/slideMaster1.xml",
            "ppt/slideMasters/_rels/slideMaster1.xml.rels",
            "ppt/slideLayouts/slideLayout1.xml",
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            "ppt/theme/theme1.xml",
            "ppt/slides/slide1.xml",
            "ppt/slides/slide2.xml",
            "ppt/slides/_rels/slide2.xml.rels",
        ] {
            assert!(names.contains(&required), "missing part {required}");
        }
        let slide2 = parts
            .iter()
            .find(|(n, _)| n == "ppt/slides/slide2.xml")
            .map(|(_, b)| String::from_utf8_lossy(b).to_string())
            .unwrap();
        assert!(slide2.contains("链表 vs 数组"));
        assert!(slide2.contains("O(1) 插入"));
        // The cover embeds the escaped title.
        let cover = parts
            .iter()
            .find(|(n, _)| n == "ppt/slides/slide1.xml")
            .map(|(_, b)| String::from_utf8_lossy(b).to_string())
            .unwrap();
        assert!(cover.contains("数据结构&lt;T1&gt;"), "cover: {cover}");
        // Deck text is natively extractable by the existing ingest path.
        let text = crate::ingest::ooxml_to_text(p.to_str().unwrap()).unwrap();
        assert!(text.contains("链表 vs 数组"), "ooxml text: {text}");
        assert!(text.contains("期末复习"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn material_slides_maps_payloads() {
        let payload = serde_json::json!([
            {"q": "栈的性质?", "options": ["LIFO", "FIFO"], "answer": 0, "explain": "先进后出"},
            {"q": "队列的性质?", "options": ["LIFO", "FIFO"], "answer": 1}
        ]);
        let slides = material_slides("quiz", &payload).unwrap();
        assert_eq!(slides.len(), 2);
        assert_eq!(slides[0].heading, "1. 栈的性质?");
        assert!(slides[0].bullets[0].starts_with("A. LIFO"));
        assert!(slides[0].bullets.last().unwrap().contains("答案：A"));

        let ss = serde_json::json!({"slides": [
            {"title": "引入", "bullets": ["背景", "动机"], "notes": "强调例子"}
        ]});
        let slides = material_slides("slideshow", &ss).unwrap();
        assert_eq!(slides.len(), 1);
        assert_eq!(slides[0].bullets, vec!["背景", "动机", "备注：强调例子"]);

        let err = material_slides("audio_nope", &serde_json::json!({})).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn save_deck_caps_runaway_decks() {
        let dir = std::env::temp_dir().join(format!("cortex-pptx-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("big.pptx");
        let payload = serde_json::Value::Array(
            (0..100)
                .map(|i| serde_json::json!({"q": format!("问题{i}"), "options": [], "answer": 0}))
                .collect(),
        );
        save_deck(&p, "quiz", "巨型题库", None, &payload).unwrap();
        let parts = super::super::read_zip_parts(&p).unwrap();
        // 60 content slides + truncation notice + cover = 62.
        let count = parts.iter().filter(|(n, _)| n.starts_with("ppt/slides/slide") && n.ends_with(".xml")).count();
        assert_eq!(count, 62);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
