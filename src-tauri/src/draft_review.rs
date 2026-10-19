//! 2.1 写作升格批改 — three layers over a finished draft:
//!   1. 规则体检 (`draft_review_check`): pure-rule scan, no LLM — hedges,
//!      colloquialisms, template verbs, first-person, over-long sentences and
//!      paragraphs. Instant, free, and it anchors what the AI pass should fix.
//!   2. AI 升格 (`draft_review_upgrade`): ONE LLM call over the numbered
//!      sentences returns an overall verdict + concrete before→after
//!      suggestions the user accepts or ignores one by one.
//!   3. 逐句改写 (`draft_rewrite_sentence`): one focused call for a single
//!      sentence (paragraph context + optional user instruction), applied on
//!      accept via `draft_update`.
//! Sentences are indexed by the SAME splitter everywhere (check returns them,
//! upgrade/rewrite address them), so a click in the UI rewrites exactly the
//! sentence the user pointed at. [n] citation markers must survive every
//! rewrite — the prompts demand it and the tests pin it.

use crate::drafts::{DraftBody, DraftPara};
use crate::error::{Error, Result};
use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

// ---- text shaping -------------------------------------------------------

/// Half-open byte span of one sentence (join of spans == original text).
struct Span {
    start: usize,
    end: usize,
}

/// Split into sentences, keeping every byte (no trimming) so joining the
/// spans reproduces the paragraph exactly. Chinese terminators always split;
/// Latin `.` only splits when followed by whitespace + sentence start, so
/// "Fig. 3" and "et al." stay intact.
fn split_sentences(text: &str) -> Vec<Span> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut spans = Vec::new();
    let mut start = 0usize;
    for i in 0..chars.len() {
        let (off, ch) = chars[i];
        let is_break = matches!(ch, '。' | '！' | '？' | '；' | '!' | '?' | ';' | '\n')
            || (ch == '.' && {
                let next_ws = chars.get(i + 1).map(|(_, c)| c.is_whitespace()).unwrap_or(true);
                let after = chars
                    .get(i + 2)
                    .map(|(_, c)| c.is_uppercase() || *c == '"' || *c == '“')
                    .unwrap_or(true);
                next_ws && after
            });
        if is_break {
            let end = off + ch.len_utf8();
            spans.push(Span { start, end });
            start = end;
        }
    }
    if start < text.len() {
        spans.push(Span { start, end: text.len() });
    }
    spans
}

pub fn sentences_of(text: &str) -> Vec<String> {
    split_sentences(text)
        .iter()
        .map(|s| text[s.start..s.end].to_string())
        .collect()
}

/// zh when CJK ideographs dominate — the two prompt registers are per-language.
pub fn detect_lang(body: &DraftBody) -> &'static str {
    let all: String = body
        .sections
        .iter()
        .flat_map(|s| s.paragraphs.iter().map(|p| p.text.clone()))
        .chain(body.paragraphs.iter().map(|p| p.text.clone()))
        .collect();
    let cjk = all.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
    if cjk * 4 > all.chars().count() {
        "zh"
    } else {
        "en"
    }
}

/// One paragraph reference: `section` = None for legacy flat drafts.
fn iter_paras(body: &DraftBody) -> Vec<(Option<usize>, usize, &DraftPara)> {
    let mut out = Vec::new();
    for (si, sec) in body.sections.iter().enumerate() {
        for (pi, p) in sec.paragraphs.iter().enumerate() {
            out.push((Some(si), pi, p));
        }
    }
    for (pi, p) in body.paragraphs.iter().enumerate() {
        out.push((None, pi, p));
    }
    out
}

// ---- layer 1: rule check --------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub kind: String, // hedge | colloquial | cliche | first_person | long_sentence | long_para
    pub severity: String, // info | warn
    pub section: Option<usize>,
    pub para: usize,
    pub sentence: Option<usize>,
    pub excerpt: String,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DraftText {
    pub lang: String,
    /// section → para → sentences (mirrors payload.sections)
    pub sections: Vec<Vec<Vec<String>>>,
    /// legacy flat paragraphs → sentences
    pub flat: Vec<Vec<String>>,
    pub findings: Vec<Finding>,
}

const HEDGES_ZH: &[&str] = &["可能会", "也许", "或许", "大概", "基本上", "似乎", "某种程度上", "相对来说"];
const HEDGES_EN: &[&str] = &[
    "arguably", "somewhat", "rather ", "quite ", "very ", "really ", "basically", "actually",
    "literally", "generally ",
];
const COLLOQUIAL_ZH: &[&str] = &["很多", "差不多", "东西", "挺", " lots of", "a lot of"];
const COLLOQUIAL_EN: &[&str] = &["things", "stuff", "kind of", "sort of", "a big ", "huge "];
const CLICHE_ZH: &[&str] = &["进行了", "开展了", "做了相关"];
const CLICHE_EN: &[&str] = &["in order to", "was performed", "was carried out", "due to the fact that"];
const FIRST_PERSON_ZH: &[&str] = &["我认为", "我感觉", "我们觉得", "笔者觉得"];
const FIRST_PERSON_EN: &[&str] = &["i think", "i believe", "i feel", "we feel"];

fn matcher(patterns: &'static [&'static str], lower: bool) -> impl Fn(&str) -> bool {
    move |text: &str| {
        let hay = if lower { text.to_lowercase() } else { text.to_string() };
        patterns.iter().any(|p| hay.contains(p))
    }
}

fn rule_check(body: &DraftBody) -> Vec<Finding> {
    let lang = detect_lang(body);
    let mut out = Vec::new();
    let rules: Vec<(&str, &str, &str, Box<dyn Fn(&str) -> bool>)> = if lang == "zh" {
        vec![
            ("hedge", "info", "模糊限定语削弱论证力度，能删则删或改用确切表述", Box::new(matcher(HEDGES_ZH, false))),
            ("colloquial", "info", "口语化表达，换用书面语", Box::new(matcher(COLLOQUIAL_ZH, false))),
            ("cliche", "info", "模板化动词（进行了/开展了）→ 直接用具体动词", Box::new(matcher(CLICHE_ZH, false))),
            ("first_person", "warn", "学术写作避免「我认为」式主观表述，改陈述事实或引用", Box::new(matcher(FIRST_PERSON_ZH, false))),
        ]
    } else {
        vec![
            ("hedge", "info", "Hedging fillers weaken the claim — cut or state precisely", Box::new(matcher(HEDGES_EN, true))),
            ("colloquial", "info", "Colloquial wording — use the formal register", Box::new(matcher(COLLOQUIAL_EN, true))),
            ("cliche", "info", "Template phrase (in order to / was performed) — use the direct verb", Box::new(matcher(CLICHE_EN, true))),
            ("first_person", "warn", "Avoid subjective first-person framing; state the fact or cite", Box::new(matcher(FIRST_PERSON_EN, true))),
        ]
    };
    for (section, para, p) in iter_paras(body) {
        let sents = sentences_of(&p.text);
        // long paragraph
        let plen = p.text.chars().count();
        let pwords = p.text.split_whitespace().count();
        if (lang == "zh" && plen > 500) || (lang == "en" && pwords > 200) {
            out.push(Finding {
                kind: "long_para".into(),
                severity: "warn".into(),
                section,
                para,
                sentence: None,
                excerpt: p.text.chars().take(60).collect(),
                hint: if lang == "zh" { "段落过长，考虑拆分".into() } else { "Paragraph too long — consider splitting".into() },
            });
        }
        for (ki, s) in sents.iter().enumerate() {
            let chars = s.chars().count();
            let words = s.split_whitespace().count();
            if (lang == "zh" && chars > 90) || (lang == "en" && words > 45) {
                out.push(Finding {
                    kind: "long_sentence".into(),
                    severity: "warn".into(),
                    section,
                    para,
                    sentence: Some(ki),
                    excerpt: s.trim().to_string(),
                    hint: if lang == "zh" { "句子过长，建议拆成两句".into() } else { "Sentence too long — split it".into() },
                });
                // pattern rules still apply — a long sentence can also hedge
            }
            for (kind, severity, hint, f) in &rules {
                if f(s) {
                    out.push(Finding {
                        kind: (*kind).into(),
                        severity: (*severity).into(),
                        section,
                        para,
                        sentence: Some(ki),
                        excerpt: s.trim().to_string(),
                        hint: (*hint).into(),
                    });
                }
            }
        }
    }
    out
}

/// 规则体检 — pure, synchronous, free.
#[tauri::command]
pub fn draft_review_check(state: State<'_, crate::db::AppState>, draft_id: String) -> Result<DraftText> {
    let c = state.db.lock().unwrap();
    let draft = crate::drafts::get(&c, &draft_id)?;
    let payload = draft
        .payload
        .ok_or_else(|| Error::Other("该成稿没有正文".into()))?;
    let body: DraftBody = serde_json::from_value(payload)
        .map_err(|_| Error::Other("成稿结构无法解析".into()))?;
    let sections = body
        .sections
        .iter()
        .map(|s| s.paragraphs.iter().map(|p| sentences_of(&p.text)).collect())
        .collect();
    let flat = body.paragraphs.iter().map(|p| sentences_of(&p.text)).collect();
    Ok(DraftText {
        lang: detect_lang(&body).into(),
        sections,
        flat,
        findings: rule_check(&body),
    })
}

// ---- shared LLM setup -----------------------------------------------------

fn style_line(lang: &str) -> &'static str {
    if lang == "zh" {
        "目标文体：中文学术论文体——书面、客观、精炼；不用口语词、模糊限定语和「进行了/开展了」式模板动词；保留专业术语与数字。"
    } else {
        "Target register: English academic prose — formal, objective, concise; no colloquialisms, hedging fillers, or template verbs (in order to / was performed); keep technical terms and numbers."
    }
}

const CITE_RULE: &str =
    " If the sentence contains [n] citation markers, they MUST appear in your rewrite unchanged — same numbers, and keep at least one marker per cited source.";

fn load_body(conn: &Connection, draft_id: &str) -> Result<(DraftBody, String)> {
    let draft = crate::drafts::get(conn, draft_id)?;
    let payload = draft
        .payload
        .ok_or_else(|| Error::Other("该成稿没有正文".into()))?;
    let body: DraftBody = serde_json::from_value(payload)
        .map_err(|_| Error::Other("成稿结构无法解析".into()))?;
    Ok((body, draft.title))
}

fn model_for(conn: &Connection) -> Result<Box<dyn crate::llm::Llm>> {
    let spec = crate::repo::get_setting(conn, "model_cheatsheet")?
        .unwrap_or_else(|| "openrouter:deepseek/deepseek-v4-flash".into());
    crate::commands::guard_offline_llm(conn, &spec)?;
    let keys = crate::commands::read_keys(conn)?;
    let mut model = crate::llm::from_spec_or_any(&spec, &keys)
        .ok_or_else(|| Error::Other("No model configured — add an API key in Settings".into()))?;
    crate::commands::apply_budget(&mut model, conn, "cheatsheet");
    Ok(model)
}

/// Address label for one sentence, e.g. `S0.P2.3` / `P1.0` — the model echoes
/// it and we treat it as authoritative placement.
fn address(section: Option<usize>, para: usize, sentence: usize) -> String {
    match section {
        Some(s) => format!("S{s}.P{para}.{sentence}"),
        None => format!("P{para}.{sentence}"),
    }
}

/// Numbered full text of the body, one sentence per line.
fn numbered_sentences(body: &DraftBody) -> String {
    let mut out = String::new();
    if !body.thesis.is_empty() {
        out.push_str(&format!("THESIS: {}\n\n", body.thesis));
    }
    for (section, para, p) in iter_paras(body) {
        if let Some(si) = section {
            out.push_str(&format!("[section {}: {}]\n", si, body.sections[si].heading));
        }
        for (ki, s) in sentences_of(&p.text).iter().enumerate() {
            out.push_str(&format!("{} {}\n", address(section, para, ki), s.trim()));
        }
    }
    out
}

fn resolve_address(
    body: &DraftBody,
    section: Option<usize>,
    para: usize,
    sentence: usize,
) -> Result<&DraftPara> {
    let para_ref = match section {
        Some(si) => body
            .sections
            .get(si)
            .and_then(|s| s.paragraphs.get(para)),
        None => body.paragraphs.get(para),
    };
    let p = para_ref.ok_or_else(|| Error::Other("建议指向的段落不存在，已跳过".into()))?;
    let n = sentences_of(&p.text).len();
    if sentence >= n {
        return Err(Error::Other("建议指向的句子不存在，已跳过".into()));
    }
    Ok(p)
}

// ---- layer 2: AI upgrade ---------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Upgrade {
    pub section: Option<usize>,
    pub para: usize,
    pub sentence: usize,
    pub original: String,
    pub revised: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpgradeReport {
    pub lang: String,
    pub overall: String,
    pub suggestions: Vec<Upgrade>,
}

/// AI 升格 — one call, whole draft, concrete before→after suggestions.
pub fn upgrade_draft(app: &AppHandle, draft_id: &str) -> Result<UpgradeReport> {
    let state = app.state::<crate::db::AppState>();
    let (body, title, mut model) = {
        let c = state.db.lock().unwrap();
        let (body, title) = load_body(&c, draft_id)?;
        let model = model_for(&c)?;
        (body, title, model)
    };
    let lang = detect_lang(&body);
    let n_sents = numbered_sentences(&body).lines().count();
    let system = format!(
        "You are an academic writing coach performing a rigorous upgrade pass on the draft \"{title}\". \
        You get the draft's sentences, each prefixed with an address like S0.P1.3 (section 0, paragraph 1, \
        sentence 3) or P1.0 (legacy flat paragraph). Identify the sentences that most weaken the paper — \
        vague hedging, colloquial register, template verbs, subjective framing, wordiness, faulty flow — \
        and output a CONCRETE rewrite for each, in the draft's language. {style}\
        Also give a 2-4 sentence overall verdict in the draft's language.\
        Output ONLY a JSON object of the exact shape {{\"overall\":\"...\",\"suggestions\":[{{\"address\":\"...\",\"revised\":\"...\",\"reason\":\"...\"}}]}} \
        — no markdown, no commentary. Prefer 5-15 high-value suggestions over line-by-line nitpicks; never rewrite a sentence that is already fine.{CITE_RULE}",
        title = title,
        style = style_line(lang),
    );
    let user = numbered_sentences(&body);
    model.set_max_tokens((2048 + 40 * n_sents as u32).min(6000));
    let raw = model.complete(&system, &user)?;
    {
        let c = state.db.lock().unwrap();
        crate::tokens::record(&c, "draft_review", &model.name(), &format!("{system}\n{user}"), &raw, None);
    }
    let parsed = crate::llm::extract_json(&raw)
        .map_err(|_| Error::Other("模型输出无法解析；请重试".into()))?;
    let overall = parsed
        .get("overall")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let mut suggestions = Vec::new();
    if let Some(list) = parsed.get("suggestions").and_then(|v| v.as_array()) {
        for s in list {
            let addr = s.get("address").and_then(|v| v.as_str()).unwrap_or_default();
            let revised = s
                .get("revised")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or_default();
            let reason = s
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // parse S{si}.P{pi}.{ki} | P{pi}.{ki}
            let (section, para, sentence) = parse_address(addr);
            if revised.is_empty() {
                continue;
            }
            // placement is authoritative (same splitter); drop out-of-range items
            let original = match resolve_address(&body, section, para, sentence) {
                Ok(p) => sentences_of(&p.text)[sentence].trim().to_string(),
                Err(_) => continue,
            };
            if original == revised {
                continue;
            }
            suggestions.push(Upgrade { section, para, sentence, original, revised: revised.to_string(), reason });
        }
    }
    Ok(UpgradeReport { lang: lang.into(), overall, suggestions })
}

fn parse_address(addr: &str) -> (Option<usize>, usize, usize) {
    let nums: Vec<usize> = addr
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    match nums.as_slice() {
        [s, p, k] => (Some(*s), *p, *k),
        [p, k] => (None, *p, *k),
        _ => (None, usize::MAX, usize::MAX), // unparseable → dropped by resolve
    }
}

#[tauri::command]
pub async fn draft_review_upgrade(app: AppHandle, draft_id: String) -> Result<UpgradeReport> {
    tauri::async_runtime::spawn_blocking(move || upgrade_draft(&app, &draft_id))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

// ---- layer 3: single-sentence rewrite --------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SentenceRewrite {
    pub original: String,
    pub revised: String,
    pub reason: String,
}

/// 逐句改写 — one focused call: paragraph context + the single sentence +
/// optional user instruction. Nothing is persisted; the UI applies on accept.
pub fn rewrite_sentence(
    app: &AppHandle,
    draft_id: &str,
    section: Option<usize>,
    para: usize,
    sentence: usize,
    instruction: Option<&str>,
) -> Result<SentenceRewrite> {
    let state = app.state::<crate::db::AppState>();
    let (body, _title, mut model) = {
        let c = state.db.lock().unwrap();
        let (body, title) = load_body(&c, draft_id)?;
        let model = model_for(&c)?;
        (body, title, model)
    };
    let lang = detect_lang(&body);
    let p = resolve_address(&body, section, para, sentence)?;
    let sents = sentences_of(&p.text);
    let original = sents[sentence].trim().to_string();
    if original.is_empty() {
        return Err(Error::Other("目标句子为空".into()));
    }
    let focus = instruction
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| format!("\n\nUSER INSTRUCTION (obey it):\n{s}"))
        .unwrap_or_default();
    let system = format!(
        "You are an academic writing coach. Rewrite EXACTLY ONE sentence from a draft. \
        You get the thesis, the full paragraph for context, and the sentence marked <<<>>>. \
        Improve only that sentence: keep its meaning and every fact, fix register, wordiness, \
        vague hedging or template verbs. Keep it a similar length (±30%). {}{CITE_RULE}\
        Output ONLY a JSON object of the exact shape {{\"revised\":\"...\",\"reason\":\"...\"}} (reason: one short line, in the draft's language) — no markdown.",
        style_line(lang),
    );
    let context_para: String = sents
        .iter()
        .enumerate()
        .map(|(i, s)| if i == sentence { format!("<<<>>> {}", s.trim()) } else { s.trim().to_string() })
        .collect::<Vec<_>>()
        .join("");
    let mut user = String::new();
    if !body.thesis.is_empty() {
        user.push_str(&format!("THESIS: {}\n\n", body.thesis));
    }
    if let Some(si) = section {
        user.push_str(&format!("SECTION: {}\n\n", body.sections[si].heading));
    }
    user.push_str(&format!("PARAGRAPH: {context_para}{focus}"));
    model.set_max_tokens(1024);
    let raw = model.complete(&system, &user)?;
    {
        let c = state.db.lock().unwrap();
        crate::tokens::record(&c, "draft_review", &model.name(), &format!("{system}\n{user}"), &raw, None);
    }
    let parsed = crate::llm::extract_json(&raw)
        .map_err(|_| Error::Other("模型输出无法解析；请重试".into()))?;
    let revised = parsed
        .get("revised")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if revised.is_empty() {
        return Err(Error::Other("模型未给出改写结果；请重试".into()));
    }
    let reason = parsed
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(SentenceRewrite { original, revised, reason })
}

#[tauri::command]
pub async fn draft_rewrite_sentence(
    app: AppHandle,
    draft_id: String,
    section: Option<usize>,
    para: usize,
    sentence: usize,
    instruction: Option<String>,
) -> Result<SentenceRewrite> {
    tauri::async_runtime::spawn_blocking(move || {
        rewrite_sentence(&app, &draft_id, section, para, sentence, instruction.as_deref())
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

// ---- write-back -------------------------------------------------------------

/// Persist an edited payload (accepted rewrites patch sentences in place).
#[tauri::command]
pub fn draft_update(
    state: State<crate::db::AppState>,
    draft_id: String,
    payload: serde_json::Value,
) -> Result<()> {
    let c = state.db.lock().unwrap();
    crate::drafts::update_payload(&c, &draft_id, &payload)
}

// ---- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn body_zh() -> DraftBody {
        DraftBody {
            thesis: "本文研究检索增强生成。".into(),
            paragraphs: vec![DraftPara {
                text: "很多研究进行了相关实验。另外，我认为这个方法基本上有效，实验结果表明它可能很好，这一点我们做了大量的验证工作，验证工作覆盖了多个数据集、多个语言、多个任务设定、多种评测口径以及多种实现细节的消融分析，从而确保结论的稳健性与可复现性。".into(),
                cites: vec![1],
            }],
            sections: vec![],
        }
    }

    #[test]
    fn splitter_roundtrips_and_handles_abbreviations() {
        let t = "如图 1 所示。第二句！第三句？Fig. 3 shows it. And et al. did more.";
        let sents = sentences_of(t);
        assert_eq!(sents.join(""), t); // lossless
        assert!(sents[0].ends_with('。'));
        // "Fig. 3" and "et al." must NOT split on the period
        let en = sentences_of("Fig. 3 shows it. And et al. did more.");
        assert_eq!(en.len(), 2);
        assert!(en[1].starts_with(" And"));
    }

    #[test]
    fn rules_flag_zh_issues() {
        let findings = rule_check(&body_zh());
        let kinds: Vec<&str> = findings.iter().map(|f| f.kind.as_str()).collect();
        assert!(kinds.contains(&"colloquial")); // 很多
        assert!(kinds.contains(&"cliche")); // 进行了
        assert!(kinds.contains(&"first_person")); // 我认为
        assert!(kinds.contains(&"hedge")); // 基本上 / 可能
        assert!(kinds.contains(&"long_sentence")); // the 100+ char sentence
    }

    #[test]
    fn lang_detection_chooses_by_cjk_share() {
        assert_eq!(detect_lang(&body_zh()), "zh");
        let en = DraftBody {
            thesis: "We study retrieval-augmented generation.".into(),
            paragraphs: vec![DraftPara {
                text: "The results were performed well. In order to verify, we ran a lot of things.".into(),
                cites: vec![],
            }],
            sections: vec![],
        };
        assert_eq!(detect_lang(&en), "en");
        let f = rule_check(&en);
        assert!(f.iter().any(|x| x.kind == "cliche")); // "in order to"
    }

    #[test]
    fn addresses_parse_roundtrip() {
        assert_eq!(parse_address("S2.P1.4"), (Some(2), 1, 4));
        assert_eq!(parse_address("P0.3"), (None, 0, 3));
        assert_eq!(parse_address("junk"), (None, usize::MAX, usize::MAX));
        assert_eq!(address(Some(2), 1, 4), "S2.P1.4");
    }

    #[test]
    fn resolve_address_bounds_checks() {
        let b = body_zh();
        assert!(resolve_address(&b, None, 0, 0).is_ok());
        assert!(resolve_address(&b, None, 1, 0).is_err()); // no such para
        assert!(resolve_address(&b, None, 0, 99).is_err()); // no such sentence
        assert!(resolve_address(&b, Some(0), 0, 0).is_err()); // no sections
    }

    #[test]
    fn update_payload_validates_and_persists() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let payload = serde_json::json!({
            "thesis": "t",
            "paragraphs": [{"text": "句子。", "cites": []}],
            "references": []
        });
        let id = crate::drafts::save(&c, "T", &[], "", &payload).unwrap();
        // valid edit persists
        let mut edited = payload.clone();
        edited["paragraphs"][0]["text"] = "改好的句子。".into();
        update_payload_ok(&c, &id, &edited);
        let d = crate::drafts::get(&c, &id).unwrap();
        assert_eq!(d.payload.unwrap()["paragraphs"][0]["text"], "改好的句子。");
        // structurally-broken payload is rejected
        assert!(crate::drafts::update_payload(&c, &id, &serde_json::json!({"foo": 1})).is_err());
    }

    fn update_payload_ok(c: &Connection, id: &str, payload: &serde_json::Value) {
        crate::drafts::update_payload(c, id, payload).unwrap();
    }
}
