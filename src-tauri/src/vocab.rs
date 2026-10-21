//! Vocabulary workbench: per-subject, corpus-aware term dictionary.
//!
//! Distinct from the localStorage-backed flashcard glossary in
//! src/lib/glossary.ts. Backed by the `vocab_terms` SQLite table (migration
//! 0032). Each row is a (subject, normalized_term) pair with optional
//! definition, examples, corpus frequency, and a 3-state mastery status.
//!
//! Term extraction is intentionally simple: tokenize the source text, drop
//! stop words and short tokens, count case-insensitive frequencies, and
//! return the top N. No POS tagger, no lemmatizer — we just want a useful
//! "what English words keep showing up in this subject" list to seed manual
//! curation. The user adds definitions, grades themselves, and the workbench
//! shows frequency + in-context examples.

use crate::db::AppState;
use crate::error::Result;
use crate::repo;
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection};
use tauri::Manager;

#[derive(Debug, Clone, Serialize)]
pub struct VocabTerm {
    pub id: String,
    pub subject_id: String,
    pub term: String,
    pub normalized: String,
    pub definition: Option<String>,
    pub examples: Option<String>,
    pub freq: i64,
    pub mastery: f64,
    /// "new" | "learning" | "known"
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractedTerm {
    pub term: String,
    pub normalized: String,
    pub freq: i64,
}

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can",
    "had", "her", "was", "one", "our", "out", "day", "get", "has", "him",
    "his", "how", "man", "new", "now", "old", "see", "two", "way", "who",
    "boy", "did", "its", "let", "put", "say", "she", "too", "use",
    "this", "that", "with", "have", "from", "they", "been", "said",
    "each", "which", "their", "will", "other", "about", "many", "then",
    "them", "some", "her", "would", "make", "like", "him", "into",
    "time", "very", "when", "come", "may", "such", "more", "your",
    "what", "than", "were", "could", "should", "these", "those", "also",
    "only", "over", "just", "even", "most", "much", "such",
    "into", "through", "during", "before", "after", "above", "below",
    "between", "under", "again", "further", "once", "here", "there",
];

fn is_word(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '\''
}

fn normalize(term: &str) -> String {
    term.trim().to_lowercase()
}

/// Tokenize a body of text into candidate English terms. Returns a vec of
/// (normalized_term, original_token) pairs to preserve original casing for
/// display. Filters out stop words, pure-numeric tokens, and short (< 3)
/// tokens. Single tokens only — multi-word phrase extraction is a future
/// improvement (it needs POS tagging, which is out of scope here).
pub fn extract_tokens(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut Vec<(String, String)>| {
        if buf.is_empty() { return; }
        let original = buf.clone();
        buf.clear();
        let norm = original.to_lowercase();
        if norm.len() < 3 { return; }
        if STOP_WORDS.contains(&norm.as_str()) { return; }
        // Skip pure-numeric / all-punct tokens.
        if !norm.chars().any(|c| c.is_alphabetic()) { return; }
        out.push((norm, original));
    };
    for c in text.chars() {
        if is_word(c) {
            buf.push(c);
        } else {
            flush(&mut buf, &mut out);
        }
    }
    flush(&mut buf, &mut out);
    out
}

/// Count term frequencies in a body of text and return the top N. Tie-break
/// alphabetically so the output is deterministic across runs.
pub fn top_terms(text: &str, n: usize) -> Vec<ExtractedTerm> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, (i64, String)> = HashMap::new();
    for (norm, original) in extract_tokens(text) {
        let entry = counts.entry(norm).or_insert((0, original));
        entry.0 += 1;
    }
    let mut v: Vec<(String, i64, String)> = counts
        .into_iter()
        .map(|(k, (c, o))| (k, c, o))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter()
        .take(n)
        .map(|(normalized, freq, term)| ExtractedTerm { term, normalized, freq })
        .collect()
}

/// Pull the source text for a subject (chunks joined by blank line). Used by
/// the extractor when the user clicks "extract from this subject".
pub fn subject_corpus(conn: &Connection, subject_id: &str) -> Result<String> {
    let mut stmt = conn.prepare("SELECT content FROM sources WHERE subject_id=?1 AND content IS NOT NULL ORDER BY id")?;
    let sources = stmt.query_map([subject_id], |r| r.get::<_, String>(0))?;
    let mut out = String::new();
    for s in sources {
        let content = s?;
        {
            if !content.is_empty() {
                if !out.is_empty() { out.push_str("\n\n"); }
                out.push_str(&content);
            }
        }
    }
    Ok(out)
}

/// Insert-or-bump a term for a subject. Returns the row id. If the term
/// already exists, its freq is incremented and the updated_at is bumped;
/// status/mastery/definition are not changed.
pub fn upsert_term(
    conn: &Connection,
    subject_id: &str,
    term: &str,
    bump_freq: i64,
) -> Result<String> {
    let norm = normalize(term);
    if norm.is_empty() {
        return Err(crate::error::Error::Other("empty term".into()));
    }
    let now = crate::db::now_ms();
    let id = crate::db::new_id();
    // Try to fetch existing.
    let existing: Option<(String, i64)> = conn
        .query_row(
            "SELECT id, freq FROM vocab_terms WHERE subject_id=?1 AND normalized=?2",
            params![subject_id, norm],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    match existing {
        Some((id, freq)) => {
            conn.execute(
                "UPDATE vocab_terms SET freq=?1, updated_at=?2 WHERE id=?3",
                params![freq + bump_freq, now, id],
            )?;
            Ok(id)
        }
        None => {
            conn.execute(
                "INSERT INTO vocab_terms (id, subject_id, term, normalized, freq, status, mastery, created_at, updated_at)                  VALUES (?1, ?2, ?3, ?4, ?5, 'new', 0.0, ?6, ?6)",
                params![id, subject_id, term.trim(), norm, bump_freq, now],
            )?;
            Ok(id)
        }
    }
}

pub fn list_terms(conn: &Connection, subject_id: &str) -> Result<Vec<VocabTerm>> {
    let mut st = conn.prepare(
        "SELECT id, subject_id, term, normalized, definition, examples, freq, mastery, status, created_at, updated_at          FROM vocab_terms WHERE subject_id=?1 ORDER BY freq DESC, term ASC"
    )?;
    let rows = st
        .query_map(params![subject_id], |r| {
            Ok(VocabTerm {
                id: r.get(0)?,
                subject_id: r.get(1)?,
                term: r.get(2)?,
                normalized: r.get(3)?,
                definition: r.get(4)?,
                examples: r.get(5)?,
                freq: r.get(6)?,
                mastery: r.get(7)?,
                status: r.get(8)?,
                created_at: r.get(9)?,
                updated_at: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn update_term(
    conn: &Connection,
    id: &str,
    definition: Option<&str>,
    examples: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    if status.is_some_and(|s| !matches!(s, "new" | "learning" | "known")) {
        return Err(crate::error::Error::Other("invalid vocabulary status".into()));
    }
    let tx = conn.unchecked_transaction()?;
    let conn = &tx;
    let now = crate::db::now_ms();
    // Only update fields that were supplied.
    if let Some(d) = definition {
        conn.execute("UPDATE vocab_terms SET definition=?1, updated_at=?2 WHERE id=?3",
            params![d, now, id])?;
    }
    if let Some(e) = examples {
        conn.execute("UPDATE vocab_terms SET examples=?1, updated_at=?2 WHERE id=?3",
            params![e, now, id])?;
    }
    if let Some(s) = status {
        let mastery = match s {
            "known" => 1.0,
            "learning" => 0.5,
            _ => 0.0,
        };
        conn.execute("UPDATE vocab_terms SET status=?1, mastery=?2, updated_at=?3 WHERE id=?4",
            params![s, mastery, now, id])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn delete_term(conn: &Connection, id: &str) -> Result<String> {
    let tx = conn.unchecked_transaction()?;
    let rows = crate::repo::snapshot_rows(&tx, "vocab_terms", "WHERE id=?1", id)?;
    let row = rows.first().ok_or_else(|| crate::error::Error::NotFound(format!("term {id}")))?;
    let label = row["term"].as_str().unwrap_or("term").to_string();
    let undo = crate::repo::record_undo_row(&tx, "delete_vocab", "vocab", id, &label,
        serde_json::json!({"vocab_terms": rows}))?;
    tx.execute("DELETE FROM vocab_terms WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(undo)
}

/// Extract terms from a subject's corpus and upsert them all (default top 200).
/// Returns the number of terms upserted. Existing frequencies are replaced by the current corpus count.
pub fn extract_and_upsert(
    conn: &Connection,
    subject_id: &str,
    limit: usize,
) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let conn = &tx;
    let text = subject_corpus(conn, subject_id)?;
    let top = top_terms(&text, limit.clamp(1, 1000));
    conn.execute("UPDATE vocab_terms SET freq=0 WHERE subject_id=?1", [subject_id])?;
    let mut count = 0usize;
    for t in &top {
        let id = upsert_term(conn, subject_id, &t.term, 0)?;
        let snippets = text.lines().filter(|line| extract_tokens(line).iter().any(|(n, _)| n == &t.normalized))
            .take(3).map(|line| line.chars().take(400).collect::<String>()).collect::<Vec<_>>().join("\n");
        conn.execute("UPDATE vocab_terms SET freq=?1, examples=CASE WHEN examples IS NULL THEN ?2 ELSE examples END WHERE id=?3",
            params![t.freq, snippets, id])?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
}

// ---- corpus mining (AI-assisted) -------------------------------------------
//
// The frequency extractor above answers "what words keep showing up". Mining
// answers the harder question: "what is worth learning here". The model picks
// the words and writes the definitions — but the SENTENCE is checked against
// the material verbatim, because a fabricated example sentence is the one
// failure that would make the whole feature lie to you. Saved words become
// ordinary flashcards (same material + srs_cards path as clip → card), so
// they enter the normal review rotation instead of a second schedule.

use crate::llm::Llm;

/// System prompt for corpus mining. Registered in `prompts`.
pub const MINE_SYS: &str = r#"你是语料采词员。给你一段用户自己的学习材料（以英文为主），挑出值得学的词。

规则：
1. 挑 5–15 个词：学术词、技术词优先；太常见的词、停用词、纯专有名词不要（领域术语除外）
2. 每个词输出：word（材料里的原文形式）、definition（中文释义，一句话）、sentence——**材料里包含这个词的原句**，一字不改地照抄，不许改写、不许翻译、不许拼接
3. 只输出 JSON：{"words":[{"word":"…","definition":"…","sentence":"…"}]}
4. 没有值得学的词就返回 {"words":[]}"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MineCandidate {
    pub word: String,
    pub normalized: String,
    pub definition: String,
    pub sentence: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MinedOutcome {
    pub saved: usize,
    pub seeded: usize,
}

/// What mining produced, including how many candidates the verbatim check
/// rejected — a model that fabricates example sentences should be visible,
/// not silently filtered.
#[derive(Debug, Clone, Serialize)]
pub struct MineOutcome {
    pub candidates: Vec<MineCandidate>,
    pub dropped: usize,
}

/// Mine a body of text. Returns the kept candidates plus how many were
/// dropped — a drop count above zero means the model fabricated sentences,
/// which is worth seeing rather than hiding.
pub fn mine(model: &dyn Llm, text: &str) -> Result<(Vec<MineCandidate>, usize)> {
    if text.trim().chars().count() < 40 {
        return Err(crate::error::Error::Other("材料太短，采不出东西 —— 至少给一段话".into()));
    }
    let excerpt: String = text.chars().take(6000).collect();
    let raw = model.complete(
        &crate::prompts::text(crate::prompts::VOCAB_MINE),
        &format!("材料：\n{excerpt}\n\n返回 JSON。"),
    )?;
    let parsed = parse_candidates(&raw);
    let mut kept: Vec<MineCandidate> = Vec::new();
    let mut dropped = 0usize;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for c in parsed {
        let word = c.word.trim();
        let sentence = c.sentence.trim();
        let definition = c.definition.trim();
        if word.is_empty() || definition.is_empty() || sentence.is_empty() {
            dropped += 1;
            continue;
        }
        // The whole point of corpus mining: the example must be from YOUR
        // material, verbatim. Anything else is a hallucinated example.
        if !text.contains(sentence) {
            dropped += 1;
            continue;
        }
        let norm = normalize(word);
        if !seen.insert(norm.clone()) {
            dropped += 1;
            continue;
        }
        kept.push(MineCandidate {
            word: word.to_string(),
            normalized: norm,
            definition: definition.to_string(),
            sentence: sentence.to_string(),
        });
    }
    Ok((kept, dropped))
}

/// Lenient JSON pull: models wrap JSON in fences or chatter around it, so
/// take the outermost braces and let serde complain if it isn't JSON.
pub fn parse_candidates(raw: &str) -> Vec<MineCandidate> {
    let trimmed = raw.trim();
    let start = match trimmed.find('{') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let end = match trimmed.rfind('}') {
        Some(i) => i,
        None => return Vec::new(),
    };
    if end <= start {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&trimmed[start..=end]) else {
        return Vec::new();
    };
    let Some(words) = value.get("words").and_then(|w| w.as_array()) else {
        return Vec::new();
    };
    words
        .iter()
        .filter_map(|w| {
            Some(MineCandidate {
                word: w.get("word")?.as_str()?.to_string(),
                normalized: String::new(),
                definition: w.get("definition")?.as_str()?.to_string(),
                sentence: w.get("sentence")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// The one per-subject flashcards material that carries mined words, so the
/// review UI can resolve backs (it searches all flashcard materials for the
/// front). Found by the meta marker, not by title — titles are editable.
fn vocab_material(conn: &Connection, subject_id: &str) -> Result<String> {
    if let Some(m) = repo::list_materials(conn, subject_id)?
        .into_iter()
        .find(|m| m.kind == "flashcards" && m.meta.contains("\"vocab\""))
    {
        return Ok(m.id);
    }
    repo::save_material(
        conn,
        subject_id,
        None,
        "flashcards",
        "词汇本",
        "{\"vocab\":true}",
        &serde_json::json!([]),
    )
}

/// Save mined words: upsert the term (definition/examples only when the user
/// hasn't curated them), append to the vocab material's payload, and seed the
/// SRS schedule. Idempotent — a second save of the same word adds nothing.
pub fn save_mined(
    conn: &Connection,
    subject_id: &str,
    candidates: &[MineCandidate],
) -> Result<MinedOutcome> {
    if candidates.is_empty() {
        return Err(crate::error::Error::Other("没有可保存的词".into()));
    }
    let material_id = vocab_material(conn, subject_id)?;
    let mut payload = repo::get_material(conn, &material_id)?.payload;
    let arr = match payload.as_array_mut() {
        Some(a) => a,
        None => {
            payload = serde_json::json!([]);
            payload.as_array_mut().unwrap()
        }
    };
    let mut saved = 0usize;
    let mut fronts: Vec<(i64, String)> = Vec::new();
    for c in candidates {
        let id = upsert_term(conn, subject_id, &c.word, 1)?;
        conn.execute(
            "UPDATE vocab_terms
                SET definition=CASE WHEN definition IS NULL OR definition='' THEN ?1 ELSE definition END,
                    examples=CASE WHEN examples IS NULL OR examples='' THEN ?2 ELSE examples END
              WHERE id=?3",
            params![c.definition, c.sentence, id],
        )?;
        let back = format!("{}\n\n例句：{}", c.definition, c.sentence);
        if arr.iter().any(|p| p.get("q").and_then(|q| q.as_str()) == Some(c.word.as_str())) {
            continue; // already in the deck — curation in the material wins
        }
        fronts.push((arr.len() as i64, c.word.clone()));
        arr.push(serde_json::json!({ "q": c.word, "a": back }));
        saved += 1;
    }
    conn.execute(
        "UPDATE materials SET payload=?1 WHERE id=?2",
        params![payload.to_string(), material_id],
    )?;
    let seeded = repo::srs_seed_due(conn, subject_id, &material_id, &fronts)?;
    Ok(MinedOutcome { saved, seeded })
}

#[tauri::command]
pub async fn vocab_mine(app: tauri::AppHandle, text: String) -> Result<MineOutcome> {
    tauri::async_runtime::spawn_blocking(move || -> Result<MineOutcome> {
        let state = app.state::<AppState>();
        let model = {
            let c = state.db.lock().unwrap();
            let keys = crate::commands::read_keys(&c)?;
            crate::commands::load_model(&c, &keys, "chat")?
        };
        let (candidates, dropped) = mine(model.as_ref(), &text)?;
        Ok(MineOutcome { candidates, dropped })
    })
    .await
    .map_err(|e| crate::error::Error::Other(format!("background task failed: {e}")))?
}

#[tauri::command]
pub fn vocab_save_mined(
    state: tauri::State<'_, AppState>,
    subject_id: String,
    candidates: Vec<MineCandidate>,
) -> Result<MinedOutcome> {
    let c = state.db.lock().unwrap();
    save_mined(&c, &subject_id, &candidates)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tokens_skips_stopwords_and_short() {
        let t = "The quick brown fox jumps over a lazy dog in 2024.";
        let toks: Vec<String> = extract_tokens(t).into_iter().map(|(n,_)|n).collect();
        // Stop words "the", "a", "over", "in" gone. Short < 3 gone. "2024" gone (no alpha).
        assert!(!toks.contains(&"the".to_string()));
        assert!(!toks.contains(&"in".to_string()));
        assert!(!toks.contains(&"a".to_string()));
        assert!(!toks.contains(&"2024".to_string()));
        assert!(toks.contains(&"quick".to_string()));
        assert!(toks.contains(&"brown".to_string()));
        assert!(toks.contains(&"fox".to_string()));
    }

    #[test]
    fn top_terms_sorts_by_frequency_then_alpha() {
        let t = "alpha beta alpha gamma beta alpha";
        let top = top_terms(t, 10);
        assert_eq!(top[0].normalized, "alpha");
        assert_eq!(top[0].freq, 3);
        assert_eq!(top[1].normalized, "beta");
        assert_eq!(top[1].freq, 2);
        assert_eq!(top[2].normalized, "gamma");
    }

    #[test]
    fn corpus_scan_is_idempotent_and_delete_can_be_restored() {
        let state = crate::db::AppState::in_memory().unwrap();
        let mut c = state.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "Biology", None, None, None).unwrap();
        let src = crate::repo::insert_source(&c, &sid, None, "Paper", "txt", None).unwrap();
        crate::repo::finalize_source(&c, &src, "ready", None, Some("Neuron neuron synapse. 中文不应提取"), None).unwrap();
        assert_eq!(extract_and_upsert(&c, &sid, 200).unwrap(), 2);
        let term = list_terms(&c, &sid).unwrap()[0].clone();
        assert_eq!(term.freq, 2);
        assert!(term.examples.as_deref().unwrap().contains("Neuron"));
        update_term(&c, &term.id, Some("cell"), None, Some("known")).unwrap();
        extract_and_upsert(&c, &sid, 200).unwrap();
        let again = list_terms(&c, &sid).unwrap()[0].clone();
        assert_eq!(again.freq, 2);
        assert_eq!(again.definition.as_deref(), Some("cell"));
        assert_eq!(again.status, "known");
        assert!(update_term(&c, &term.id, Some("bad"), None, Some("invalid")).is_err());
        assert_eq!(list_terms(&c, &sid).unwrap()[0].definition.as_deref(), Some("cell"));
        update_term(&c, &term.id, Some(""), Some(""), None).unwrap();
        assert_eq!(list_terms(&c, &sid).unwrap()[0].definition.as_deref(), Some(""));
        let undo = delete_term(&c, &term.id).unwrap();
        assert_eq!(list_terms(&c, &sid).unwrap().len(), 1);
        crate::repo::restore_undo(&mut c, &undo).unwrap();
        assert_eq!(list_terms(&c, &sid).unwrap().len(), 2);
        c.execute("DELETE FROM subjects WHERE id=?1", [&sid]).unwrap();
        assert!(list_terms(&c, &sid).unwrap().is_empty());
        assert!(upsert_term(&c, "missing", "term", 1).is_err());
    }

    #[test]
    fn normalize_lowercases_and_trims() {
        assert_eq!(normalize("  Hello "), "hello");
        assert_eq!(normalize("DATA"), "data");
    }

    // ---- corpus mining ------------------------------------------------------

    struct Canned(&'static str);

    impl Llm for Canned {
        fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
        fn name(&self) -> String {
            "canned".into()
        }
    }

    const MATERIAL: &str = "Spaced repetition exploits the spacing effect: distributing study over time improves long-term retention. The scheduler adjusts each interval from the learner's recall difficulty.";

    #[test]
    fn a_fabricated_sentence_is_dropped_not_saved() {
        let llm = Canned(r#"{"words":[
            {"word":"retention","definition":"保持、记忆保持","sentence":"distributing study over time improves long-term retention"},
            {"word":"interval","definition":"（时间）间隔","sentence":"The interval was set to three days by the app."}
        ]}"#);
        let (kept, dropped) = mine(&llm, MATERIAL).unwrap();
        assert_eq!(kept.len(), 1, "the verbatim sentence survives");
        assert_eq!(kept[0].normalized, "retention");
        assert_eq!(dropped, 1, "the invented sentence is dropped, and counted");
    }

    #[test]
    fn empty_definitions_and_duplicates_are_dropped() {
        let llm = Canned(r#"{"words":[
            {"word":"recall","definition":"","sentence":"from the learner's recall difficulty"},
            {"word":"recall","definition":"回忆","sentence":"from the learner's recall difficulty"},
            {"word":"spacing","definition":"间隔安排","sentence":"exploits the spacing effect"},
            {"word":"spacing","definition":"间隔效应","sentence":"exploits the spacing effect"}
        ]}"#);
        let (kept, dropped) = mine(&llm, MATERIAL).unwrap();
        assert_eq!(kept.len(), 2, "recall and spacing, once each");
        assert_eq!(dropped, 2, "one empty definition, one duplicate");
    }

    #[test]
    fn fenced_and_chattery_json_still_parses() {
        let raw = "好的，这是采词结果：\n```json\n{\"words\":[{\"word\":\"spacing\",\"definition\":\"间隔安排\",\"sentence\":\"exploits the spacing effect\"}]}\n```\n希望对你有帮助。";
        let parsed = parse_candidates(raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].word, "spacing");
        assert!(parse_candidates("没有 JSON").is_empty());
    }

    #[test]
    fn saving_seeds_a_real_review_card_with_the_example_as_back() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "English", None, None, None).unwrap();
        let candidates = vec![MineCandidate {
            word: "retention".into(),
            normalized: "retention".into(),
            definition: "记忆保持".into(),
            sentence: "distributing study over time improves long-term retention".into(),
        }];
        let out = save_mined(&c, &sid, &candidates).unwrap();
        assert_eq!(out.saved, 1);
        assert_eq!(out.seeded, 1);

        // The word is a term with its definition and the verbatim example.
        let terms = list_terms(&c, &sid).unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].definition.as_deref(), Some("记忆保持"));
        assert!(terms[0].examples.as_deref().unwrap().contains("long-term retention"));

        // …and a due SRS card whose back carries the example sentence.
        let due = crate::repo::review_today(&c, 50).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].item_key, "retention");
        let mat = crate::repo::get_material(&c, &due[0].material_id.as_deref().unwrap()).unwrap();
        let card = mat.payload.as_array().unwrap()[0].clone();
        assert_eq!(card["q"], "retention");
        assert!(card["a"].as_str().unwrap().contains("例句：distributing study over time"));
    }

    #[test]
    fn saving_twice_adds_nothing_and_keeps_curated_definitions() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = crate::repo::insert_subject(&c, "English", None, None, None).unwrap();
        let mk = |def: &str| MineCandidate {
            word: "retention".into(),
            normalized: "retention".into(),
            definition: def.into(),
            sentence: "distributing study over time improves long-term retention".into(),
        };
        save_mined(&c, &sid, &[mk("记忆保持")]).unwrap();
        // The user curates a better definition by hand…
        let term = list_terms(&c, &sid).unwrap().remove(0);
        update_term(&c, &term.id, Some("保持率（记忆研究术语）"), None, None).unwrap();
        // …a second mining run must not overwrite it.
        save_mined(&c, &sid, &[mk("记忆保持")]).unwrap();
        let again = list_terms(&c, &sid).unwrap().remove(0);
        assert_eq!(again.definition.as_deref(), Some("保持率（记忆研究术语）"));
        let out = save_mined(&c, &sid, &[mk("记忆保持")]).unwrap();
        assert_eq!(out.saved, 0, "a word already in the deck adds nothing");
        assert_eq!(out.seeded, 0);
        // One material, one card — no deck sprawl.
        let mats = crate::repo::list_materials(&c, &sid).unwrap();
        assert_eq!(mats.iter().filter(|m| m.kind == "flashcards").count(), 1);
        assert_eq!(mats[0].payload.as_array().unwrap().len(), 1);
        assert_eq!(crate::repo::review_today(&c, 50).unwrap().len(), 1);
    }

    #[test]
    fn mining_refuses_a_too_short_material() {
        let llm = Canned("{}");
        assert!(mine(&llm, "too short").is_err());
    }
}
