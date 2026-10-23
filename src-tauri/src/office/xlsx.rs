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

// ------------------------------------------------- editing an existing book

/// A value written into one cell. `Blank` clears the content but keeps the
/// cell's style — "clear the cell" in Excel means keep the formatting.
#[derive(Debug, Clone)]
pub enum CellValue {
    Text(String),
    Num(f64),
    /// Formula WITHOUT the leading `=`; no cached result is written, so Excel
    /// recalculates on open instead of showing a value that is already stale.
    Formula(String),
    Blank,
}

/// One cell assignment for [`set_cells`].
///
/// `value` is typed the way Excel types it: a leading `=` is a formula, a bare
/// number is a number, an empty string clears the cell, anything else is text.
/// Keeping the IPC surface to strings means the UI asks "which cell, what
/// value" instead of demanding a type first — and `007` behaves like it does in
/// Excel (becomes 7), which is the behaviour a person already expects.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CellEdit {
    /// Sheet name; omitted → the workbook's first sheet.
    #[serde(default)]
    pub sheet: Option<String>,
    /// A1-style address, e.g. `B2`.
    pub cell: String,
    #[serde(default)]
    pub value: String,
}

impl CellEdit {
    fn typed(&self) -> CellValue {
        let s = self.value.trim();
        if s.is_empty() {
            CellValue::Blank
        } else if let Some(f) = s.strip_prefix('=') {
            CellValue::Formula(f.to_string())
        } else if let Ok(n) = s.parse::<f64>() {
            CellValue::Num(n)
        } else {
            CellValue::Text(self.value.clone())
        }
    }
}

/// What one assignment actually changed, including the previous value — the
/// "改了哪几处" list the caller shows so an edit is never a silent rewrite.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CellChange {
    pub sheet: String,
    pub cell: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct XlsxEditOutcome {
    pub output: String,
    pub changed: Vec<CellChange>,
    /// Assignments that could not be applied, each with its reason. A bad
    /// address is reported, never guessed at.
    pub skipped: Vec<String>,
}

/// Edit cells of an existing workbook, writing the result to `output`.
///
/// The input is never touched: a spreadsheet the user already has cannot be
/// regenerated, so the only safe edit is one that lands next to it. Values are
/// written as XML surgery on the sheet part, which is what keeps everything
/// this code does not understand — styles, merges, conditional formatting,
/// images, other sheets — byte-identical.
pub fn set_cells(input: &Path, output: &Path, edits: &[CellEdit]) -> Result<XlsxEditOutcome> {
    if input == output {
        return Err(Error::Other("输出文件需与原文件不同（改表在副本上做）".into()));
    }
    if edits.is_empty() {
        return Err(Error::Other("没有要改的单元格".into()));
    }
    let mut parts = crate::office::read_zip_parts(input)?;
    let sheets = sheet_parts(&parts)?;
    let shared = shared_strings(&parts);
    let first = sheets.first().map(|(n, _)| n.clone());
    let mut changed = Vec::new();
    let mut skipped = Vec::new();

    for (name, part) in &sheets {
        let mine: Vec<&CellEdit> = edits
            .iter()
            .filter(|e| match &e.sheet {
                Some(s) => s == name,
                None => first.as_deref() == Some(name.as_str()),
            })
            .collect();
        if mine.is_empty() {
            continue;
        }
        let idx = parts
            .iter()
            .position(|(n, _)| n == part)
            .ok_or_else(|| Error::Other(format!("缺少工作表部件 {part}")))?;
        let mut xml = String::from_utf8(parts[idx].1.clone())
            .map_err(|_| Error::Other(format!("{part} 不是 UTF-8")))?;
        for e in mine {
            match set_cell_in_sheet(&xml, &e.cell, &e.typed(), &shared) {
                Ok((next, before)) => {
                    xml = next;
                    changed.push(CellChange {
                        sheet: name.clone(),
                        cell: e.cell.trim().to_uppercase(),
                        before,
                        after: e.value.trim().to_string(),
                    });
                }
                Err(err) => skipped.push(format!("{}!{}: {err}", name, e.cell.trim())),
            }
        }
        parts[idx].1 = xml.into_bytes();
    }

    // A sheet that does not exist is a mistake worth naming — silently writing
    // nothing would look like the edit succeeded.
    for e in edits {
        if let Some(s) = &e.sheet {
            if !sheets.iter().any(|(n, _)| n == s) {
                skipped.push(format!("{s}: 表格里没有这个工作表"));
            }
        }
    }
    crate::office::write_zip(output, &parts)?;
    Ok(XlsxEditOutcome { output: output.display().to_string(), changed, skipped })
}

// ------------------------------------------------------------ preview

/// What the caller needs to pick a cell: the sheet list, the active sheet, and
/// its head rows. Read-only, so a bad path costs nothing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SheetPreview {
    pub sheets: Vec<String>,
    pub sheet: String,
    pub rows: Vec<Vec<String>>,
    /// True when there are more rows than were returned.
    pub truncated: bool,
}

/// Render one spreadsheet cell the way it reads on screen.
fn calamine_text(d: &calamine::Data) -> String {
    use calamine::Data;
    match d {
        Data::Empty => String::new(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => num_text(*f),
        Data::String(s) => s.clone(),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::DateTime(dt) => num_text(dt.as_f64()),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#{e}"),
    }
}

pub fn preview(path: &Path, sheet: Option<&str>, max_rows: usize) -> Result<SheetPreview> {
    use calamine::Reader;
    let mut wb = calamine::open_workbook_auto(path)
        .map_err(|e| Error::Other(format!("打开表格失败：{e}")))?;
    let sheets = wb.sheet_names().to_vec();
    let name = match sheet {
        Some(s) if sheets.iter().any(|n| n == s) => s.to_string(),
        _ => sheets
            .first()
            .cloned()
            .ok_or_else(|| Error::Other("表格里没有工作表".into()))?,
    };
    let range = wb
        .worksheet_range(&name)
        .map_err(|e| Error::Other(format!("读取工作表 {name} 失败：{e}")))?;
    let max = max_rows.clamp(1, 200);
    let rows: Vec<Vec<String>> = range
        .rows()
        .take(max)
        .map(|r| r.iter().map(calamine_text).collect())
        .collect();
    Ok(SheetPreview { sheets, sheet: name, rows, truncated: range.height() > max })
}

// ------------------------------------------------------- XML surface

/// One located element: `<name …>`, `<name …/>`, or `<name …>…</name>`.
#[derive(Debug, Clone, Copy)]
struct Element {
    start: usize,
    /// Index of the `>` that closes the OPENING tag.
    tag_end: usize,
    content_start: usize,
    /// One past the element's final `>` (== `content_start` when self-closing).
    end: usize,
    self_closing: bool,
    name_len: usize,
}

impl Element {
    fn tag<'a>(&self, s: &'a str) -> &'a str {
        &s[self.start + 1..self.tag_end]
    }
    fn inner<'a>(&self, s: &'a str) -> &'a str {
        if self.self_closing {
            ""
        } else {
            // content ends where the closing tag starts
            &s[self.content_start..self.end - (self.name_len + 3)]
        }
    }
}

/// Locate the next `<name>` / `<name/>` at or after `from`. Names are matched
/// exactly (so `<c` never matches `<cols>`), comments and closing tags are
/// skipped, and no XML parser is involved: the sheet part of a workbook is
/// machine-written and shallow, and round-tripping it through a parser would
/// reorder attributes and namespaces that other writers key off.
fn find_element(s: &str, name: &str, from: usize) -> Option<Element> {
    let mut i = from.min(s.len());
    while let Some(off) = s[i..].find('<') {
        let start = i + off;
        let rest = &s[start..];
        if rest.starts_with("</") || rest.starts_with("<!") || rest.starts_with("<?") {
            i = start + 2;
            continue;
        }
        let name_start = start + 1;
        if !s[name_start..].starts_with(name) {
            i = name_start;
            continue;
        }
        let after = name_start + name.len();
        let next = s[after..].chars().next()?;
        if !(next.is_whitespace() || next == '>' || next == '/') {
            i = after;
            continue;
        }
        let tag_end = after + s[after..].find('>')?;
        let self_closing = s[name_start..tag_end].trim_end().ends_with('/');
        let content_start = tag_end + 1;
        if self_closing {
            return Some(Element {
                start,
                tag_end,
                content_start,
                end: content_start,
                self_closing,
                name_len: name.len(),
            });
        }
        let close = format!("</{name}>");
        let close_at = s[tag_end..].find(&close)? + tag_end;
        return Some(Element {
            start,
            tag_end,
            content_start,
            end: close_at + close.len(),
            self_closing,
            name_len: name.len(),
        });
    }
    None
}

/// Value of one attribute inside a tag's text (`r="B2"` → `B2`). Requires a
/// whitespace boundary in front so `id` cannot match `sheetId` or `r:id`.
fn find_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let mut from = 0;
    while let Some(i) = tag[from..].find(&needle) {
        let at = from + i;
        let boundary = tag[..at].chars().next_back().map(char::is_whitespace).unwrap_or(true);
        if boundary {
            let vstart = at + needle.len();
            let vend = vstart + tag[vstart..].find('"')?;
            return Some(tag[vstart..vend].to_string());
        }
        from = at + needle.len();
    }
    None
}

/// A1 address → (0-based column, 1-based row). `$B$2` is accepted (people paste
/// absolute references), anything else is refused.
fn parse_a1(addr: &str) -> Option<(usize, u32)> {
    let mut letters = String::new();
    let mut digits = String::new();
    for c in addr.trim().chars().filter(|c| *c != '$') {
        if c.is_ascii_alphabetic() && digits.is_empty() {
            letters.push(c.to_ascii_uppercase());
        } else if c.is_ascii_digit() {
            digits.push(c);
        } else {
            return None;
        }
    }
    if letters.is_empty() || digits.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut col = 0usize;
    for b in letters.bytes() {
        col = col * 26 + (b - b'A' + 1) as usize;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((col - 1, row))
}

fn num_text(n: f64) -> String {
    if n.is_finite() && (n - n.trunc()).abs() < 1e-9 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn render_cell(addr: &str, style: Option<&str>, value: &CellValue) -> String {
    let s = style
        .map(|v| format!(" s=\"{}\"", v.trim()))
        .unwrap_or_default();
    match value {
        CellValue::Blank => format!("<c r=\"{addr}\"{s}/>"),
        CellValue::Num(n) => format!("<c r=\"{addr}\"{s}><v>{}</v></c>", num_text(*n)),
        // The cached value would be wrong the moment the formula is opened, so
        // none is written; Excel fills it in. A formula that references other
        // edited cells must see their new values, which only a real recalc can
        // guarantee.
        CellValue::Formula(f) => format!(
            "<c r=\"{addr}\"{s}><f>{}</f></c>",
            crate::office::xml_escape(f.trim_start_matches('='))
        ),
        // Inline string: self-contained, so editing needs no sharedStrings
        // surgery and cannot corrupt the index every other cell depends on.
        CellValue::Text(t) => format!(
            "<c r=\"{addr}\"{s} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
            crate::office::xml_escape(t)
        ),
    }
}

/// The displayed previous value of a cell, for the change list.
fn cell_text(xml: &str, el: &Element, shared: &[String]) -> String {
    let tag = el.tag(xml);
    let ty = find_attr(tag, "t");
    let inner = el.inner(xml);
    if let Some(t) = find_element(inner, "t", 0) {
        return crate::office::xml_unescape(t.inner(inner));
    }
    if let Some(f) = find_element(inner, "f", 0) {
        return format!("={}", crate::office::xml_unescape(f.inner(inner)));
    }
    if let Some(v) = find_element(inner, "v", 0) {
        let raw = v.inner(inner).to_string();
        return match ty.as_deref() {
            Some("s") => raw
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|i| shared.get(i).cloned())
                .unwrap_or(raw),
            Some("b") => if raw.trim() == "1" { "TRUE" } else { "FALSE" }.to_string(),
            Some("str") => crate::office::xml_unescape(&raw),
            _ => raw,
        };
    }
    String::new()
}

/// Write one cell into a sheet part. Returns the new XML and the old display
/// value. Missing rows and cells are created in order; a bad address is an
/// error, not a guess.
fn set_cell_in_sheet(
    xml: &str,
    cell: &str,
    value: &CellValue,
    shared: &[String],
) -> Result<(String, String)> {
    let (col, row) = parse_a1(cell)
        .ok_or_else(|| Error::Other("不是合法的单元格地址（应形如 B2）".into()))?;
    let addr = format!("{}{}", col_letters(col), row);

    let row_label = row.to_string();
    let mut cursor = 0;
    let mut row_el: Option<Element> = None;
    while let Some(el) = find_element(xml, "row", cursor) {
        if find_attr(el.tag(xml), "r").map(|v| v.trim() == row_label).unwrap_or(false) {
            row_el = Some(el);
            break;
        }
        cursor = el.end.max(el.start + 1);
    }

    let Some(row_el) = row_el else {
        // No such row: create it, in numeric order.
        let sd = find_element(xml, "sheetData", 0)
            .ok_or_else(|| Error::Other("工作表缺少 sheetData".into()))?;
        let new_row = format!("<row r=\"{row}\">{}</row>", render_cell(&addr, None, value));
        if sd.self_closing {
            let opened = format!(
                "<{}{new_row}</sheetData>",
                sd.tag(xml).trim_end_matches('/').trim_end()
            );
            let mut out = String::with_capacity(xml.len() + opened.len());
            out.push_str(&xml[..sd.start]);
            out.push_str(&opened);
            out.push_str(&xml[sd.end..]);
            return Ok((out, String::new()));
        }
        let mut at = sd.end - "</sheetData>".len();
        let mut c = sd.content_start;
        while let Some(re) = find_element(xml, "row", c) {
            if re.start >= at {
                break;
            }
            let r = find_attr(re.tag(xml), "r").and_then(|v| v.trim().parse::<u32>().ok());
            if r.map(|r| r > row).unwrap_or(false) {
                at = re.start;
                break;
            }
            c = re.end.max(re.start + 1);
        }
        let mut out = String::with_capacity(xml.len() + new_row.len());
        out.push_str(&xml[..at]);
        out.push_str(&new_row);
        out.push_str(&xml[at..]);
        return Ok((out, String::new()));
    };

    // Collect the row's cells so a new one lands in column order.
    let mut cells: Vec<Element> = Vec::new();
    let inner_end = if row_el.self_closing {
        row_el.content_start
    } else {
        row_el.end - "</row>".len()
    };
    let mut c = row_el.content_start;
    while c < inner_end {
        match find_element(xml, "c", c) {
            Some(ce) if ce.start < inner_end => {
                c = ce.end.max(ce.start + 1);
                cells.push(ce);
            }
            _ => break,
        }
    }

    let hit = cells
        .iter()
        .find(|ce| find_attr(ce.tag(xml), "r").map(|r| r.trim().eq_ignore_ascii_case(&addr)).unwrap_or(false));

    if let Some(ce) = hit {
        let before = cell_text(xml, ce, shared);
        let style = find_attr(ce.tag(xml), "s");
        let new_cell = render_cell(&addr, style.as_deref(), value);
        let mut out = String::with_capacity(xml.len() + new_cell.len());
        out.push_str(&xml[..ce.start]);
        out.push_str(&new_cell);
        out.push_str(&xml[ce.end..]);
        return Ok((out, before));
    }

    let new_cell = render_cell(&addr, None, value);
    if row_el.self_closing {
        // `<row r="5"/>` has nowhere to put a child: reopen it.
        let opened = format!(
            "<{}{new_cell}</row>",
            row_el.tag(xml).trim_end_matches('/').trim_end()
        );
        let mut out = String::with_capacity(xml.len() + opened.len());
        out.push_str(&xml[..row_el.start]);
        out.push_str(&opened);
        out.push_str(&xml[row_el.end..]);
        return Ok((out, String::new()));
    }
    let at = cells
        .iter()
        .find(|ce| {
            find_attr(ce.tag(xml), "r")
                .and_then(|r| parse_a1(&r))
                .map(|(c, _)| c > col)
                .unwrap_or(false)
        })
        .map(|ce| ce.start)
        .unwrap_or(inner_end);
    let mut out = String::with_capacity(xml.len() + new_cell.len());
    out.push_str(&xml[..at]);
    out.push_str(&new_cell);
    out.push_str(&xml[at..]);
    Ok((out, String::new()))
}

/// sheet name → sheet part path, in workbook order.
fn sheet_parts(parts: &[(String, Vec<u8>)]) -> Result<Vec<(String, String)>> {
    let wb = parts
        .iter()
        .find(|(n, _)| n == "xl/workbook.xml")
        .ok_or_else(|| Error::Other("不是 xlsx：缺少 xl/workbook.xml".into()))?;
    let wb_xml = std::str::from_utf8(&wb.1)
        .map_err(|_| Error::Other("workbook.xml 不是 UTF-8".into()))?;
    let rels_xml = parts
        .iter()
        .find(|(n, _)| n == "xl/_rels/workbook.xml.rels")
        .and_then(|(_, b)| std::str::from_utf8(b).ok())
        .unwrap_or("");

    let mut targets: Vec<(String, String)> = Vec::new();
    let mut cur = 0;
    while let Some(rel) = find_element(rels_xml, "Relationship", cur) {
        let tag = rel.tag(rels_xml);
        if let (Some(id), Some(target)) = (find_attr(tag, "Id"), find_attr(tag, "Target")) {
            // Targets are relative to xl/, unless absolute.
            let path = match target.strip_prefix('/') {
                Some(abs) => abs.to_string(),
                None => format!("xl/{}", target.trim_start_matches("./")),
            };
            targets.push((id, path));
        }
        cur = rel.end.max(rel.start + 1);
    }

    let mut out = Vec::new();
    let mut cur = 0;
    while let Some(sh) = find_element(wb_xml, "sheet", cur) {
        let tag = sh.tag(wb_xml);
        let name = find_attr(tag, "name").unwrap_or_default();
        let rid = find_attr(tag, "r:id").unwrap_or_default();
        if let Some((_, path)) = targets.iter().find(|(id, _)| *id == rid) {
            out.push((crate::office::xml_unescape(&name), path.clone()));
        }
        cur = sh.end.max(sh.start + 1);
    }
    if out.is_empty() {
        return Err(Error::Other("表格里没有工作表".into()));
    }
    Ok(out)
}

/// The workbook's shared-string table, so a change list can show the old text
/// instead of `t="s"` index 17.
fn shared_strings(parts: &[(String, Vec<u8>)]) -> Vec<String> {
    let Some((_, bytes)) = parts.iter().find(|(n, _)| n == "xl/sharedStrings.xml") else {
        return Vec::new();
    };
    let Ok(xml) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cur = 0;
    while let Some(si) = find_element(xml, "si", cur) {
        out.push(
            find_element(si.inner(xml), "t", 0)
                .map(|t| crate::office::xml_unescape(t.inner(si.inner(xml))))
                .unwrap_or_default(),
        );
        cur = si.end.max(si.start + 1);
    }
    out
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


    // ---- editing an existing workbook ----

    /// A workbook shaped like the ones the app produces: title, header, three
    /// data rows, one formula column.
    fn edit_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("cortex-xlsx-edit-{tag}-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("原表.xlsx");
        let sheets = vec![SheetSpec {
            name: "数据".into(),
            title: Some("测试表".into()),
            headers: vec!["名称".into(), "数量".into()],
            widths: vec![],
            rows: vec![
                vec![Cell::Text("a".into()), Cell::Num(3.0)],
                vec![Cell::Text("b".into()), Cell::Num(7.0)],
                vec![Cell::Text("合计".into()), Cell::Formula("SUM(B4:B5)".into())],
            ],
            chart: None,
        }];
        build_workbook(&src, &sheets).unwrap();
        let out = dir.join("改后.xlsx");
        (dir, src, out)
    }

    fn cell_of(xml: &str, addr: &str) -> Option<String> {
        let needle = format!("<c r=\"{addr}\"");
        let at = xml.find(&needle)?;
        let end = at + xml[at..].find('>')? + 1;
        Some(xml[at..end].to_string())
    }

    #[test]
    fn editing_a_copy_writes_numbers_text_and_formulas_without_touching_the_input() {
        let (dir, src, out) = edit_fixture("apply");
        let before_xml = find_part(&crate::office::read_zip_parts(&src).unwrap(), "xl/worksheets/sheet1.xml");
        assert!(cell_of(&before_xml, "A3").unwrap().contains("s=\"2\""), "the header row is styled");

        let edits = vec![
            CellEdit { sheet: Some("数据".into()), cell: "B4".into(), value: "11".into() },
            CellEdit { sheet: Some("数据".into()), cell: "A4".into(), value: "甲".into() },
            CellEdit { sheet: Some("数据".into()), cell: "D4".into(), value: "=B4*2".into() },
            CellEdit { sheet: Some("数据".into()), cell: "A9".into(), value: "新行".into() },
            CellEdit { sheet: Some("数据".into()), cell: "A3".into(), value: "名称2".into() },
        ];
        let r = set_cells(&src, &out, &edits).unwrap();
        assert_eq!(r.changed.len(), 5, "skipped: {:?}", r.skipped);
        assert!(r.skipped.is_empty(), "{:?}", r.skipped);

        // The original is byte-identical: an edit never rewrites what it read.
        let after_src = crate::office::read_zip_parts(&src).unwrap();
        assert_eq!(find_part(&after_src, "xl/worksheets/sheet1.xml"), before_xml);

        let xml = find_part(&crate::office::read_zip_parts(&out).unwrap(), "xl/worksheets/sheet1.xml");
        assert!(xml.contains("<v>11</v>"), "number not written: {xml}");
        assert!(xml.contains("<t xml:space=\"preserve\">甲</t>"), "text not inlined: {xml}");
        assert!(xml.contains("<f>B4*2</f>"), "formula not written: {xml}");
        assert!(xml.contains("<row r=\"9\""), "missing row not created: {xml}");
        // Formatting survived: rewriting a styled cell keeps its `s=` index,
        // which is what makes an edit in place rather than a rebuild.
        let header = cell_of(&xml, "A3").unwrap();
        assert!(header.contains("s=\"2\""), "the style is gone: {header}");
        assert!(xml.contains("名称2"), "the header text was not replaced: {header}");

        // And the file opens as a workbook with the new values.
        use calamine::Reader;
        let mut wb = calamine::open_workbook_auto(&out).unwrap();
        let range = wb.worksheet_range("数据").unwrap();
        assert_eq!(crate::office::xlsx::calamine_text(&range[(3, 0)]), "甲");
        assert_eq!(crate::office::xlsx::calamine_text(&range[(3, 1)]), "11");
        assert_eq!(crate::office::xlsx::calamine_text(&range[(8, 0)]), "新行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_row_that_does_not_exist_yet_lands_in_numeric_order() {
        let (dir, src, out) = edit_fixture("order");
        set_cells(&src, &out, &[CellEdit { sheet: None, cell: "B2".into(), value: "9".into() }]).unwrap();
        let xml = find_part(&crate::office::read_zip_parts(&out).unwrap(), "xl/worksheets/sheet1.xml");
        let r2 = xml.find("<row r=\"2\"").expect("row 2 created");
        let r3 = xml.find("<row r=\"3\"").expect("row 3 is the header");
        assert!(r2 < r3, "a new row must not be appended out of order");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_a_cell_drops_the_value_but_keeps_the_formatting() {
        let (dir, src, out) = edit_fixture("clear");
        // A3 is the styled header cell; clearing it must not strip the format.
        let r = set_cells(&src, &out, &[CellEdit { sheet: Some("数据".into()), cell: "A3".into(), value: String::new() }]).unwrap();
        assert_eq!(r.changed[0].before, "名称", "the change list shows what was there");
        let xml = find_part(&crate::office::read_zip_parts(&out).unwrap(), "xl/worksheets/sheet1.xml");
        let cleared = cell_of(&xml, "A3").unwrap();
        assert!(cleared.ends_with("/>"), "cleared cell is empty: {cleared}");
        assert!(cleared.contains("s=\"2\""), "a cleared cell keeps its style: {cleared}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mistakes_are_reported_instead_of_written() {
        let (dir, src, out) = edit_fixture("skip");
        let r = set_cells(
            &src,
            &out,
            &[
                CellEdit { sheet: Some("不存在".into()), cell: "A1".into(), value: "x".into() },
                CellEdit { sheet: Some("数据".into()), cell: "不是地址".into(), value: "x".into() },
            ],
        )
        .unwrap();
        assert!(r.changed.is_empty());
        assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
        assert!(r.skipped.iter().any(|m| m.contains("不存在")), "{:?}", r.skipped);
        assert!(r.skipped.iter().any(|m| m.contains("不是合法的单元格地址")), "{:?}", r.skipped);
        // Refusing to write into itself is the difference between an edit and
        // destroying the only copy.
        let err = set_cells(&src, &src, &[CellEdit { sheet: None, cell: "A1".into(), value: "x".into() }])
            .unwrap_err()
            .to_string();
        assert!(err.contains("副本"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_shows_the_sheets_and_their_head_rows() {
        let (dir, src, _) = edit_fixture("preview");
        let p = preview(&src, None, 10).unwrap();
        assert_eq!(p.sheets, vec!["数据".to_string()]);
        assert_eq!(p.sheet, "数据");
        assert_eq!(p.rows[0][0], "测试表", "title row");
        assert_eq!(p.rows[2][0], "名称", "header row");
        assert!(!p.truncated);
        assert!(preview(&src, Some("数据"), 2).unwrap().truncated, "the cap is honest");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn col_letters_basics() {
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(3), "D");
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_range("表", 1, 2, 5), "'表'!$B$3:$B$6");
    }
}
