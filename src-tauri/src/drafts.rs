//! Drafts — the last leg of the research pipeline: a selection of excerpts
//! becomes a citable paragraph. The excerpt ids (in citation order) are stored
//! alongside the payload, so [1] always resolves to `excerpt_ids[0]` even if the
//! model rearranges or drops citations; the reference list is built HERE, never
//! taken from the model.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};

/// One numbered citation slot, resolved server-side from the picked excerpts.
#[derive(Debug, Clone, Serialize)]
pub struct DraftRef {
    pub n: i64,
    pub excerpt_id: String,
    pub paper_id: Option<String>,
    pub title: String,
    pub authors: Option<String>,
    pub year: Option<i64>,
    pub page: Option<i64>,
    pub quote: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub id: String,
    pub title: String,
    pub excerpt_ids: Vec<String>,
    pub meta: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub created_at: i64,
}

/// What the model returns: {thesis, paragraphs:[{text, cites}]} — or, for
/// multi-section drafts, {thesis, sections:[{heading, paragraphs:[…]}]}.
/// `paragraphs` stays for backward compatibility with drafts written before
/// sections existed; renderers handle both.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftBody {
    #[serde(default)]
    pub thesis: String,
    #[serde(default)]
    pub paragraphs: Vec<DraftPara>,
    #[serde(default)]
    pub sections: Vec<DraftSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DraftSection {
    pub heading: String,
    #[serde(default)]
    pub paragraphs: Vec<DraftPara>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DraftPara {
    pub text: String,
    #[serde(default)]
    pub cites: Vec<i64>,
}

/// Resolve the picked excerpts into ordered citation slots, preserving the
/// caller's order and rejecting duplicates. Errors if any excerpt has been
/// deleted since it was picked — better a loud failure than a silent [3] that
/// points at nothing.
pub fn load_refs(conn: &Connection, excerpt_ids: &[String]) -> Result<Vec<DraftRef>> {
    let mut seen = HashSet::new();
    let mut ordered: Vec<&String> = Vec::new();
    for id in excerpt_ids {
        if !id.is_empty() && seen.insert(id.as_str()) {
            ordered.push(id);
        }
    }
    if ordered.is_empty() {
        return Err(Error::Other("No excerpts selected.".into()));
    }
    let mut refs = Vec::with_capacity(ordered.len());
    for (i, id) in ordered.into_iter().enumerate() {
        let row = conn
            .query_row(
                "SELECT e.id, e.paper_id, COALESCE(p.title, ''), p.authors, p.year, e.page, e.quote, e.note
                 FROM excerpts e LEFT JOIN papers p ON p.id = e.paper_id
                 WHERE e.id = ?1",
                params![id],
                |r| {
                    Ok(DraftRef {
                        n: (i + 1) as i64,
                        excerpt_id: r.get(0)?,
                        paper_id: r.get(1)?,
                        title: r.get(2)?,
                        authors: r.get(3)?,
                        year: r.get(4)?,
                        page: r.get(5)?,
                        quote: r.get(6)?,
                        note: r.get(7)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("excerpt {id} (deleted since it was picked)")))?;
        refs.push(row);
    }
    Ok(refs)
}

/// The numbered excerpt blocks handed to the model as the user message.
pub fn build_user_prompt(refs: &[DraftRef]) -> String {
    let mut blocks = String::new();
    for r in refs {
        let mut head = format!("[{}] Paper: {}", r.n, r.title);
        let mut meta: Vec<String> = Vec::new();
        if let Some(a) = r.authors.as_deref().filter(|s| !s.trim().is_empty()) {
            meta.push(a.to_string());
        }
        if let Some(y) = r.year {
            meta.push(y.to_string());
        }
        if !meta.is_empty() {
            head.push_str(&format!(" ({})", meta.join(", ")));
        }
        if let Some(p) = r.page {
            head.push_str(&format!(", p.{p}"));
        }
        head.push_str("\n    Quote: ");
        blocks.push_str(&head);
        blocks.push_str(r.quote.trim());
        blocks.push('\n');
        if let Some(note) = r.note.as_deref().filter(|s| !s.trim().is_empty()) {
            blocks.push_str(&format!("    Note: {}\n", note.trim()));
        }
        blocks.push('\n');
    }
    blocks.trim_end().to_string()
}

pub fn save(
    conn: &Connection,
    title: &str,
    excerpt_ids: &[String],
    meta: &str,
    payload: &serde_json::Value,
) -> Result<String> {    let id = new_id();
    conn.execute(
        "INSERT INTO drafts (id, title, excerpt_ids, meta, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            title,
            serde_json::to_string(excerpt_ids).unwrap_or_else(|_| "[]".into()),
            meta,
            payload.to_string(),
            now_ms(),
        ],
    )?;
    Ok(id)
}

pub fn get(conn: &Connection, id: &str) -> Result<Draft> {
    conn.query_row(
        "SELECT id, title, excerpt_ids, meta, payload, created_at FROM drafts WHERE id = ?1",
        params![id],
        row_to_draft,
    )
    .map_err(|_| Error::NotFound(format!("draft {id}")))
}

pub fn list(conn: &Connection) -> Result<Vec<Draft>> {
    let mut st = conn.prepare(
        "SELECT id, title, excerpt_ids, meta, payload, created_at FROM drafts ORDER BY created_at DESC LIMIT 100",
    )?;
    let rows = st.query_map([], row_to_draft)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let n = conn.execute("DELETE FROM drafts WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(Error::NotFound(format!("draft {id}")));
    }
    Ok(())
}

/// Persist an edited payload — the write-back path for 2.1 升格批改 (accepted
/// rewrites / upgrades patch sentences in place). Payload must round-trip
/// through DraftBody so a corrupted table can never be saved over a draft.
pub fn update_payload(conn: &Connection, id: &str, payload: &serde_json::Value) -> Result<()> {
    let body: DraftBody = serde_json::from_value(payload.clone())
        .map_err(|_| Error::Other("payload 不符合成稿结构（thesis/paragraphs/sections）".into()))?;
    if body.paragraphs.is_empty() && body.sections.is_empty() {
        return Err(Error::Other("payload 不能为空：缺少 paragraphs/sections".into()));
    }
    let n = conn.execute(
        "UPDATE drafts SET payload = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(payload).unwrap_or_default()],
    )?;
    if n == 0 {
        return Err(Error::NotFound(format!("draft {id}")));
    }
    Ok(())
}

fn row_to_draft(r: &rusqlite::Row<'_>) -> rusqlite::Result<Draft> {
    let ids_raw: String = r.get(2)?;
    let payload_raw: Option<String> = r.get(4)?;
    Ok(Draft {
        id: r.get(0)?,
        title: r.get(1)?,
        excerpt_ids: serde_json::from_str(&ids_raw).unwrap_or_default(),
        meta: r.get(3)?,
        payload: payload_raw.and_then(|p| serde_json::from_str(&p).ok()),
        created_at: r.get(5)?,
    })
}


// ---- LaTeX export (multi-section drafts → compilable .tex) -----------------

/// Escape LaTeX special characters OUTSIDE math spans. Math (\(…\), $$…$$,
/// $…$) passes through verbatim — escaping inside it would destroy the
/// equations the model already wrote in LaTeX. Bracket citations [1] survive
/// escaping untouched (brackets are not special).
pub fn latex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0;
    // Math states: display $$…$$, inline-paren \(…\), single $…$.
    let mut math_until: Option<&str> = None;
    while i < n {
        if math_until.is_none() {
            // Detect math openers.
            if chars[i] == '\\' && i + 1 < n && chars[i + 1] == '(' {
                math_until = Some("\\)");
                out.push_str("\\(");
                i += 2;
                continue;
            }
            if chars[i] == '$' && i + 1 < n && chars[i + 1] == '$' {
                math_until = Some("$$");
                out.push_str("$$");
                i += 2;
                continue;
            }
            if chars[i] == '$' {
                math_until = Some("$");
                out.push('$');
                i += 1;
                continue;
            }
            match chars[i] {
                '&' => out.push_str("\\&"),
                '%' => out.push_str("\\%"),
                '#' => out.push_str("\\#"),
                '_' => out.push_str("\\_"),
                '{' => out.push_str("\\{"),
                '}' => out.push_str("\\}"),
                '~' => out.push_str("\\textasciitilde{}"),
                '^' => out.push_str("\\textasciicircum{}"),
                '\\' => out.push_str("\\textbackslash{}"),
                c => out.push(c),
            }
            i += 1;
        } else {
            let closer = math_until.unwrap();
            let cl: Vec<char> = closer.chars().collect();
            let m = cl.len();
            if i + m <= n && chars[i..i + m] == cl[..] {
                for _ in 0..m {
                    out.push(chars[i]);
                    i += 1;
                }
                math_until = None;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Render a draft payload as a COMPILABLE .tex (ctexart handles Chinese via
/// xelatex). Deterministic: the reference list comes from the payload's
/// references (built from real excerpts at generation time), never from the
/// model. Multi-section drafts become \section blocks; legacy flat drafts
/// render as plain paragraphs.
pub fn to_latex(title: &str, meta: Option<&str>, payload: &serde_json::Value) -> String {
    let s = |v: Option<&serde_json::Value>| {
        v.and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let arr = |v: &serde_json::Value, key: &str| -> Vec<serde_json::Value> {
        v.get(key).and_then(|x| x.as_array()).cloned().unwrap_or_default()
    };

    let mut out = String::new();
    out.push_str("\\documentclass[UTF8,12pt]{ctexart}\n");
    out.push_str("\\usepackage[a4paper,margin=2.5cm]{geometry}\n\n");
    out.push_str(&format!("\\title{{{}}}\n", latex_escape(title)));
    out.push_str("\\date{}\n\n\\begin{document}\n\\maketitle\n\n");

    if let Some(m) = meta.filter(|m| !m.trim().is_empty()) {
        out.push_str(&format!("\\begin{{center}}\\itshape {}\\end{{center}}\n\n", latex_escape(m)));
    }
    let thesis = s(payload.get("thesis"));
    if !thesis.trim().is_empty() {
        out.push_str(&format!("\\noindent\\textbf{{{} }}\n\n", latex_escape(&thesis)));
    }

    let flat: Vec<String> = arr(payload, "paragraphs")
        .iter()
        .map(|p| s(p.get("text")))
        .filter(|t| !t.trim().is_empty())
        .collect();
    for t in &flat {
        out.push_str(&latex_escape(t));
        out.push_str("\n\n");
    }
    for sec in arr(payload, "sections") {
        let heading = s(sec.get("heading"));
        if !heading.is_empty() {
            out.push_str(&format!("\\section{{{}}}\n\n", latex_escape(&heading)));
        }
        for p in arr(&sec, "paragraphs") {
            let t = s(p.get("text"));
            if !t.trim().is_empty() {
                out.push_str(&latex_escape(&t));
                out.push_str("\n\n");
            }
        }
    }

    let refs = arr(payload, "references");
    if !refs.is_empty() {
        out.push_str(&format!("\\begin{{thebibliography}}{{{}}}\n", refs.len()));
        for r in &refs {
            let n = r.get("n").and_then(|x| x.as_i64()).unwrap_or(0);
            let authors = s(r.get("authors"));
            let year = r.get("year").and_then(|x| x.as_i64());
            let page = r.get("page").and_then(|x| x.as_i64());
            let title_ = s(r.get("title"));
            let line = format!(
                "{}{}. {}.{}",
                if authors.is_empty() { "佚名".to_string() } else { authors },
                year.map(|y| format!(" ({y})")).unwrap_or_default(),
                title_,
                page.map(|p| format!(" p.{p} 摘录")).unwrap_or_default()
            );
            out.push_str(&format!("\\bibitem{{r{}}} {}\n", n, latex_escape(&line)));
        }
        out.push_str("\\end{thebibliography}\n");
    }

    out.push_str("\\end{document}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;
    use crate::papers;

    fn state() -> AppState {
        AppState::in_memory().unwrap()
    }

    fn seed(conn: &Connection) -> Vec<String> {
        let p = papers::import_paper(
            conn,
            &papers::PaperInput {
                title: "Attention Is All You Need".into(),
                authors: Some("Ashish Vaswani, Noam Shazeer".into()),
                year: Some(2017),
                venue: Some("NeurIPS".into()),
                doi: None,
                url: None,
                abstract_text: None,
                subject_id: None,
                source_id: None,
            },
        )
        .unwrap();
        let e1 = papers::add_excerpt(
            conn,
            Some(&p.id),
            None,
            "Self-attention beats recurrence.",
            Some(3),
            Some("core claim"),
            None,
        )
        .unwrap();
        let e2 = papers::add_excerpt(
            conn,
            Some(&p.id),
            None,
            "Multi-head attention captures subspaces.",
            Some(4),
            None,
            None,
        )
        .unwrap();
        vec![e1.id, e2.id]
    }

    #[test]
    fn refs_keep_pick_order_and_dedupe() {
        let st = state();
        let c = st.db.lock().unwrap();
        let ids = seed(&c);
        let refs = load_refs(&c, &[ids[1].clone(), ids[0].clone(), ids[1].clone()]).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].n, 1);
        assert_eq!(refs[0].excerpt_id, ids[1]);
        assert_eq!(refs[1].n, 2);
        assert_eq!(refs[1].title, "Attention Is All You Need");
        assert_eq!(refs[1].page, Some(3));
    }

    #[test]
    fn missing_excerpt_is_a_loud_error() {
        let st = state();
        let c = st.db.lock().unwrap();
        let ids = seed(&c);
        let err = load_refs(&c, &[ids[0].clone(), "gone".into()]).unwrap_err();
        assert!(err.to_string().contains("gone"));
    }

    #[test]
    fn empty_selection_is_rejected() {
        let st = state();
        let c = st.db.lock().unwrap();
        assert!(load_refs(&c, &[]).is_err());
    }

    #[test]
    fn user_prompt_numbers_excerpts() {
        let st = state();
        let c = st.db.lock().unwrap();
        let ids = seed(&c);
        let refs = load_refs(&c, &[ids[0].clone()]).unwrap();
        let p = build_user_prompt(&refs);
        assert!(p.contains("[1] Paper: Attention Is All You Need (Ashish Vaswani, Noam Shazeer, 2017), p.3"));
        assert!(p.contains("Quote: Self-attention beats recurrence."));
        assert!(p.contains("Note: core claim"));
    }

    #[test]
    fn save_get_list_roundtrip() {
        let st = state();
        let c = st.db.lock().unwrap();
        let ids = seed(&c);
        let payload = serde_json::json!({
            "thesis": "t",
            "paragraphs": [{"text": "x [1]", "cites": [1]}],
            "references": []
        });
        let id = save(&c, "成稿", &ids, "2 条摘录 · 1 段", &payload).unwrap();
        let d = get(&c, &id).unwrap();
        assert_eq!(d.excerpt_ids, ids);
        assert_eq!(d.payload.unwrap()["paragraphs"][0]["cites"][0], 1);
        assert_eq!(list(&c).unwrap().len(), 1);
        delete(&c, &id).unwrap();
        assert!(list(&c).unwrap().is_empty());
    }

    #[test]
    fn latex_escape_spares_math_spans() {
        assert_eq!(latex_escape("a & b"), "a \\& b");
        assert_eq!(latex_escape("x_1 #2"), "x\\_1 \\#2");
        // Inline \(…\) math passes verbatim, including specials.
        assert_eq!(latex_escape("损失 \\(L = \\frac{a}{b}\\) 收敛"), "损失 \\(L = \\frac{a}{b}\\) 收敛");
        // $$…$$ display math too.
        assert_eq!(latex_escape("$$e^{i\\pi}+1=0$$"), "$$e^{i\\pi}+1=0$$");
        // Single $…$.
        assert_eq!(latex_escape("$\\alpha$ 与 $\\beta$"), "$\\alpha$ 与 $\\beta$");
        // Citations [1] untouched.
        assert_eq!(latex_escape("如 [1] 所示"), "如 [1] 所示");
    }

    #[test]
    fn latex_document_has_sections_and_bibliography() {
        let payload = serde_json::json!({
            "thesis": "核心论点。",
            "sections": [
                { "heading": "背景", "paragraphs": [
                    { "text": "Transformer 改变了领域 [1]，准确率 88.2% & 显著。", "cites": [1] }
                ]},
                { "heading": "缺口", "paragraphs": [
                    { "text": "仍缺少跨语言验证 [1]。", "cites": [1] }
                ]}
            ],
            "references": [
                { "n": 1, "authors": "Vaswani, A.", "year": 2017, "title": "Attention", "page": 3 }
            ]
        });
        let tex = to_latex("综述：注意力", Some("由 1 条摘录生成"), &payload);
        assert!(tex.starts_with("\\documentclass[UTF8,12pt]{ctexart}"));
        assert!(tex.contains("\\section{背景}"));
        assert!(tex.contains("\\section{缺口}"));
        // The & inside prose is escaped, but math-span-free text keeps [1].
        assert!(tex.contains("88.2\\% \\& 显著"));
        assert!(tex.contains("\\bibitem{r1} Vaswani, A. (2017). Attention. p.3 摘录"));
        assert!(tex.trim_end().ends_with("\\end{document}"));
        // Legacy flat drafts still render (no sections key).
        let legacy = serde_json::json!({ "thesis": "t", "paragraphs": [ { "text": "一段话", "cites": [1] } ] });
        let tex2 = to_latex("旧稿", None, &legacy);
        assert!(!tex2.contains("\\section"));
        assert!(tex2.contains("一段话"));
    }
}
