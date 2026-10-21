//! Agent write-back (L2): the agent proposes, the human disposes.
//!
//! L1 put the library on the agent surface for *reading*. The obvious next step
//! — letting the agent write notes — is also the one place where "fusion" turns
//! dangerous: an agent that edits notes unattended will eventually overwrite
//! something the user spent an evening on, and no undo log makes that pleasant
//! to discover. So writes are two-phase:
//!
//! ```text
//! agent ──propose_note──▶ agent_proposals (pending)
//!                                   │
//!            Cortex UI renders a diff, the user clicks 采纳
//!                                   │
//!                        notes table + vault/<title>.md
//! ```
//!
//! The agent's only write target is the queue. Applying a proposal happens in
//! this process, which owns both the database and the vault — the same code
//! path a manual save takes, so `[[wiki links]]`, `vault_path` and the Markdown
//! mirror stay consistent no matter who typed the words.
//!
//! Two details carry most of the safety:
//!
//! - **`base_body` is snapshotted at propose time.** If the note changed while
//!   the proposal sat in the queue, accepting fails unless the user explicitly
//!   forces it — a stale draft can never silently undo a later edit.
//! - **`body` is always the FINAL body**, whichever mode produced it (new,
//!   append, replace). One diff renderer, one apply path, one thing to audit.

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use crate::models::Note;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub const NOTE_NEW: &str = "note_new";
pub const NOTE_APPEND: &str = "note_append";
pub const NOTE_REPLACE: &str = "note_replace";

pub const PENDING: &str = "pending";
pub const ACCEPTED: &str = "accepted";
pub const REJECTED: &str = "rejected";
pub const WITHDRAWN: &str = "withdrawn";

/// Lines of context kept around each change in a preview.
const CONTEXT: usize = 3;
/// Hard cap on preview lines returned to the UI. A note diff is a review aid,
/// not a merge tool — past a few hundred lines nobody reads it anyway.
const MAX_PREVIEW: usize = 400;
/// Refuse absurd drafts early; the queue is not a scratch disk.
const MAX_BODY_CHARS: usize = 200_000;

// ---- data -------------------------------------------------------------

/// One diff row, ready for the UI to colour.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DiffLine {
    /// `same` | `add` | `del` | `gap` (a skipped run of unchanged lines).
    pub kind: &'static str,
    pub text: String,
    /// 1-based line number in the original, when the line exists there.
    pub old_no: Option<usize>,
    /// 1-based line number in the proposed body, when the line exists there.
    pub new_no: Option<usize>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Preview {
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
    /// True when the middle of the diff was elided.
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Proposal {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub note_id: Option<String>,
    pub title: String,
    pub subject_id: Option<String>,
    pub rationale: String,
    pub author: String,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
    pub result_note_id: Option<String>,
    pub preview: Preview,
}

/// What the caller must supply to queue a draft. `text` means: the new body
/// (`new`, `replace`) or the block to append (`append`).
pub struct Draft<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub note_id: Option<&'a str>,
    /// Subject *name* (the agent reads names from `list_notebooks`, not ids).
    pub subject: Option<&'a str>,
    pub text: &'a str,
    pub rationale: &'a str,
    pub author: &'a str,
}

/// Result of applying a proposal.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Applied {
    pub note_id: String,
    pub title: String,
    pub vault_path: Option<String>,
    /// True when the vault file could not be written (DB row is still saved).
    pub vault_error: Option<String>,
}

// ---- diff -------------------------------------------------------------

fn split_lines(s: &str) -> Vec<&str> {
    s.lines().map(|l| l.trim_end_matches('\r')).collect()
}

fn same_line(text: &str, old_no: usize, new_no: usize) -> DiffLine {
    DiffLine {
        kind: "same",
        text: text.to_string(),
        old_no: Some(old_no),
        new_no: Some(new_no),
    }
}

fn del_line(text: &str, old_no: usize) -> DiffLine {
    DiffLine {
        kind: "del",
        text: text.to_string(),
        old_no: Some(old_no),
        new_no: None,
    }
}

fn add_line(text: &str, new_no: usize) -> DiffLine {
    DiffLine {
        kind: "add",
        text: text.to_string(),
        old_no: None,
        new_no: Some(new_no),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Same,
    Del,
    Add,
}

/// Classic LCS diff over lines. With identical prefix/suffix trimmed first,
/// the table stays small for the realistic case (a section edited in a long
/// note); past the cell cap we bail out to one replaced block rather than
/// allocate gigabytes for a preview nobody would read.
fn lcs_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    const MAX_CELLS: u64 = 1_000_000;
    if (a.len() as u64) * (b.len() as u64) > MAX_CELLS {
        let mut ops = Vec::with_capacity(a.len() + b.len());
        ops.extend(std::iter::repeat(Op::Del).take(a.len()));
        ops.extend(std::iter::repeat(Op::Add).take(b.len()));
        return ops;
    }
    let n = a.len();
    let m = b.len();
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if a[i] == b[j] {
                dp[(i + 1) * w + j + 1] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Same);
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
            ops.push(Op::Del);
            i += 1;
        } else {
            ops.push(Op::Add);
            j += 1;
        }
    }
    for _ in i..n {
        ops.push(Op::Del);
    }
    for _ in j..m {
        ops.push(Op::Add);
    }
    ops
}

/// Full, uncollapsed diff of two bodies.
pub fn diff_lines(base: &str, proposed: &str) -> Vec<DiffLine> {
    let a = split_lines(base);
    let b = split_lines(proposed);

    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }

    let mut out = Vec::with_capacity(a.len().max(b.len()));
    for (k, line) in a.iter().enumerate().take(pre) {
        out.push(same_line(line, k + 1, k + 1));
    }

    let (mut ai, mut bi) = (pre, pre);
    for op in lcs_ops(&a[pre..a.len() - suf], &b[pre..b.len() - suf]) {
        match op {
            Op::Same => {
                out.push(same_line(a[ai], ai + 1, bi + 1));
                ai += 1;
                bi += 1;
            }
            Op::Del => {
                out.push(del_line(a[ai], ai + 1));
                ai += 1;
            }
            Op::Add => {
                out.push(add_line(b[bi], bi + 1));
                bi += 1;
            }
        }
    }

    for k in 0..suf {
        out.push(same_line(
            a[a.len() - suf + k],
            a.len() - suf + k + 1,
            b.len() - suf + k + 1,
        ));
    }
    out
}

/// Diff plus counts, with long unchanged runs folded away so the UI shows the
/// edit instead of the note.
pub fn preview(base: &str, proposed: &str) -> Preview {
    let raw = diff_lines(base, proposed);
    let added = raw.iter().filter(|l| l.kind == "add").count();
    let removed = raw.iter().filter(|l| l.kind == "del").count();

    // Fold runs of unchanged lines down to `CONTEXT` on each side.
    let mut folded: Vec<DiffLine> = Vec::with_capacity(raw.len().min(MAX_PREVIEW));
    let mut i = 0;
    while i < raw.len() {
        if raw[i].kind != "same" {
            folded.push(raw[i].clone());
            i += 1;
            continue;
        }
        let start = i;
        while i < raw.len() && raw[i].kind == "same" {
            i += 1;
        }
        let run = &raw[start..i];
        if run.len() <= CONTEXT * 2 + 2 {
            folded.extend_from_slice(run);
        } else {
            folded.extend_from_slice(&run[..CONTEXT]);
            folded.push(DiffLine {
                kind: "gap",
                text: format!("⋯ {} 行未改动", run.len() - CONTEXT * 2),
                old_no: None,
                new_no: None,
            });
            folded.extend_from_slice(&run[run.len() - CONTEXT..]);
        }
    }

    let mut truncated = false;
    if folded.len() > MAX_PREVIEW {
        let head = MAX_PREVIEW / 2;
        let tail = MAX_PREVIEW - head;
        let mut capped: Vec<DiffLine> = folded[..head].to_vec();
        capped.push(DiffLine {
            kind: "gap",
            text: format!("⋯ 预览已截断（共 {} 处改动）", added + removed),
            old_no: None,
            new_no: None,
        });
        capped.extend_from_slice(&folded[folded.len() - tail..]);
        folded = capped;
        truncated = true;
    }

    Preview {
        lines: folded,
        added,
        removed,
        truncated,
    }
}

// ---- queue ------------------------------------------------------------

fn row_to_proposal(r: &rusqlite::Row<'_>) -> rusqlite::Result<Proposal> {
    let base: String = r.get("base_body")?;
    let body: String = r.get("body")?;
    Ok(Proposal {
        id: r.get("id")?,
        kind: r.get("kind")?,
        status: r.get("status")?,
        note_id: r.get("note_id")?,
        title: r.get("title")?,
        subject_id: r.get("subject_id")?,
        rationale: r.get("rationale")?,
        author: r.get("author")?,
        created_at: r.get("created_at")?,
        resolved_at: r.get("resolved_at")?,
        result_note_id: r.get("result_note_id")?,
        preview: preview(&base, &body),
    })
}

const PROPOSAL_COLS: &str = "id, kind, status, note_id, title, subject_id, base_body, body, \
     rationale, author, created_at, resolved_at, result_note_id";

pub fn get(conn: &Connection, id: &str) -> Result<Proposal> {
    let sql = format!("SELECT {PROPOSAL_COLS} FROM agent_proposals WHERE id=?1");
    conn.query_row(&sql, [id], row_to_proposal)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("proposal {id}")))
}

/// Newest first. `status = None` returns every proposal (history view).
pub fn list(conn: &Connection, status: Option<&str>, limit: usize) -> Result<Vec<Proposal>> {
    let sql = format!(
        "SELECT {PROPOSAL_COLS} FROM agent_proposals \
         WHERE (?1 IS NULL OR status=?1) ORDER BY created_at DESC LIMIT ?2"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![status, limit as i64], row_to_proposal)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn pending_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM agent_proposals WHERE status='pending'",
        [],
        |r| r.get(0),
    )?)
}

fn line_count(s: &str) -> usize {
    split_lines(s).len()
}

fn subject_id_by_name(conn: &Connection, name: &str) -> Result<Option<String>> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    conn.query_row(
        "SELECT id FROM subjects WHERE lower(trim(name)) = lower(trim(?1)) LIMIT 1",
        [name],
        |r| r.get(0),
    )
    .optional()?
    .map(Some)
    .ok_or_else(|| {
        Error::Other(format!(
            "没有名为「{name}」的学科 —— 先用 list_notebooks 看有哪些"
        ))
    })
}

fn find_target(conn: &Connection, note_id: Option<&str>, title: &str) -> Result<Note> {
    if let Some(id) = note_id.filter(|s| !s.trim().is_empty()) {
        return crate::repo::get_note(conn, id);
    }
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::Other("note_id 或 title 必填".into()));
    }
    let id: String = conn
        .query_row(
            "SELECT id FROM notes WHERE lower(trim(title)) = lower(trim(?1)) LIMIT 1",
            [title],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::Other(format!("没有标题为「{title}」的笔记")))?;
    crate::repo::get_note(conn, &id)
}

/// Compose the final body for a mode. Appending to an empty note is just the
/// new text — no leading blank line to accumulate on every AI pass.
fn compose(base: &str, text: &str, kind: &str) -> String {
    match kind {
        NOTE_APPEND => {
            let base_trim = base.trim_end();
            if base_trim.is_empty() {
                text.trim().to_string()
            } else {
                format!("{base_trim}\n\n{}", text.trim())
            }
        }
        _ => text.to_string(),
    }
}

/// Queue a draft. Returns the stored proposal — including an existing pending
/// one when the agent retries the same draft, so a flaky client cannot fill the
/// queue with duplicates.
pub fn create(conn: &Connection, draft: Draft<'_>) -> Result<Proposal> {
    if !matches!(draft.kind, NOTE_NEW | NOTE_APPEND | NOTE_REPLACE) {
        return Err(Error::Other(format!(
            "未知的 kind「{}」（new / append / replace）",
            draft.kind
        )));
    }
    if draft.text.trim().is_empty() {
        return Err(Error::Other("正文为空，没有任何可写入的内容".into()));
    }
    if draft.text.chars().count() > MAX_BODY_CHARS {
        return Err(Error::Other(format!(
            "正文超过 {MAX_BODY_CHARS} 字，请拆成多篇提案"
        )));
    }

    let (note_id, title, base_body, subject_id) = if draft.kind == NOTE_NEW {
        let title = draft.title.trim();
        if title.is_empty() {
            return Err(Error::Other("新建笔记需要 title".into()));
        }
        let clash: Option<String> = conn
            .query_row(
                "SELECT id FROM notes WHERE lower(trim(title)) = lower(trim(?1)) LIMIT 1",
                [title],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = clash {
            return Err(Error::Other(format!(
                "已有同名笔记「{title}」（note_id={id}）—— 想补写请用 mode=append 并传 note_id"
            )));
        }
        let sid = match draft.subject {
            Some(name) => subject_id_by_name(conn, name)?,
            None => None,
        };
        (None, title.to_string(), String::new(), sid)
    } else {
        let target = find_target(conn, draft.note_id, draft.title)?;
        (Some(target.id), target.title, target.body, target.subject_id)
    };

    let body = compose(&base_body, draft.text, draft.kind);

    // Idempotency: same target + same final body + still pending => same draft.
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM agent_proposals \
             WHERE status='pending' AND kind=?1 AND COALESCE(note_id,'')=COALESCE(?2,'') AND body=?3 \
             LIMIT 1",
            params![draft.kind, note_id, body],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return get(conn, &id);
    }

    let id = new_id();
    conn.execute(
        "INSERT INTO agent_proposals \
         (id, kind, status, note_id, title, subject_id, base_body, body, rationale, author, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id,
            draft.kind,
            PENDING,
            note_id,
            title,
            subject_id,
            base_body,
            body,
            draft.rationale.trim(),
            if draft.author.trim().is_empty() {
                "agent"
            } else {
                draft.author.trim()
            },
            now_ms(),
        ],
    )?;
    get(conn, &id)
}

/// Apply a proposal: create or update the note, rebuild `[[links]]`, mirror to
/// the vault, then mark the proposal accepted (all in one transaction).
///
/// `force` is the user confirming an overwrite after the two-phase check found
/// that the note moved on since the draft was written.
pub fn accept(
    conn: &Connection,
    root: Option<&Path>,
    id: &str,
    force: bool,
) -> Result<Applied> {
    let p = get(conn, id)?;
    if p.status != PENDING {
        return Err(Error::Other(format!(
            "提案已处理（{}）",
            status_label(&p.status)
        )));
    }
    // The queue view carries only the preview; applying needs the two full
    // bodies, so read them here and keep them out of the serialized struct.
    let (base_body, body): (String, String) = conn.query_row(
        "SELECT base_body, body FROM agent_proposals WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;

    let tx = conn.unchecked_transaction()?;
    let note_id = if p.kind == NOTE_NEW {
        let clash: Option<String> = tx
            .query_row(
                "SELECT id FROM notes WHERE lower(trim(title)) = lower(trim(?1)) LIMIT 1",
                [&p.title],
                |r| r.get(0),
            )
            .optional()?;
        if clash.is_some() {
            return Err(Error::Other(format!(
                "已有同名笔记「{}」—— 提案生成后你新建了同名笔记，请改名后再采纳",
                p.title
            )));
        }
        crate::repo::insert_note(&tx, p.subject_id.as_deref(), None, &p.title, &body)?
    } else {
        let target = p
            .note_id
            .clone()
            .ok_or_else(|| Error::Other("提案缺少目标笔记".into()))?;
        let current = crate::repo::get_note(&tx, &target)?;
        if current.body != base_body && !force {
            return Err(Error::Other(format!(
                "「{}」在提案生成后又被改过（提案基于 {} 行，现在是 {} 行）—— 确认覆盖请选择「仍然应用」",
                current.title,
                line_count(&base_body),
                line_count(&current.body)
            )));
        }
        // Keep the live title: a rename between draft and review is the user's
        // decision, not an older draft's.
        crate::repo::update_note(&tx, &target, &current.title, &body)?;
        target
    };

    crate::note_links::rebuild(&tx, &note_id, &body)?;
    tx.execute(
        "UPDATE agent_proposals SET status=?2, resolved_at=?3, result_note_id=?4 WHERE id=?1",
        params![id, ACCEPTED, now_ms(), note_id],
    )?;
    tx.commit()?;

    // The DB is committed at this point; a vault hiccup must not roll the
    // accept back (identical policy to `notes::update_note`).
    let note = crate::repo::get_note(conn, &note_id)?;
    let mut vault_path = note.vault_path.clone();
    let mut vault_error = None;
    if let Some(root) = root {
        match crate::vault::write_note(conn, root, &note) {
            Ok(rel) => vault_path = Some(rel),
            Err(e) => {
                eprintln!("[vault] could not write note {}: {e}", note.id);
                vault_error = Some(e.to_string());
            }
        }
    }

    Ok(Applied {
        note_id,
        title: note.title,
        vault_path,
        vault_error,
    })
}

pub fn reject(conn: &Connection, id: &str) -> Result<()> {
    resolve(conn, id, REJECTED)
}

/// The agent withdrawing its own draft — same effect as a rejection, kept as a
/// separate status so the history shows who gave up on the idea.
pub fn withdraw(conn: &Connection, id: &str) -> Result<()> {
    resolve(conn, id, WITHDRAWN)
}

fn resolve(conn: &Connection, id: &str, status: &str) -> Result<()> {
    let n = conn.execute(
        "UPDATE agent_proposals SET status=?2, resolved_at=?3 WHERE id=?1 AND status='pending'",
        params![id, status, now_ms()],
    )?;
    if n == 0 {
        let p = get(conn, id)?;
        return Err(Error::Other(format!(
            "提案已处理（{}）",
            status_label(&p.status)
        )));
    }
    Ok(())
}

fn status_label(status: &str) -> &'static str {
    match status {
        ACCEPTED => "已采纳",
        REJECTED => "已丢弃",
        WITHDRAWN => "已撤回",
        _ => "待审核",
    }
}

// ---- command surface --------------------------------------------------

#[tauri::command]
pub fn list_proposals(
    state: tauri::State<crate::db::AppState>,
    status: Option<String>,
) -> Result<Vec<Proposal>> {
    let c = state.db.lock().unwrap();
    list(&c, status.as_deref(), 200)
}

#[tauri::command]
pub fn accept_proposal(
    state: tauri::State<crate::db::AppState>,
    id: String,
    force: Option<bool>,
) -> Result<Applied> {
    let c = state.db.lock().unwrap();
    let root = crate::vault::root_for(&c, state.data_dir.as_deref());
    accept(&c, root.as_deref(), &id, force.unwrap_or(false))
}

#[tauri::command]
pub fn reject_proposal(state: tauri::State<crate::db::AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    reject(&c, &id)
}

/// How many drafts are waiting. Kept separate from `list_proposals` because
/// Today's flow bar polls it and does not need the diffs.
#[tauri::command]
pub fn pending_proposals(state: tauri::State<crate::db::AppState>) -> Result<i64> {
    let c = state.db.lock().unwrap();
    pending_count(&c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::AppState;

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cortex-proposals-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn draft<'a>(kind: &'a str, title: &'a str, text: &'a str) -> Draft<'a> {
        Draft {
            kind,
            title,
            note_id: None,
            subject: None,
            text,
            rationale: "test",
            author: "codex",
        }
    }

    #[test]
    fn diff_marks_only_the_edited_line() {
        let d = diff_lines("a\nb\nc", "a\nB\nc");
        let kinds: Vec<&str> = d.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec!["same", "del", "add", "same"]);
        assert_eq!(d[1].old_no, Some(2));
        assert_eq!(d[2].new_no, Some(2));
    }

    #[test]
    fn long_unchanged_runs_fold_into_a_gap() {
        let base: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let mut changed = base.clone();
        changed.push_str("last line\n");
        let p = preview(&base, &changed);
        assert!(p.lines.iter().any(|l| l.kind == "gap"), "gap expected");
        assert_eq!(p.added, 1);
        assert_eq!(p.removed, 0);
        assert!(!p.truncated);
    }

    #[test]
    fn new_draft_is_queued_and_counted() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = create(&c, draft(NOTE_NEW, "TCP 笔记", "# TCP\n三次握手")).unwrap();
        assert_eq!(p.status, PENDING);
        assert_eq!(p.preview.added, 2);
        assert_eq!(pending_count(&c).unwrap(), 1);
    }

    #[test]
    fn duplicate_draft_returns_the_same_proposal() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = create(&c, draft(NOTE_NEW, "重复", "x")).unwrap();
        let b = create(&c, draft(NOTE_NEW, "重复", "x")).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(pending_count(&c).unwrap(), 1);
    }

    #[test]
    fn new_draft_refuses_an_existing_title() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        crate::repo::insert_note(&c, None, None, "已有", "body").unwrap();
        let err = create(&c, draft(NOTE_NEW, "已有", "x")).unwrap_err();
        assert!(err.to_string().contains("同名"));
    }

    #[test]
    fn append_composes_onto_the_live_body_and_accept_writes_the_vault() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let note_id = crate::repo::insert_note(&c, None, None, "笔记", "第一段").unwrap();
        let p = create(
            &c,
            Draft {
                kind: NOTE_APPEND,
                title: "",
                note_id: Some(&note_id),
                subject: None,
                text: "第二段",
                rationale: "补充",
                author: "codex",
            },
        )
        .unwrap();
        assert_eq!(p.preview.added, 2); // blank separator + the paragraph

        let root = tmp_root("append");
        let applied = accept(&c, Some(&root), &p.id, false).unwrap();
        assert_eq!(applied.note_id, note_id);
        let note = crate::repo::get_note(&c, &note_id).unwrap();
        assert_eq!(note.body, "第一段\n\n第二段");
        let rel = applied.vault_path.expect("vault path");
        let text = std::fs::read_to_string(root.join(&rel)).unwrap();
        assert!(text.contains("第二段"), "{text}");
        assert!(text.contains("cortex_id:"), "frontmatter stamped: {text}");
        assert_eq!(get(&c, &p.id).unwrap().status, ACCEPTED);
        assert_eq!(pending_count(&c).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_base_is_refused_unless_forced() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let note_id = crate::repo::insert_note(&c, None, None, "并发", "v1").unwrap();
        let p = create(
            &c,
            Draft {
                kind: NOTE_REPLACE,
                title: "",
                note_id: Some(&note_id),
                subject: None,
                text: "v2 by agent",
                rationale: "",
                author: "codex",
            },
        )
        .unwrap();
        // The user edits the same note while the proposal waits for review.
        crate::repo::update_note(&c, &note_id, "并发", "v1 + my own edit").unwrap();

        let err = accept(&c, None, &p.id, false).unwrap_err();
        assert!(err.to_string().contains("又被改过"), "{err}");
        assert_eq!(get(&c, &p.id).unwrap().status, PENDING);

        accept(&c, None, &p.id, true).unwrap();
        assert_eq!(
            crate::repo::get_note(&c, &note_id).unwrap().body,
            "v2 by agent"
        );
    }

    #[test]
    fn accept_is_idempotent_guarded() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = create(&c, draft(NOTE_NEW, "一次性", "x")).unwrap();
        accept(&c, None, &p.id, false).unwrap();
        let err = accept(&c, None, &p.id, false).unwrap_err();
        assert!(err.to_string().contains("已处理"));
        let notes = crate::repo::list_notes(&c, None).unwrap();
        assert_eq!(notes.len(), 1, "second accept must not duplicate the note");
    }

    #[test]
    fn reject_keeps_the_note_untouched() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = create(&c, draft(NOTE_NEW, "丢弃", "x")).unwrap();
        reject(&c, &p.id).unwrap();
        assert_eq!(get(&c, &p.id).unwrap().status, REJECTED);
        assert!(crate::repo::list_notes(&c, None).unwrap().is_empty());
        assert!(reject(&c, &p.id).unwrap_err().to_string().contains("已处理"));
    }

    #[test]
    fn unknown_subject_name_is_reported() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let err = create(
            &c,
            Draft {
                kind: NOTE_NEW,
                title: "带学科",
                note_id: None,
                subject: Some("不存在的学科"),
                text: "x",
                rationale: "",
                author: "",
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("list_notebooks"));
    }

    /// The panel reads a fixed shape; a renamed field would silently render an
    /// empty diff instead of failing loudly in Rust. Pin the contract.
    #[test]
    fn proposal_json_shape_matches_the_ui() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = create(&c, draft(NOTE_NEW, "JSON 契约", "a\nb")).unwrap();
        let v = serde_json::to_value(&p).unwrap();
        for key in [
            "id", "kind", "status", "note_id", "title", "subject_id", "rationale", "author",
            "created_at", "resolved_at", "result_note_id", "preview",
        ] {
            assert!(v.get(key).is_some(), "missing {key} in {v}");
        }
        let preview = &v["preview"];
        for key in ["lines", "added", "removed", "truncated"] {
            assert!(preview.get(key).is_some(), "missing preview.{key}");
        }
        let line = &preview["lines"][0];
        for key in ["kind", "text", "old_no", "new_no"] {
            assert!(line.get(key).is_some(), "missing line.{key}");
        }
        assert_eq!(line["old_no"], serde_json::Value::Null);
    }
}
