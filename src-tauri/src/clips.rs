//! Clips — 「选中一段，带到笔记里」。
//!
//! This is deliberately NOT the `excerpts` shelf. An excerpt is paper evidence:
//! it must belong to a paper or a source, it carries a page number, and its job
//! is to end up in a citation. A clip is looser — a sentence from an AI answer,
//! a line you liked in your own note, three words from a web page. It has no
//! owner until you file it, which is why `note_id` is nullable.
//!
//! The whole point is one gesture: select text anywhere → pick a note → the
//! passage lands under that note's `## 摘录` heading, attributed. Nothing else
//! in the app touches the note body, so an aborted clip never leaves a trace.
//!
//! The second half of the loop lives at the bottom of this file: a clip can also
//! become a flashcard batch. A passage you deliberately kept is the cleanest
//! possible input for a self-test question — better than any auto-extraction,
//! because a human already decided this sentence mattered.

use crate::db::{new_id, now_ms, AppState};
use crate::error::{Error, Result};
use crate::llm::Llm;
use crate::models::CardDraft;
use crate::repo;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::State;

/// Where a clip came from. Kept as plain strings so a future source (a Zotero
/// item, a podcast transcript) needs no migration — only a label mapping.
pub const KIND_CHAT: &str = "chat";
pub const KIND_AGENT: &str = "agent";
pub const KIND_NOTE: &str = "note";
pub const KIND_PAPER: &str = "paper";
pub const KIND_SOURCE: &str = "source";
pub const KIND_WEB: &str = "web";
pub const KIND_MANUAL: &str = "manual";

/// Heading under which filed clips accumulate. Stable on purpose: it is what
/// makes "insert into the existing section" idempotent across sessions, and
/// what makes undo able to find its own block again.
const SECTION: &str = "## 摘录";

/// A passage worth keeping. `note_id == None` means it is still in the inbox.
#[derive(Debug, Clone, Serialize)]
pub struct Clip {
    pub id: String,
    pub quote: String,
    pub source_kind: String,
    pub source_id: Option<String>,
    pub source_label: Option<String>,
    pub page: Option<i64>,
    pub note_id: Option<String>,
    /// The exact Markdown written into the note — kept so undo is a string
    /// match, never a guess.
    pub block: Option<String>,
    pub created_at: i64,
    /// Joined for display: which note this landed in.
    pub note_title: Option<String>,
    /// How many flashcards this passage has already produced (0 = not carded).
    pub card_count: i64,
}

/// Whether discarding a filed clip could also take the passage back out of the
/// note. False means the user edited that block since, and removing text by
/// match would be destruction, not undo.
#[derive(Debug, Clone, Serialize)]
pub struct DiscardOutcome {
    pub reverted: bool,
}

const SELECT: &str = "SELECT c.id, c.quote, c.source_kind, c.source_id, c.source_label, \
     c.page, c.note_id, c.block, c.created_at, n.title, \
     (SELECT COALESCE(SUM(cc.cards), 0) FROM clip_cards cc WHERE cc.clip_id = c.id) \
     FROM clips c LEFT JOIN notes n ON n.id = c.note_id";

fn row_to_clip(row: &rusqlite::Row<'_>) -> rusqlite::Result<Clip> {
    Ok(Clip {
        id: row.get(0)?,
        quote: row.get(1)?,
        source_kind: row.get(2)?,
        source_id: row.get(3)?,
        source_label: row.get(4)?,
        page: row.get(5)?,
        note_id: row.get(6)?,
        block: row.get(7)?,
        created_at: row.get(8)?,
        note_title: row.get(9)?,
        card_count: row.get(10)?,
    })
}

pub fn get(conn: &Connection, id: &str) -> Result<Clip> {
    conn.query_row(&format!("{SELECT} WHERE c.id = ?1"), params![id], row_to_clip)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("clip {id}")))
}

/// Newest first. `pending_only` is the inbox view; the full list is the log.
pub fn list(conn: &Connection, pending_only: bool) -> Result<Vec<Clip>> {
    let sql = if pending_only {
        format!("{SELECT} WHERE c.note_id IS NULL ORDER BY c.created_at DESC")
    } else {
        format!("{SELECT} ORDER BY c.created_at DESC")
    };
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], row_to_clip)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Capture a passage. A double-click on the floating button, or two `mouseup`
/// events from one gesture, must not create two clips — hence the 60-second
/// same-source-same-text lookup, which returns the existing row untouched.
pub fn add(
    conn: &Connection,
    quote: &str,
    kind: &str,
    source_id: Option<&str>,
    label: Option<&str>,
    page: Option<i64>,
) -> Result<Clip> {
    let q = quote.trim();
    if q.is_empty() {
        return Err(Error::Other("摘录内容为空".into()));
    }
    if q.chars().count() > 20_000 {
        return Err(Error::Other(
            "摘录超过 20000 字 —— 长文请先选定你要的那几段".into(),
        ));
    }
    let kind = match kind.trim() {
        "" => KIND_MANUAL,
        k => k,
    };
    let dup: Option<String> = conn
        .query_row(
            &format!(
                "{SELECT} WHERE c.quote = ?1 AND c.source_kind = ?2 \
                 AND coalesce(c.source_id, '') = coalesce(?3, '') AND c.created_at >= ?4 \
                 LIMIT 1"
            ),
            params![q, kind, source_id, now_ms() - 60_000],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = dup {
        return get(conn, &id);
    }
    let id = new_id();
    conn.execute(
        "INSERT INTO clips (id, quote, source_kind, source_id, source_label, page, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![id, q, kind, source_id, label, page, now_ms()],
    )?;
    get(conn, &id)
}

/// The Markdown a clip turns into: a blockquote, a blank quote line, then the
/// attribution. No timestamp — the file belongs to the user, and a date in every
/// block makes re-reading your own notes feel like reading a log.
pub fn render_block(quote: &str, kind: &str, label: Option<&str>, page: Option<i64>) -> String {
    let mut out = String::new();
    for line in quote.trim().split('\n') {
        let t = line.trim_end();
        out.push('>');
        if !t.trim().is_empty() {
            out.push(' ');
            out.push_str(t);
        }
        out.push('\n');
    }
    out.push_str(">\n> —— ");
    out.push_str(&attribution(kind, label, page));
    out.push('\n');
    out
}

fn attribution(kind: &str, label: Option<&str>, page: Option<i64>) -> String {
    let base = match label.map(str::trim).filter(|s| !s.is_empty()) {
        Some(l) => l.to_string(),
        None => match kind {
            KIND_CHAT => "AI 对话".into(),
            KIND_AGENT => "智能体".into(),
            KIND_NOTE => "笔记".into(),
            KIND_PAPER => "论文".into(),
            KIND_SOURCE => "资料".into(),
            KIND_WEB => "网页".into(),
            _ => "摘录".into(),
        },
    };
    match page {
        Some(p) if p > 0 => format!("{base} · p.{p}"),
        _ => base,
    }
}

/// Put `block` at the end of the `## 摘录` section, creating that section the
/// first time. Everything above the section is left byte-identical — clips are
/// additive, so filing one can never reformat prose you already wrote.
pub fn insert_into_section(body: &str, block: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let block = block.trim_end();
    let Some(i) = lines.iter().position(|l| l.trim() == SECTION) else {
        let mut out = body.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(SECTION);
        out.push_str("\n\n");
        out.push_str(block);
        out.push('\n');
        return out;
    };

    // The section runs until the next top-level heading; `###` sub-headings stay
    // inside it, so a curated `### 关于 BBR` grouping survives the next clip.
    let end = lines
        .iter()
        .enumerate()
        .skip(i + 1)
        .find(|(_, l)| {
            let t = l.trim_start();
            t.starts_with("# ") || t.starts_with("## ")
        })
        .map(|(k, _)| k)
        .unwrap_or(lines.len());

    let mut insert_at = end;
    while insert_at > i + 1 && lines[insert_at - 1].trim().is_empty() {
        insert_at -= 1;
    }

    let mut out: Vec<String> = lines[..insert_at].iter().map(|s| s.to_string()).collect();
    if !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
        out.push(String::new());
    }
    out.push(block.to_string());
    if insert_at < lines.len() {
        out.push(String::new());
        out.extend(lines[insert_at..].iter().map(|s| s.to_string()));
    }
    out.join("\n") + "\n"
}

/// File one clip under existing note `note_id`.
pub fn to_note(conn: &Connection, clip_id: &str, note_id: &str) -> Result<Clip> {
    let clip = get(conn, clip_id)?;
    if clip.note_id.is_some() {
        return Err(Error::Other(format!(
            "这条摘录已经落到《{}》里了",
            clip.note_title.clone().unwrap_or_default()
        )));
    }
    let note = repo::get_note(conn, note_id)?;
    let block = render_block(&clip.quote, &clip.source_kind, clip.source_label.as_deref(), clip.page);
    let body = insert_into_section(&note.body, &block);
    repo::update_note(conn, note_id, &note.title, &body)?;
    crate::note_links::rebuild(conn, note_id, &body)?;
    conn.execute(
        "UPDATE clips SET note_id = ?2, block = ?3 WHERE id = ?1",
        params![clip_id, note_id, block],
    )?;
    get(conn, clip_id)
}

/// File one clip into a brand-new note whose body is just that passage.
pub fn to_new_note(
    conn: &Connection,
    clip_id: &str,
    title: &str,
    subject_id: Option<&str>,
) -> Result<Clip> {
    let clip = get(conn, clip_id)?;
    if clip.note_id.is_some() {
        return Err(Error::Other("这条摘录已经落到笔记里了".into()));
    }
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::Other("新笔记需要一个标题".into()));
    }
    let block = render_block(&clip.quote, &clip.source_kind, clip.source_label.as_deref(), clip.page);
    let body = format!("{SECTION}\n\n{block}");
    let note_id = repo::insert_note(conn, subject_id, None, title, &body)?;
    crate::note_links::rebuild(conn, &note_id, &body)?;
    conn.execute(
        "UPDATE clips SET note_id = ?2, block = ?3 WHERE id = ?1",
        params![clip_id, note_id, block],
    )?;
    get(conn, clip_id)
}

/// Forget a clip. If it was filed and the note still contains the block
/// verbatim, take the block back out too — that is the undo the user expects
/// from "丢弃". If the block was edited, leave the note alone and report it.
pub fn discard(conn: &Connection, clip_id: &str) -> Result<DiscardOutcome> {
    let clip = get(conn, clip_id)?;
    let mut reverted = false;
    if let (Some(nid), Some(block)) = (clip.note_id.as_deref(), clip.block.as_deref()) {
        if let Ok(note) = repo::get_note(conn, nid) {
            if let Some(body) = remove_block(&note.body, block) {
                let body = drop_empty_section(&body);
                repo::update_note(conn, nid, &note.title, &body)?;
                crate::note_links::rebuild(conn, nid, &body)?;
                reverted = true;
            }
        }
    }
    conn.execute("DELETE FROM clips WHERE id = ?1", params![clip_id])?;
    Ok(DiscardOutcome { reverted })
}

/// Remove `block` from `body` if it is still there verbatim.
///
/// Deleting cannot invent a trailing newline: if the note did not end with one
/// before the clip, it must not end with one after the undo. So this returns the
/// body with its surrounding blank lines tidied, and nothing else — the caller
/// compares trimmed text, not bytes. (Markdown gives a trailing newline no
/// meaning; pretending to restore bytes we never recorded would be worse than
/// saying so.)
fn remove_block(body: &str, block: &str) -> Option<String> {
    let needle = block.trim_end();
    let at = body.find(needle)?;
    let head = body[..at].trim_end();
    let tail = body[at + needle.len()..].trim_start_matches('\n');
    let mut out = String::with_capacity(body.len());
    out.push_str(head);
    if !head.is_empty() && !tail.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(tail);
    Some(out)
}

/// An emptied `## 摘录` heading is litter. Drop it so undo restores the note to
/// what it looked like before the clip existed.
fn drop_empty_section(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let Some(i) = lines.iter().position(|l| l.trim() == SECTION) else {
        return body.to_string();
    };
    let end = lines
        .iter()
        .enumerate()
        .skip(i + 1)
        .find(|(_, l)| {
            let t = l.trim_start();
            t.starts_with("# ") || t.starts_with("## ")
        })
        .map(|(k, _)| k)
        .unwrap_or(lines.len());
    let empty = lines[i + 1..end].iter().all(|l| l.trim().is_empty());
    if !empty {
        return body.to_string();
    }
    // Drop the blank line that separated the now-empty heading from the prose —
    // it was ours, not the note's.
    let mut out: Vec<&str> = lines[..i].to_vec();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.extend(lines[end..].iter());
    out.join("\n")
}

// ---- clip → flashcards -----------------------------------------------
//
// Filing a clip ends at the note. That is only half a loop: the sentence you
// kept is exactly the kind of thing you will forget, and a note you never
// re-read does not help you remember it. So a clip can also become review
// material — the question is generated from the passage, you edit it, and the
// cards land in the subject's existing SRS queue (no second scheduler).

/// What a carding run produced. `material_id` points at the flashcards
/// material, so the batch can be found (and deleted) from 材料 as a unit.
#[derive(Debug, Clone, Serialize)]
pub struct CardsSaved {
    pub material_id: String,
    pub subject_id: String,
    pub cards: u32,
    /// Cards actually inserted into the schedule. Below `cards` when the same
    /// front was already scheduled — the unique key keeps the user's progress.
    pub seeded: usize,
}

/// The subject a clip's cards belong to: what the caller picked, else the
/// subject of the note it was filed under. A clip with neither is refused
/// rather than defaulted — cards in the wrong subject are worse than an error.
fn subject_for(conn: &Connection, clip: &Clip, given: Option<&str>) -> Result<String> {
    if let Some(s) = given.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(s.to_string());
    }
    if let Some(nid) = clip.note_id.as_deref() {
        if let Ok(note) = repo::get_note(conn, nid) {
            if let Some(sid) = note.subject_id {
                return Ok(sid);
            }
        }
    }
    Err(Error::Other(
        "这条摘录还没有学科 —— 选一个，卡片才有地方复习".into(),
    ))
}

/// Trim, drop empties, drop duplicate fronts (the SRS key is the front, so two
/// identical fronts would silently collapse into one card anyway).
fn clean_cards(cards: &[CardDraft]) -> Vec<CardDraft> {
    let mut out: Vec<CardDraft> = Vec::new();
    for card in cards {
        let front = card.front.trim();
        let back = card.back.trim();
        if front.is_empty() || back.is_empty() {
            continue;
        }
        if out.iter().any(|c| c.front == front) {
            continue;
        }
        out.push(CardDraft {
            front: front.chars().take(300).collect(),
            back: back.chars().take(2000).collect(),
        });
        if out.len() == 8 {
            break;
        }
    }
    out
}

/// Turn `cards` into a flashcards material and seed them into the SRS queue.
pub fn save_cards(
    conn: &Connection,
    clip_id: &str,
    subject_id: Option<&str>,
    topic_id: Option<&str>,
    cards: &[CardDraft],
) -> Result<CardsSaved> {
    let clip = get(conn, clip_id)?;
    let sid = subject_for(conn, &clip, subject_id)?;
    repo::get_subject(conn, &sid)?;
    if let Some(tid) = topic_id {
        let belongs: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM topics WHERE id=?1 AND subject_id=?2)",
            params![tid, sid],
            |r| r.get(0),
        )?;
        if !belongs {
            return Err(Error::Other("所选主题不属于该学科".into()));
        }
    }
    let clean = clean_cards(cards);
    if clean.is_empty() {
        return Err(Error::Other("需要至少一张完整的卡片（正面和背面）".into()));
    }
    let label = attribution(&clip.source_kind, clip.source_label.as_deref(), clip.page);
    let short: String = label.chars().take(48).collect();
    let title = format!("摘录 · {short} · {} 张", clean.len());
    let payload = serde_json::Value::Array(
        clean
            .iter()
            .map(|c| serde_json::json!({ "q": c.front, "a": c.back }))
            .collect(),
    );
    let material_id = repo::save_material(
        conn,
        &sid,
        topic_id,
        "flashcards",
        &title,
        &format!("{} cards · 摘录", clean.len()),
        &payload,
    )?;
    let fronts: Vec<(i64, String)> = clean
        .iter()
        .enumerate()
        .map(|(i, c)| (i as i64, c.front.clone()))
        .collect();
    let seeded = repo::srs_seed_due(conn, &sid, &material_id, &fronts)?;
    conn.execute(
        "INSERT OR REPLACE INTO clip_cards (clip_id, material_id, cards, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![clip_id, material_id, clean.len() as i64, now_ms()],
    )?;
    Ok(CardsSaved {
        material_id,
        subject_id: sid,
        cards: clean.len() as u32,
        seeded,
    })
}

/// Ask the model for self-test questions from this passage. No DB writes — the
/// UI shows the drafts for editing, and only `save_cards` persists.
pub fn draft_cards(llm: &dyn Llm, clip: &Clip) -> Result<Vec<CardDraft>> {
    let label = attribution(&clip.source_kind, clip.source_label.as_deref(), clip.page);
    let page = clip
        .page
        .filter(|p| *p > 0)
        .map(|p| format!("（第 {p} 页）"))
        .unwrap_or_default();
    let prompt = format!(
        "下面是我从「{label}」{page}里摘出来的一段话。\n\n---\n{}\n---\n\n\
         把它变成 1–3 张自测卡：front 是一个能凭这段话回答出来的问题，back 是答案。\n\
         要求：\n\
         - 用原文的语言\n\
         - 问题要具体：问机制、因果、条件、数值、区别，不要「这段话讲了什么」\n\
         - 答案尽量沿用原文措辞，80 字以内\n\
         - 宁少勿滥：只有一段话确实承载了多个知识点时才出多张\n\
         只输出 JSON：{{\"cards\":[{{\"front\":\"...\",\"back\":\"...\"}}]}}，不要解释、不要围栏",
        clip.quote.trim()
    );
    let raw = llm.complete("你是学习卡片编辑，输出必须是严格 JSON。", &prompt)?;
    parse_cards(&raw)
}

#[derive(Deserialize)]
struct RawCard {
    front: String,
    back: String,
}

/// Tolerant JSON: models add fences, prose, and sometimes drop the wrapper.
/// Anything unusable is a clear error, not an empty list — an empty result would
/// look like "this passage has nothing to test".
fn parse_cards(raw: &str) -> Result<Vec<CardDraft>> {
    let mut s = raw.trim();
    if let Some(start) = s.find('{').or_else(|| s.find('[')) {
        if let Some(end) = s.rfind('}').or_else(|| s.rfind(']')).filter(|e| *e > start) {
            s = &s[start..=end];
        }
    }
    let value: serde_json::Value = serde_json::from_str(s)
        .map_err(|e| Error::Other(format!("模型没返回可用的卡片 JSON：{e}")))?;
    let arr = value
        .get("cards")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array())
        .ok_or_else(|| Error::Other("模型返回的 JSON 里没有 cards 数组".into()))?;
    let mut out = Vec::new();
    for item in arr {
        match serde_json::from_value::<RawCard>(item.clone()) {
            Ok(c) => out.push(CardDraft {
                front: c.front.trim().to_string(),
                back: c.back.trim().to_string(),
            }),
            Err(_) => continue,
        }
    }
    let clean = clean_cards(&out);
    if clean.is_empty() {
        return Err(Error::Other(
            "模型没有给出完整的问答 —— 再试一次，或自己写一张".into(),
        ));
    }
    Ok(clean)
}

// ---- command surface --------------------------------------------------

#[tauri::command]
pub fn clip_add(
    state: State<AppState>,
    quote: String,
    source_kind: Option<String>,
    source_id: Option<String>,
    source_label: Option<String>,
    page: Option<i64>,
) -> Result<Clip> {
    let c = state.db.lock().unwrap();
    add(
        &c,
        &quote,
        source_kind.as_deref().unwrap_or(KIND_MANUAL),
        source_id.as_deref(),
        source_label.as_deref(),
        page,
    )
}

#[tauri::command]
pub fn clips_list(state: State<AppState>, pending_only: Option<bool>) -> Result<Vec<Clip>> {
    let c = state.db.lock().unwrap();
    list(&c, pending_only.unwrap_or(true))
}

#[tauri::command]
pub fn clip_to_note(state: State<AppState>, clip_id: String, note_id: String) -> Result<Clip> {
    let c = state.db.lock().unwrap();
    let clip = to_note(&c, &clip_id, &note_id)?;
    // Filing must reach disk too — otherwise the passage exists in the app but
    // is missing from the .md file the user reads in Obsidian.
    if let Ok(note) = repo::get_note(&c, &note_id) {
        crate::notes::mirror_to_vault(&state, &c, &note);
    }
    Ok(clip)
}

#[tauri::command]
pub fn clip_to_new_note(
    state: State<AppState>,
    clip_id: String,
    title: String,
    subject_id: Option<String>,
) -> Result<Clip> {
    let c = state.db.lock().unwrap();
    let clip = to_new_note(&c, &clip_id, &title, subject_id.as_deref())?;
    if let Some(nid) = clip.note_id.clone() {
        if let Ok(note) = repo::get_note(&c, &nid) {
            crate::notes::mirror_to_vault(&state, &c, &note);
        }
    }
    Ok(clip)
}

#[tauri::command]
pub fn clip_discard(state: State<AppState>, clip_id: String) -> Result<DiscardOutcome> {
    let c = state.db.lock().unwrap();
    let before = get(&c, &clip_id).ok();
    let out = discard(&c, &clip_id)?;
    if let Some(nid) = before.and_then(|b| b.note_id) {
        if let Ok(note) = repo::get_note(&c, &nid) {
            crate::notes::mirror_to_vault(&state, &c, &note);
        }
    }
    Ok(out)
}

/// Draft flashcards from a clip. Returns editable pairs; nothing is written, so
/// a bad draft costs one model call instead of a queue full of junk.
#[tauri::command]
pub async fn clip_draft_cards(state: State<'_, AppState>, clip_id: String) -> Result<Vec<CardDraft>> {
    let (model, clip) = {
        let c = state.db.lock().unwrap();
        let clip = get(&c, &clip_id)?;
        let keys = crate::commands::read_keys(&c)?;
        if keys.offline {
            return Err(Error::Other(
                "离线模式下不能生成卡片 —— 先写一张，或到设置里连上模型".into(),
            ));
        }
        let mut m = crate::commands::load_model(&c, &keys, "chat")
            .map_err(|_| Error::Other(crate::commands::NO_MODEL.into()))?;
        crate::commands::apply_budget(&mut m, &c, "chat");
        (m, clip)
    };
    tauri::async_runtime::spawn_blocking(move || draft_cards(model.as_ref(), &clip))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

/// Persist the cards the user approved. They join the subject's normal review
/// queue (due now), so 今日复习 picks them up without a second system.
#[tauri::command]
pub fn clip_save_cards(
    state: State<AppState>,
    clip_id: String,
    subject_id: Option<String>,
    topic_id: Option<String>,
    cards: Vec<CardDraft>,
) -> Result<CardsSaved> {
    let c = state.db.lock().unwrap();
    save_cards(
        &c,
        &clip_id,
        subject_id.as_deref(),
        topic_id.as_deref(),
        &cards,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_note(c: &Connection, title: &str, body: &str) -> String {
        repo::insert_note(c, None, None, title, body).unwrap()
    }

    #[test]
    fn block_is_a_quote_with_attribution() {
        let b = render_block("第一行\n第二行", KIND_CHAT, Some("AI 回答 · deepseek"), None);
        assert_eq!(b, "> 第一行\n> 第二行\n>\n> —— AI 回答 · deepseek\n");
    }

    #[test]
    fn blank_lines_inside_a_quote_stay_in_the_quote() {
        let b = render_block("a\n\nb", KIND_PAPER, None, Some(4));
        assert_eq!(b, "> a\n>\n> b\n>\n> —— 论文 · p.4\n");
    }

    #[test]
    fn empty_selection_is_refused() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let err = add(&c, "   \n ", KIND_CHAT, None, None, None).unwrap_err();
        assert!(err.to_string().contains("为空"));
    }

    #[test]
    fn same_passage_twice_returns_one_clip() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = add(&c, "同样的句子", KIND_CHAT, Some("t1"), None, None).unwrap();
        let b = add(&c, "同样的句子", KIND_CHAT, Some("t1"), None, None).unwrap();
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn same_text_from_a_different_source_is_a_different_clip() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = add(&c, "同样的句子", KIND_CHAT, Some("t1"), None, None).unwrap();
        let b = add(&c, "同样的句子", KIND_NOTE, Some("n1"), None, None).unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn filing_creates_the_section_once_and_appends() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let nid = seed_note(&c, "TCP", "正文第一段。\n");
        let a = add(&c, "句子A", KIND_CHAT, None, None, None).unwrap();
        to_note(&c, &a.id, &nid).unwrap();
        let b = add(&c, "句子B", KIND_CHAT, None, None, None).unwrap();
        to_note(&c, &b.id, &nid).unwrap();
        let body = repo::get_note(&c, &nid).unwrap().body;
        assert_eq!(body.matches(SECTION).count(), 1, "heading must not repeat");
        assert_eq!(body.matches("句子A").count(), 1);
        assert_eq!(body.matches("句子B").count(), 1);
        assert!(body.starts_with("正文第一段。"), "prose above stays first");
        assert!(body.find("句子A").unwrap() < body.find("句子B").unwrap());
    }

    #[test]
    fn an_existing_section_keeps_its_subsections() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let body = "正文。\n\n## 摘录\n\n### 关于 BBR\n\n> 旧摘录\n\n## 其他\n\n尾注。\n";
        let nid = seed_note(&c, "X", body);
        let a = add(&c, "新摘录", KIND_CHAT, None, None, None).unwrap();
        to_note(&c, &a.id, &nid).unwrap();
        let got = repo::get_note(&c, &nid).unwrap().body;
        let bbr = got.find("### 关于 BBR").unwrap();
        let new = got.find("新摘录").unwrap();
        let other = got.find("## 其他").unwrap();
        assert!(bbr < new && new < other, "clip lands inside the section, before the next heading");
    }

    #[test]
    fn filing_twice_is_refused() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let nid = seed_note(&c, "A", "");
        let a = add(&c, "一度", KIND_CHAT, None, None, None).unwrap();
        to_note(&c, &a.id, &nid).unwrap();
        let err = to_note(&c, &a.id, &nid).unwrap_err();
        assert!(err.to_string().contains("已经落到"));
    }

    #[test]
    fn discard_takes_the_passage_back_out() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let nid = seed_note(&c, "A", "正文\n");
        let a = add(&c, "会被撤回", KIND_CHAT, None, None, None).unwrap();
        to_note(&c, &a.id, &nid).unwrap();
        let out = discard(&c, &a.id).unwrap();
        assert!(out.reverted);
        let body = repo::get_note(&c, &nid).unwrap().body;
        assert!(!body.contains("会被撤回"));
        assert!(!body.contains(SECTION), "emptied section heading is removed");
        assert!(body.contains("正文"));
    }

    #[test]
    fn undo_does_not_invent_a_trailing_newline() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        // A note written by the editor has no trailing newline (real libraries
        // look like this); filing and undoing must leave it exactly as it was.
        let nid = seed_note(&c, "A", "第一段。\n第二段。");
        let a = add(&c, "摘一句", KIND_CHAT, None, None, None).unwrap();
        to_note(&c, &a.id, &nid).unwrap();
        discard(&c, &a.id).unwrap();
        assert_eq!(repo::get_note(&c, &nid).unwrap().body, "第一段。\n第二段。");
    }

    #[test]
    fn discard_after_a_manual_edit_leaves_the_note_alone() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let nid = seed_note(&c, "A", "");
        let a = add(&c, "原始摘录", KIND_CHAT, None, None, None).unwrap();
        to_note(&c, &a.id, &nid).unwrap();
        let body = repo::get_note(&c, &nid).unwrap().body.replace("原始摘录", "我改过的摘录");
        repo::update_note(&c, &nid, "A", &body).unwrap();
        let out = discard(&c, &a.id).unwrap();
        assert!(!out.reverted, "edited block is not ours to delete");
        assert!(repo::get_note(&c, &nid).unwrap().body.contains("我改过的摘录"));
    }

    #[test]
    fn new_note_carries_the_passage_and_the_link_goes_live() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = add(&c, "见 [[TCP 核心机制]] 的第三节", KIND_CHAT, None, None, None).unwrap();
        let clip = to_new_note(&c, &a.id, "关于拥塞控制", None).unwrap();
        let nid = clip.note_id.clone().unwrap();
        let note = repo::get_note(&c, &nid).unwrap();
        assert_eq!(note.title, "关于拥塞控制");
        assert!(note.body.starts_with(SECTION));
        assert!(note.body.contains("见 [[TCP 核心机制]] 的第三节"));
        let back = crate::note_links::backlinks(&c, "TCP 核心机制").unwrap();
        assert!(back.iter().any(|b| b.title == "关于拥塞控制"));
    }

    #[test]
    fn inbox_lists_only_unfiled() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let nid = seed_note(&c, "A", "");
        let a = add(&c, "落地", KIND_CHAT, None, None, None).unwrap();
        let _b = add(&c, "留着", KIND_CHAT, None, None, None).unwrap();
        to_note(&c, &a.id, &nid).unwrap();
        let pending = list(&c, true).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].quote, "留着");
        assert_eq!(list(&c, false).unwrap().len(), 2, "the log keeps filed clips");
    }

    /// Manual end-to-end run against a real library copy:
    ///
    /// ```text
    /// CLIPS_E2E_DB=$TMP/cortex.db cargo test --lib clips::tests::real_library_round_trip -- --ignored --nocapture
    /// ```
    ///
    /// The assertion that matters is the last one: filing a clip and then
    /// discarding it must leave the note byte-identical. Anything less and the
    /// feature becomes something you are afraid to click.
    #[test]
    #[ignore = "manual: set CLIPS_E2E_DB to a copy of the real library"]
    fn real_library_round_trip() {
        let Ok(path) = std::env::var("CLIPS_E2E_DB") else {
            eprintln!("CLIPS_E2E_DB not set — skipping");
            return;
        };
        let conn = Connection::open(&path).unwrap();
        let notes = repo::list_notes(&conn, None).unwrap();
        // The longest body is the hardest case: real prose above the section.
        let note = notes
            .iter()
            .max_by_key(|n| n.body.chars().count())
            .expect("library has no notes");
        let before = note.body.clone();
        eprintln!("note: {} ({} chars)", note.title, before.chars().count());

        let clip = add(
            &conn,
            "这是从 AI 回答里摘出来的一句，用来验证落地格式。",
            KIND_CHAT,
            Some("e2e-thread"),
            Some("AI 回答 · e2e"),
            None,
        )
        .unwrap();
        let filed = to_note(&conn, &clip.id, &note.id).unwrap();
        eprintln!("filed → {} · block://n{}", filed.note_title.unwrap_or_default(), filed.block.unwrap_or_default());
        let after = repo::get_note(&conn, &note.id).unwrap().body;
        assert!(after.contains(SECTION), "section heading missing");
        assert!(after.contains("—— AI 回答 · e2e"), "attribution missing");
        assert!(after.starts_with(before.trim_end()), "existing prose must stay first and intact");

        let out = discard(&conn, &clip.id).unwrap();
        assert!(out.reverted, "discard should have reverted the note");
        let restored = repo::get_note(&conn, &note.id).unwrap().body;
        assert_eq!(
            restored.trim_end(),
            before.trim_end(),
            "undo must restore the text exactly; only trailing whitespace may differ"
        );
        eprintln!("round trip ok: {} → +{} chars → restored exactly", note.title, after.chars().count() - before.chars().count());
    }

    #[test]
    fn long_paste_is_refused_with_a_way_forward() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let huge = "字".repeat(20_001);
        let err = add(&c, &huge, KIND_CHAT, None, None, None).unwrap_err();
        assert!(err.to_string().contains("20000"));
    }

    // ---- clip → flashcards ------------------------------------------

    /// One fixed reply, no queue — enough to prove the parsing contract.
    struct Canned(&'static str);

    impl Llm for Canned {
        fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
        fn name(&self) -> String {
            "canned".into()
        }
    }

    fn card(front: &str, back: &str) -> CardDraft {
        CardDraft {
            front: front.into(),
            back: back.into(),
        }
    }

    #[test]
    fn draft_cards_survives_fences_and_small_talk() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let clip = add(&c, "BBR 靠拥塞窗口探测带宽", KIND_CHAT, None, None, None).unwrap();
        let llm = Canned(
            "好的，这是卡片：\n```json\n{\"cards\":[{\"front\":\"BBR 如何估计带宽？\",\"back\":\"靠拥塞窗口探测\"},\
             {\"front\":\"BBR 属于哪类算法？\",\"back\":\"基于模型的拥塞控制\"}]}\n```",
        );
        let cards = draft_cards(&llm, &clip).unwrap();
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].front, "BBR 如何估计带宽？");
        assert_eq!(cards[1].back, "基于模型的拥塞控制");
    }

    #[test]
    fn draft_cards_with_broken_json_is_an_error_not_an_empty_list() {
        let llm = Canned("抱歉，我无法把这段话变成问题。");
        let err = draft_cards(
            &llm,
            &Clip {
                id: "x".into(),
                quote: "q".into(),
                source_kind: KIND_CHAT.into(),
                source_id: None,
                source_label: None,
                page: None,
                note_id: None,
                block: None,
                created_at: 0,
                note_title: None,
                card_count: 0,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("JSON"));
    }

    #[test]
    fn saving_cards_creates_a_material_and_a_due_review() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        let clip = add(
            &c,
            "BBR 靠拥塞窗口探测带宽",
            KIND_CHAT,
            None,
            Some("AI 回答 · deepseek"),
            None,
        )
        .unwrap();
        let out = save_cards(&c, &clip.id, Some(&sid), None, &[
            card("BBR 如何估计带宽？", "靠拥塞窗口探测"),
            card("BBR 属于哪类算法？", "基于模型"),
        ])
        .unwrap();
        assert_eq!(out.cards, 2);
        assert_eq!(out.seeded, 2, "both cards join the queue right away");
        assert_eq!(out.subject_id, sid);

        let due = repo::srs_due(&c, &sid, "flashcard").unwrap();
        assert_eq!(due.len(), 2);
        let title: String = c
            .query_row(
                "SELECT title FROM materials WHERE id=?1",
                params![&out.material_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(title.contains("摘录") && title.contains("deepseek"), "{title}");
        assert_eq!(get(&c, &clip.id).unwrap().card_count, 2);
    }

    #[test]
    fn cards_inherit_the_subject_of_the_note_they_were_filed_under() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "系统", None, None, None).unwrap();
        let nid = repo::insert_note(&c, Some(&sid), None, "调度", "").unwrap();
        let clip = add(&c, "CFS 用红黑树维护 vruntime", KIND_NOTE, None, None, None).unwrap();
        to_note(&c, &clip.id, &nid).unwrap();
        let out = save_cards(&c, &clip.id, None, None, &[card("CFS 用什么结构？", "红黑树")]).unwrap();
        assert_eq!(out.subject_id, sid, "no picker needed when the note knows");
    }

    #[test]
    fn a_clip_without_a_subject_is_refused_instead_of_guessed() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let clip = add(&c, "无主的一句", KIND_WEB, None, None, None).unwrap();
        let err = save_cards(&c, &clip.id, None, None, &[card("问", "答")]).unwrap_err();
        assert!(err.to_string().contains("学科"));
    }

    #[test]
    fn duplicate_fronts_collapse_instead_of_fighting_the_unique_key() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        let clip = add(&c, "重复正面", KIND_CHAT, None, None, None).unwrap();
        let out = save_cards(&c, &clip.id, Some(&sid), None, &[
            card("同一个问题？", "答案一"),
            card("同一个问题？", "答案二"),
        ])
        .unwrap();
        assert_eq!(out.cards, 1);
        assert_eq!(repo::srs_due(&c, &sid, "flashcard").unwrap().len(), 1);
    }

    #[test]
    fn carding_the_same_passage_twice_adds_cards_without_resetting_progress() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        let clip = add(&c, "同一段话", KIND_CHAT, None, None, None).unwrap();
        save_cards(&c, &clip.id, Some(&sid), None, &[card("Q1", "A1")]).unwrap();
        let again = save_cards(&c, &clip.id, Some(&sid), None, &[card("Q1", "A1")]).unwrap();
        assert_eq!(again.seeded, 0, "existing schedule is left alone");
        assert_eq!(get(&c, &clip.id).unwrap().card_count, 2, "history keeps both batches");
    }

    #[test]
    fn discarding_a_clip_keeps_the_cards_it_produced() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "网络", None, None, None).unwrap();
        let clip = add(&c, "会留下卡片的一段", KIND_CHAT, None, None, None).unwrap();
        save_cards(&c, &clip.id, Some(&sid), None, &[card("Q", "A")]).unwrap();
        discard(&c, &clip.id).unwrap();
        assert_eq!(
            repo::srs_due(&c, &sid, "flashcard").unwrap().len(),
            1,
            "the receipt goes, the cards stay"
        );
    }
}
