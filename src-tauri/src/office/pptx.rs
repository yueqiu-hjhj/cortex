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

#[cfg(test)]
mod tests {
    use super::*;

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
