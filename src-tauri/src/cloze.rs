//! Cloze (遮挡) flashcards — the occlusion self-test card type.
//!
//! A cloze card's *front* is a sentence with `{{c1::答案}}` markers; the
//! review screen renders each marker as a tappable mask (tap to reveal just
//! that span), so recall is tested per-concept instead of all-or-nothing.
//! The *back* is the same sentence with the answers bolded.
//!
//! Deliberately NOT a new SRS kind: cloze cards are ordinary flashcards
//! (kind="flashcards" material, q/a payload) and flow through the existing
//! FSRS queue untouched — the occlusion is a rendering concern, the marker
//! syntax is the storage format. The model only proposes; validation drops
//! sentences without a usable marker and caps the batch.

use crate::error::{Error, Result};
use crate::visuals;
use serde::Serialize;

const MAX_CARDS: usize = 30;
const MAX_SENTENCE: usize = 400;
const MAX_ANSWER: usize = 60;
const SYS: &str = "You are a study material writer. Output strict JSON only, no prose, no markdown fences.";

#[derive(Debug, Clone, Serialize)]
pub struct ClozeResult {
    pub material_id: String,
    pub count: usize,
}

/// Answers hidden inside one cloze sentence, in marker order.
pub fn extract_clozes(front: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = front;
    while let Some(start) = rest.find("{{c") {
        let Some(mid) = rest[start..].find("::") else { break };
        let Some(end_rel) = rest[start + mid..].find("}}") else { break };
        let answer = rest[start + mid + 2..start + mid + end_rel].trim();
        if !answer.is_empty() {
            out.push(answer.to_string());
        }
        rest = &rest[start + mid + end_rel + 2..];
    }
    out
}

/// The card back: the same sentence with every marker replaced by a bolded
/// answer (RichText renders markdown).
pub fn build_back(front: &str) -> String {
    let mut out = String::with_capacity(front.len());
    let mut rest = front;
    while let Some(start) = rest.find("{{c") {
        let Some(mid_rel) = rest[start..].find("::") else { break };
        let Some(end_rel) = rest[start + mid_rel..].find("}}") else { break };
        let answer = rest[start + mid_rel + 2..start + mid_rel + end_rel].trim();
        out.push_str(&rest[..start]);
        out.push_str(&format!("**{answer}**"));
        rest = &rest[start + mid_rel + end_rel + 2..];
    }
    out.push_str(rest);
    out
}

fn validate_sentence(s: &str) -> Result<()> {
    if s.trim().is_empty() {
        return Err(Error::Other("遮挡卡句子不能为空".into()));
    }
    if s.chars().count() > MAX_SENTENCE {
        return Err(Error::Other("遮挡卡句子过长".into()));
    }
    let answers = extract_clozes(s);
    if answers.is_empty() {
        return Err(Error::Other("句子缺少 {{c1::答案}} 遮挡标记".into()));
    }
    if s.matches("{{c").count() != answers.len() {
        return Err(Error::Other("存在空的遮挡标记".into()));
    }
    if answers.iter().any(|a| a.chars().count() > MAX_ANSWER) {
        return Err(Error::Other("遮挡的答案过长（整句就该用问答卡）".into()));
    }
    Ok(())
}

/// Parse the model reply into validated sentences. Accepts `{"cards":[…]}` or
/// a bare array; drops (rather than fails) sentences without usable markers.
fn parse_cards(raw: &str) -> Vec<String> {
    let cleaned = strip_fences(raw);
    let value: serde_json::Value = serde_json::from_str(&cleaned).unwrap_or(serde_json::Value::Null);
    let items = match &value {
        serde_json::Value::Array(a) => a.clone(),
        v => v
            .get("cards")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default(),
    };
    let mut out = Vec::new();
    for it in items.iter().take(MAX_CARDS * 2) {
        let s = it
            .get("sentence")
            .or_else(|| it.get("front"))
            .or_else(|| it.get("q"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if s.is_empty() || out.contains(&s.to_string()) {
            continue;
        }
        if validate_sentence(s).is_ok() {
            out.push(s.to_string());
        }
        if out.len() >= MAX_CARDS {
            break;
        }
    }
    out
}

fn strip_fences(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if s.starts_with("```") {
        if let Some(end) = s.rfind("```") {
            if end > 3 {
                s = s[3..end].to_string();
            }
        }
        if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].to_string();
        }
        s = s.trim().to_string();
    }
    s
}

#[tauri::command]
pub async fn cloze_generate(
    app: tauri::AppHandle,
    source_id: String,
    count: Option<usize>,
) -> Result<ClozeResult> {
    use tauri::Manager;
    let want = count.unwrap_or(8).clamp(3, MAX_CARDS);
    let (text, source_name, subject_id) = {
        let st = app.state::<crate::AppState>();
        let c = st.db.lock().unwrap();
        let subject_id: String = c
            .query_row(
                "SELECT subject_id FROM sources WHERE id=?1",
                [&source_id],
                |r| r.get(0),
            )
            .map_err(|_| Error::Other("来源不存在".into()))?;
        let name: String = c
            .query_row("SELECT name FROM sources WHERE id=?1", [&source_id], |r| r.get(0))
            .unwrap_or_else(|_| "来源".into());
        (visuals::source_text(&c, &source_id)?, name, subject_id)
    };
    let model = {
        let st = app.state::<crate::AppState>();
        visuals::load_model(&st)?
    };
    let mut model = model;
    {
        let st = app.state::<crate::AppState>();
        let c = st.db.lock().unwrap();
        crate::commands::apply_budget(&mut model, &c, "default");
    }
    let prompt = format!(
        "从下面的学习材料里挑出适合做「遮挡自测」的句子，生成填空卡。\n\n要求：\n- 每张卡是一个完整句子，其中 1 个关键概念用 {{{{c1::答案}}}} 标记遮挡\n- 遮挡的必须是理解该句的关键（术语 / 数字 / 因果词），不要遮挡整句\n- 共 {want} 张，尽量覆盖材料的不同部分\n- 严格只输出 JSON：{{\"cards\":[{{\"sentence\":\"光合作用发生在{{{{c1::叶绿体}}}}中。\"}}]}}，不要任何额外文字\n\n材料：\n{text}"
    );
    let raw = tauri::async_runtime::spawn_blocking(move || {
        model.complete(SYS, &prompt).map_err(|e| Error::Other(e.to_string()))
    })
    .await
    .map_err(|e| Error::Other(e.to_string()))??;
    let cards = parse_cards(&raw);
    if cards.is_empty() {
        return Err(Error::Other(
            "模型没有给出可用的遮挡句（每句都需要 {{c1::答案}} 标记），换个材料或重试".into(),
        ));
    }
    let payload: serde_json::Value = serde_json::Value::Array(
        cards
            .iter()
            .map(|s| serde_json::json!({ "q": s, "a": build_back(s) }))
            .collect(),
    );
    let title = format!("遮挡卡 · {}", truncate_name(&source_name));
    let material_id = {
        let st = app.state::<crate::AppState>();
        let c = st.db.lock().unwrap();
        crate::repo::save_material(&c, &subject_id, None, "flashcards", &title, "cloze", &payload)?
    };
    Ok(ClozeResult { material_id, count: cards.len() })
}

fn truncate_name(s: &str) -> String {
    s.chars().take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_and_build_roundtrip() {
        let front = "光合作用发生在{{c1::叶绿体}}中，需要{{c2::光}}。";
        let answers = extract_clozes(front);
        assert_eq!(answers, vec!["叶绿体", "光"]);
        let back = build_back(front);
        assert_eq!(back, "光合作用发生在**叶绿体**中，需要**光**。");
    }

    #[test]
    fn extract_handles_empty_and_unbalanced() {
        // empty answer → not counted
        assert!(extract_clozes("a {{c1::}} b").is_empty());
        // unbalanced marker → stops cleanly, no panic
        assert!(extract_clozes("a {{c1::答案 b").is_empty());
        assert!(extract_clozes("no markers").is_empty());
    }

    #[test]
    fn validation_rejects_markerless_and_oversized() {
        assert!(validate_sentence("没有标记的句子").is_err());
        assert!(validate_sentence("空标记 {{c1::}} 呢").is_err());
        assert!(validate_sentence(&format!("长答案 {{c1::{}}}", "字".repeat(80))).is_err());
        assert!(validate_sentence(&format!("长句子 {{c1::答案}} {}", "字".repeat(500))).is_err());
        assert!(validate_sentence("合法的{{c1::答案}}句子").is_ok());
    }

    #[test]
    fn parse_cards_accepts_wrapper_bare_array_and_drops_invalid() {
        let wrapped = r#"{"cards":[
            {"sentence":"合法{{c1::答案}}句"},
            {"sentence":"没有标记"},
            {"sentence":"合法{{c1::答案}}句"}
        ]}"#;
        let got = parse_cards(wrapped);
        assert_eq!(got, vec!["合法{{c1::答案}}句".to_string()]); // dup dropped, invalid dropped

        let bare = r#"[{"front":" bare {{c1::x}} "}]"#;
        assert_eq!(parse_cards(bare), vec!["bare {{c1::x}}".to_string()]);

        assert!(parse_cards("not json at all").is_empty());
        assert!(parse_cards(r#"{"cards":[{"sentence":"no marker here"}]}"#).is_empty());
    }

    #[test]
    fn parse_cards_caps_batch() {
        let many: Vec<String> = (0..60)
            .map(|i| format!("{{\"sentence\":\"第{i}句 {{{{c1::答案{i}}}}} 结尾\"}}"))
            .collect();
        let raw = format!("{{\"cards\":[{}]}}", many.join(","));
        assert_eq!(parse_cards(&raw).len(), MAX_CARDS);
    }

    #[test]
    fn material_payload_roundtrips() {
        let st = crate::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "生物", None, None, None).unwrap();
        let src = crate::repo::insert_source(&c, &sid, None, "讲义", "txt", Some("t"))
            .unwrap();
        let cards = vec![
            "叶绿体是{{c1::光合作用}}的场所。".to_string(),
        ];
        let payload: serde_json::Value = serde_json::Value::Array(
            cards
                .iter()
                .map(|s| serde_json::json!({ "q": s, "a": build_back(s) }))
                .collect(),
        );
        let mid = crate::repo::save_material(&c, &sid, None, "flashcards", "遮挡卡 · 讲义", "cloze", &payload).unwrap();
        let mat = crate::repo::get_material(&c, &mid).unwrap();
        assert_eq!(mat.kind, "flashcards");
        assert_eq!(mat.meta, "cloze");
        let first = &mat.payload.as_array().unwrap()[0];
        assert_eq!(first["a"], "叶绿体是**光合作用**的场所。");
        let _ = src;
    }
}
