//! xlsx output — real workbooks (rust_xlsxwriter) with live formulas and
//! charts. Study materials map onto sheets via `material_sheets`: a quiz gets
//! an answer-distribution sheet driven by COUNTIF + a column chart, sectioned
//! material gets per-section COUNTIF counts + a bar chart. The workbook must
//! recompute on open — we never bake results in.

use crate::error::{Error, Result};
use rust_xlsxwriter::{Chart, Color, Format, Formula, Workbook};
use serde_json::Value;
use std::path::Path;

/// One cell of a sheet body, independent of xlsxwriter types.
#[derive(Debug, Clone)]
pub enum Cell {
    Text(String),
    Num(f64),
    /// Formula WITHOUT the leading `=` (the writer adds it).
    Formula(String),
}

#[derive(Debug, Clone)]
pub struct ChartSpec {
    /// column | bar | line | pie
    pub kind: String,
    pub title: String,
    /// Full range references, e.g. `'答案分布'!$A$4:$A$8`.
    pub categories: String,
    pub values: String,
}

#[derive(Debug, Clone, Default)]
pub struct SheetSpec {
    pub name: String,
    pub title: Option<String>,
    pub headers: Vec<String>,
    /// Column width in Excel character units; 0 = let Excel decide.
    pub widths: Vec<f64>,
    pub rows: Vec<Vec<Cell>>,
    pub chart: Option<ChartSpec>,
}

// ------------------------------------------------------------ workbook

/// Excel sheet names: max 31 chars, none of `[]:*?/\`, not empty. Invalid
/// characters are replaced with `_` and the name truncated — collisions after
/// truncation get a numeric suffix.
fn sanitize_sheet_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => '_',
            c => c,
        })
        .take(31)
        .collect();
    let name = if cleaned.trim().is_empty() { "Sheet".to_string() } else { cleaned.trim().to_string() };
    name
}

fn dedupe_names(names: &[String]) -> Vec<String> {
    let mut used: Vec<String> = Vec::new();
    for n in names {
        let mut candidate = n.clone();
        let mut i = 2;
        while used.iter().any(|u| u.eq_ignore_ascii_case(&candidate)) {
            // Truncate by chars, not bytes — a byte slice could split a
            // multibyte character and panic on String::from.
            let suffix = i.to_string();
            let mut chars: Vec<char> = n.chars().collect();
            chars.truncate(31 - suffix.chars().count());
            candidate = format!("{}{}", chars.into_iter().collect::<String>(), suffix);
            i += 1;
        }
        used.push(candidate);
    }
    used
}

/// A1-style column letters (0-based: 0→A, 26→AA, …).
fn col_letters(mut col: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Absolute single-column range over the sheet's data rows, for formulas and
/// charts. `row0` is the 0-based index of the first data row.
pub fn col_range(sheet: &str, col: usize, row0: usize, row_to0: usize) -> String {
    let l = col_letters(col);
    format!("'{}'!${}${}:${}${}", sheet, l, row0 + 1, l, row_to0 + 1)
}

/// Write the whole workbook to `path`.
pub fn build_workbook(path: &Path, sheets: &[SheetSpec]) -> Result<()> {
    if sheets.is_empty() {
        return Err(Error::Other("工作簿至少需要一张工作表".into()));
    }
    let names = dedupe_names(&sheets.iter().map(|s| sanitize_sheet_name(&s.name)).collect::<Vec<_>>());

    let title_fmt = Format::new().set_bold().set_font_size(14);
    let head_fmt = Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x5E81AC));

    let mut wb = Workbook::new();
    for (spec, name) in sheets.iter().zip(names.iter()) {
        let ws = wb.add_worksheet();
        ws.set_name(name.as_str())?;

        let mut r: u32 = 0;
        if let Some(t) = spec.title.as_deref().filter(|t| !t.trim().is_empty()) {
            ws.write_with_format(r, 0, t, &title_fmt)?;
            r += 2; // title + spacer row
        }
        let header_row = r;
        for (c, h) in spec.headers.iter().enumerate() {
            ws.write_with_format(header_row, c as u16, h, &head_fmt)?;
        }
        let data0 = header_row + 1;

        for (i, row) in spec.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let (rr, cc) = (data0 + i as u32, c as u16);
                match cell {
                    Cell::Text(t) => {
                        ws.write_string(rr, cc, t)?;
                    }
                    Cell::Num(v) => {
                        ws.write_number(rr, cc, *v)?;
                    }
                    Cell::Formula(f) => {
                        ws.write_formula(rr, cc, Formula::new(format!("={f}")))?;
                    }
                }
            }
        }

        ws.autofit();
        // Manual widths win over autofit's estimate, so they come after.
        for (c, w) in spec.widths.iter().enumerate() {
            if *w > 0.0 {
                ws.set_column_width(c as u16, *w)?;
            }
        }

        if let Some(ch) = &spec.chart {
            let mut chart = match ch.kind.as_str() {
                "bar" => Chart::new_bar(),
                "pie" => Chart::new_pie(),
                "line" => Chart::new_line(),
                _ => Chart::new_column(),
            };
            chart.title().set_name(ch.title.as_str());
            let series = chart.add_series();
            series.set_categories(ch.categories.as_str());
            series.set_values(ch.values.as_str());
            // Anchor right of the table, below the title row.
            ws.insert_chart(data0, (spec.headers.len() + 1) as u16, &chart)?;
        }
    }
    wb.save(path)?;
    Ok(())
}

// ------------------------------------------------- material → sheets

fn s(v: Option<&Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn arr(v: &Value, key: &str) -> Vec<Value> {
    v.get(key).and_then(|x| x.as_array()).cloned().unwrap_or_default()
}

/// Item lists arrive either as a bare JSON array (quiz/flashcards) or wrapped
/// under "questions" — accept both, same as docx/pptx.
fn items(v: &Value) -> Vec<Value> {
    if v.is_array() {
        v.as_array().cloned().unwrap_or_default()
    } else {
        arr(v, "questions")
    }
}

const LETTERS: [&str; 6] = ["A", "B", "C", "D", "E", "F"];

fn answer_text(it: &Value) -> String {
    it.get("answer")
        .and_then(|a| a.as_u64())
        .map(|a| LETTERS.get(a as usize).map(|x| x.to_string()).unwrap_or_else(|| a.to_string()))
        .unwrap_or_else(|| s(it.get("answer")))
}

#[allow(dead_code)]
fn quiz_letter(it: &Value) -> Option<String> {
    let a = answer_text(it);
    if LETTERS.contains(&a.as_str()) {
        Some(a)
    } else {
        None
    }
}

/// Widen-ish defaults: narrow index column, roomy content columns.
fn widths(first_wide: usize, rest: usize, cols: usize) -> Vec<f64> {
    let mut v = vec![rest as f64; cols];
    if cols > 0 {
        v[0] = first_wide as f64;
    }
    v
}

/// Map a generated material payload onto workbook sheets. Kinds without a
/// natural tabular shape are rejected so callers can offer docx/pptx instead
/// of flattening.
pub fn material_sheets(kind: &str, title: &str, meta: Option<&str>, payload: &Value) -> Result<Vec<SheetSpec>> {
    let subtitle = meta.filter(|m| !m.trim().is_empty()).map(|m| m.to_string());
    let sheets = match kind {
        "quiz" => {
            let its = items(payload);
            let mut rows = Vec::new();
            for (i, it) in its.iter().enumerate() {
                let options = arr(it, "options")
                    .iter()
                    .enumerate()
                    .map(|(j, o)| format!("{}. {}", LETTERS.get(j).unwrap_or(&"?"), s(Some(o))))
                    .collect::<Vec<_>>()
                    .join("\n");
                rows.push(vec![
                    Cell::Num((i + 1) as f64),
                    Cell::Text(s(it.get("q"))),
                    Cell::Text(options),
                    Cell::Text(answer_text(it)),
                    Cell::Text(s(it.get("explain"))),
                ]);
            }
            let bank = SheetSpec {
                name: "题库".into(),
                title: Some(merge_title(title, subtitle.as_deref())),
                headers: vec!["#".into(), "题目".into(), "选项".into(), "答案".into(), "解析".into()],
                widths: vec![6.0, 46.0, 34.0, 8.0, 40.0],
                rows,
                chart: None,
            };
            // 答案分布: one COUNTIF per letter over the bank's 答案 column,
            // only for letters that can actually occur.
            let n = its.len();
            let max_opts = its.iter().map(|it| arr(it, "options").len()).max().unwrap_or(0);
            // 题库 always has a title row: header lands on Excel row 3, data
            // on rows 4.. — 0-based first data row is 3.
            let ans_col = col_range("题库", 3, 3, 2 + n);
            let used: Vec<&str> = LETTERS.iter().copied().take(max_opts.max(1)).collect();
            let dist_rows = used
                .iter()
                .map(|l| {
                    vec![
                        Cell::Text(l.to_string()),
                        Cell::Formula(format!("COUNTIF({ans_col},\"{l}\")")),
                    ]
                })
                .collect();
            let dist = SheetSpec {
                name: "答案分布".into(),
                title: Some("答案分布".into()),
                headers: vec!["选项".into(), "题数".into()],
                widths: vec![10.0, 12.0],
                rows: dist_rows,
                chart: Some(ChartSpec {
                    kind: "column".into(),
                    title: "答案分布".into(),
                    categories: col_range("答案分布", 0, 3, 2 + used.len()),
                    values: col_range("答案分布", 1, 3, 2 + used.len()),
                }),
            };
            vec![bank, dist]
        }
        "flashcards" => {
            let its = items(payload);
            let rows = its
                .iter()
                .enumerate()
                .map(|(i, it)| {
                    vec![
                        Cell::Num((i + 1) as f64),
                        Cell::Text(s(it.get("q"))),
                        Cell::Text(s(it.get("a"))),
                    ]
                })
                .collect();
            vec![SheetSpec {
                name: "卡片".into(),
                title: Some(merge_title(title, subtitle.as_deref())),
                headers: vec!["#".into(), "正面".into(), "背面".into()],
                widths: vec![6.0, 50.0, 50.0],
                rows,
                chart: None,
            }]
        }
        "cheatsheet" => sectioned_sheets(
            title,
            subtitle.as_deref(),
            "速查表",
            arr(payload, "sections")
                .iter()
                .map(|sec| {
                    let name = s(sec.get("title"));
                    let mut lines: Vec<String> = arr(sec, "items")
                        .iter()
                        .map(|it| {
                            let t = s(it.get("t"));
                            let d = s(it.get("d"));
                            if d.is_empty() { t } else { format!("{t} — {d}") }
                        })
                        .collect();
                    if lines.is_empty() {
                        lines = arr(sec, "points").iter().map(|p| s(Some(p))).collect();
                    }
                    (name, lines)
                })
                .collect(),
        ),
        "guide" => {
            let mut groups: Vec<(String, Vec<String>)> = Vec::new();
            let overview = s(payload.get("overview"));
            if !overview.is_empty() {
                groups.push(("总览".into(), vec![overview]));
            }
            for sec in arr(payload, "sections") {
                groups.push((s(sec.get("heading")), arr(&sec, "points").iter().map(|p| s(Some(p))).collect()));
            }
            let pitfalls = arr(payload, "pitfalls").iter().map(|p| s(Some(p))).collect::<Vec<_>>();
            if !pitfalls.is_empty() {
                groups.push(("易错点".into(), pitfalls));
            }
            let st: Vec<String> = arr(payload, "selftest")
                .iter()
                .enumerate()
                .map(|(i, it)| format!("{}. {} — {}", i + 1, s(it.get("q")), s(it.get("a"))))
                .collect();
            if !st.is_empty() {
                groups.push(("自测".into(), st));
            }
            sectioned_sheets(title, subtitle.as_deref(), "学习指南", groups)
        }
        "infographic" => {
            let mut groups: Vec<(String, Vec<String>)> = Vec::new();
            for sec in arr(payload, "sections") {
                let emoji = s(sec.get("emoji"));
                let heading = s(sec.get("heading"));
                let name = format!("{} {}", emoji, heading).trim().to_string();
                let mut lines = arr(&sec, "points").iter().map(|p| s(Some(p))).collect::<Vec<_>>();
                if let Some(stat) = sec.get("stat") {
                    let v = s(stat.get("value"));
                    let l = s(stat.get("label"));
                    if !v.is_empty() {
                        lines.push(format!("关键数字：{v} {l}"));
                    }
                }
                groups.push((name, lines));
            }
            sectioned_sheets(title, subtitle.as_deref(), "信息图", groups)
        }
        "matrix" => {
            let headers: Vec<String> = std::iter::once("项目".to_string())
                .chain(arr(payload, "columns").iter().map(|c| s(Some(c))))
                .collect();
            let rows = arr(payload, "rows")
                .iter()
                .map(|r| {
                    std::iter::once(s(r.get("label")))
                        .chain(arr(r, "cells").iter().map(|c| s(Some(c))))
                        .map(Cell::Text)
                        .collect()
                })
                .collect();
            vec![SheetSpec {
                name: "对比矩阵".into(),
                title: Some(merge_title(title, subtitle.as_deref())),
                headers,
                widths: widths(24, 34, 8),
                rows,
                chart: None,
            }]
        }
        "slideshow" => {
            let rows = arr(payload, "slides")
                .iter()
                .enumerate()
                .map(|(i, sl)| {
                    let mut body = arr(sl, "bullets").iter().map(|b| s(Some(b))).collect::<Vec<_>>().join("\n");
                    let notes = s(sl.get("notes"));
                    if !notes.is_empty() {
                        body = if body.is_empty() { format!("备注：{notes}") } else { format!("{body}\n备注：{notes}") };
                    }
                    vec![Cell::Num((i + 1) as f64), Cell::Text(s(sl.get("title"))), Cell::Text(body)]
                })
                .collect();
            vec![SheetSpec {
                name: "幻灯片".into(),
                title: Some(merge_title(title, subtitle.as_deref())),
                headers: vec!["页".into(), "标题".into(), "内容".into()],
                widths: vec![6.0, 24.0, 60.0],
                rows,
                chart: None,
            }]
        }
        "mindmap" => {
            let central_note = match subtitle {
                Some(m) => format!("{m}｜中心：{}", s(payload.get("central"))),
                None => format!("中心：{}", s(payload.get("central"))),
            };
            let rows = arr(payload, "branches")
                .iter()
                .flat_map(|br| {
                    let branch = s(br.get("label"));
                    arr(br, "children")
                        .iter()
                        .map(move |ch| {
                            let label = s(ch.get("label"));
                            let subs = arr(ch, "children").iter().map(|c| s(Some(c))).collect::<Vec<_>>().join("、");
                            vec![Cell::Text(branch.clone()), Cell::Text(label), Cell::Text(subs)]
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            vec![SheetSpec {
                name: "思维导图".into(),
                title: Some(merge_title(title, Some(&central_note))),
                headers: vec!["分支".into(), "子项".into(), "子子项".into()],
                widths: vec![24.0, 30.0, 40.0],
                rows,
                chart: None,
            }]
        }
        "audio" => {
            let rows = arr(payload, "segments")
                .iter()
                .map(|seg| vec![Cell::Text(s(seg.get("speaker"))), Cell::Text(s(seg.get("text")))])
                .collect();
            vec![SheetSpec {
                name: "转录".into(),
                title: Some(merge_title(title, subtitle.as_deref())),
                headers: vec!["说话人".into(), "内容".into()],
                widths: vec![18.0, 80.0],
                rows,
                chart: None,
            }]
        }
        other => {
            return Err(Error::Unsupported(format!(
                "{other} 没有表格形态，请导出为 docx 或 pptx"
            )));
        }
    };
    Ok(sheets)
}

fn merge_title(title: &str, meta: Option<&str>) -> String {
    match meta {
        Some(m) if !m.trim().is_empty() => format!("{title}（{m}）"),
        _ => title.to_string(),
    }
}

/// Sectioned kinds share one shape: a flat (分区, 内容[, …]) sheet plus a
/// per-section COUNTIF summary sheet with a bar chart.
fn sectioned_sheets(title: &str, meta: Option<&str>, main_name: &str, groups: Vec<(String, Vec<String>)>) -> Vec<SheetSpec> {
    let mut rows = Vec::new();
    for (name, lines) in &groups {
        for l in lines {
            rows.push(vec![Cell::Text(name.clone()), Cell::Text(l.clone())]);
        }
    }
    let total = rows.len();
    // The main sheet always has a title row → data starts at 0-based row 3
    // (Excel row 4). The summary sheet also has a title → same offset.
    let sec_col = col_range(main_name, 0, 3, 2 + total);
    let summary_rows = groups
        .iter()
        .map(|(name, _)| {
            vec![
                Cell::Text(name.clone()),
                Cell::Formula(format!("COUNTIF({sec_col},\"{}\")", name.replace('"', "\"\""))),
            ]
        })
        .collect();
    let summary_name = format!("{main_name}统计");
    vec![
        SheetSpec {
            name: main_name.to_string(),
            title: Some(merge_title(title, meta)),
            headers: vec!["分区".into(), "内容".into()],
            widths: vec![20.0, 80.0],
            rows,
            chart: None,
        },
        SheetSpec {
            name: summary_name.clone(),
            title: Some("分区条目统计".into()),
            headers: vec!["分区".into(), "条目数".into()],
            widths: vec![20.0, 12.0],
            rows: summary_rows,
            chart: Some(ChartSpec {
                kind: "bar".into(),
                title: "分区条目统计".into(),
                categories: col_range(&summary_name, 0, 3, 2 + groups.len()),
                values: col_range(&summary_name, 1, 3, 2 + groups.len()),
            }),
        },
    ]
}

/// One-call export: material payload → workbook file.
pub fn save_workbook(path: &Path, kind: &str, title: &str, meta: Option<&str>, payload: &Value) -> Result<()> {
    let sheets = material_sheets(kind, title, meta, payload)?;
    build_workbook(path, &sheets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_part<'a>(parts: &'a [(String, Vec<u8>)], name: &str) -> String {
        parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| String::from_utf8_lossy(b).to_string())
            .unwrap()
    }

    #[test]
    fn workbook_has_formulas_and_chart() {
        let dir = std::env::temp_dir().join(format!("cortex-xlsx-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("wb.xlsx");
        let sheets = vec![SheetSpec {
            name: "数据".into(),
            title: Some("测试表".into()),
            headers: vec!["名称".into(), "数量".into(), "占比".into()],
            widths: vec![20.0, 10.0, 10.0],
            rows: vec![
                vec![Cell::Text("a".into()), Cell::Num(3.0), Cell::Formula("B4/$B$6".into())],
                vec![Cell::Text("b".into()), Cell::Num(7.0), Cell::Formula("B5/$B$6".into())],
                vec![Cell::Text("合计".into()), Cell::Formula("SUM(B4:B5)".into())],
            ],
            chart: None,
        }];
        build_workbook(&p, &sheets).unwrap();
        let parts = super::super::read_zip_parts(&p).unwrap();
        let sheet1 = find_part(&parts, "xl/worksheets/sheet1.xml");
        assert!(sheet1.contains("<f>SUM(B4:B5)</f>"), "formula missing: {sheet1}");
        // Cell strings live in the shared-strings part, not in the sheet XML.
        assert!(
            find_part(&parts, "xl/sharedStrings.xml").contains("测试表"),
            "title string missing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quiz_sheets_get_countif_and_chart() {
        let payload = serde_json::json!([
            {"q": "栈的性质?", "options": ["LIFO", "FIFO"], "answer": 0, "explain": "先进后出"},
            {"q": "队列?", "options": ["LIFO", "FIFO"], "answer": 1}
        ]);
        let sheets = material_sheets("quiz", "数据结构测验", Some("期末复习"), &payload).unwrap();
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].name, "题库");
        assert_eq!(sheets[0].rows.len(), 2);
        let dist = &sheets[1];
        assert_eq!(dist.name, "答案分布");
        let f0 = match &dist.rows[0][1] {
            Cell::Formula(f) => f.clone(),
            other => panic!("not a formula: {other:?}"),
        };
        assert!(f0.starts_with("COUNTIF('题库'!$D$4:$D$5"), "formula: {f0}");
        let chart = dist.chart.as_ref().unwrap();
        assert!(chart.values.contains("'答案分布'!$B$4:$B$5"));

        let dir = std::env::temp_dir().join(format!("cortex-xlsx-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("quiz.xlsx");
        save_workbook(&p, "quiz", "数据结构测验", None, &payload).unwrap();
        let parts = super::super::read_zip_parts(&p).unwrap();
        let chart_xml = find_part(&parts, "xl/charts/chart1.xml");
        assert!(chart_xml.contains("答案分布"));
        assert!(find_part(&parts, "xl/workbook.xml").contains("题库"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sectioned_kinds_get_summary_chart() {
        let cs = serde_json::json!({"sections": [
            {"title": "排序", "items": [{"t": "快排", "d": "O(n log n)"}, {"t": "堆排", "d": "O(n log n)"}]},
            {"title": "查找", "items": [{"t": "二分", "d": "O(log n)"}]}
        ]});
        let sheets = material_sheets("cheatsheet", "算法速查", None, &cs).unwrap();
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].rows.len(), 3);
        let f = match &sheets[1].rows[0][1] {
            Cell::Formula(f) => f.clone(),
            other => panic!("not a formula: {other:?}"),
        };
        assert!(f.starts_with("COUNTIF('速查表'!$A$4:$A$6"), "formula: {f}");
        assert!(sheets[1].chart.is_some());

        let err = material_sheets("flashcards_nope", "x", None, &serde_json::json!({})).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn sheet_names_are_sanitized_and_deduped() {
        assert_eq!(sanitize_sheet_name("a[b]:c*d?e/f\\g*h"), "a_b__c_d_e_f_g_h");
        let long = "长度超限的工作表名称超出三十一字符的测试用例名称还需要更多文字".to_string();
        assert_eq!(sanitize_sheet_name(&long).chars().count(), 31);
        let deduped = dedupe_names(&["Sheet".into(), "Sheet".into()]);
        assert_ne!(deduped[0], deduped[1]);
    }

    #[test]
    fn col_letters_basics() {
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(3), "D");
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_range("表", 1, 2, 5), "'表'!$B$3:$B$6");
    }
}
