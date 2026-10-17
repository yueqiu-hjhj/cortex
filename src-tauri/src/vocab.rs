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

use crate::error::Result;
use serde::Serialize;
use rusqlite::{params, Connection};

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
}
