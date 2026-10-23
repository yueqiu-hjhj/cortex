//! Executes agent runs — the loop that turns the platform's tables into
//! behavior.
//!
//! Before this module the platform had everything EXCEPT an engine: agents,
//! runs, messages, skills and five seeded read-only tools lived in SQLite,
//! `start_run` created a pending row, and then nothing ever happened. The
//! competitor story is "agent auto-research"; this is the minimum honest
//! version: a bounded tool loop over the SAME knowledge base the app uses,
//! fully written to the audit log.
//!
//! Protocol (deliberately line-based, not JSON-only — models hold a two-line
//! contract far better than a strict JSON envelope):
//!
//!   ANSWER: <final reply>          → run finishes with this as its result
//!   TOOL <name> {"arg":"value"}    → the named tool runs, its output is
//!                                    appended, the loop continues
//!   anything else                  → treated as the final answer
//!
//! Safety mirrors the platform's: tools come from the agent's whitelist ∩ the
//! enabled built-in registry ∩ the agent's permission tier (read agents see
//! only read tools; write agents additionally get two bounded, audited
//! writers — no shell, no delete, no overwrite), every step is appended to
//! `agent_messages`, and the loop is capped by the agent's `max_steps`.

use crate::agents::{append_message, finish_run, Agent};
use crate::error::Result;
use crate::llm::Llm;
use crate::models::Note;
use crate::repo;
use rusqlite::{params, Connection};
use std::collections::HashMap;

/// Tool output is truncated before it enters the transcript — a 200 KB PDF
/// dump would eat the context window of every following step.
const TOOL_RESULT_CHARS: usize = 2400;
/// One tool result shown to the model per tool call, formatted as lines.
const TOOL_HITS: usize = 8;

/// The executor's view of one callable tool: registry row + implementation.
struct BuiltinTool {
    name: &'static str,
    /// One-line usage shown to the model, including the expected JSON args.
    usage: &'static str,
    /// Risk tier used for both prompt filtering and the dispatch gate.
    tier: &'static str,
    run: fn(&ToolCtx, &serde_json::Value) -> Result<String>,
}

/// What a tool body is allowed to touch. `data_dir` is here because a note
/// write has to reach the vault: the Markdown file is the truth and SQLite is
/// only its index, so a tool that updated the row alone would be silently
/// reverted by the next reindex. `run_id` goes into the revision log, so every
/// agent edit can be traced back to the run that made it.
struct ToolCtx<'a> {
    conn: &'a Connection,
    data_dir: Option<&'a std::path::Path>,
    run_id: &'a str,
}

impl<'a> ToolCtx<'a> {
    fn new(conn: &'a Connection, data_dir: Option<&'a std::path::Path>, run_id: &'a str) -> Self {
        Self { conn, data_dir, run_id }
    }
}

/// The five read tools seeded by migration 0030 plus the two write tools
/// seeded by 0076. Keep the names in sync with those migrations — the DB rows
/// are the registry of record for the UI; this table is the dispatch.
/// `&'static` so the executor can hold borrows.
fn builtin_tools() -> &'static [BuiltinTool] {
    static TOOLS: std::sync::OnceLock<Vec<BuiltinTool>> = std::sync::OnceLock::new();
    TOOLS.get_or_init(|| {
        vec![
            BuiltinTool {
                name: "list_notebooks",
                usage: r#"list_notebooks {} — 列出所有科目及其主题."#,
                tier: "read",
                run: tool_list_notebooks,
            },
            BuiltinTool {
                name: "search_notebook",
                usage: r#"search_notebook {"query": "关键词", "subject_id": "可选", "limit": 8} — 全文检索知识库分块."#,
                tier: "read",
                run: tool_search_notebook,
            },
            BuiltinTool {
                name: "get_source",
                usage: r#"get_source {"source_id": "…"} — 读取一个来源的名称与全文（截断）."#,
                tier: "read",
                run: tool_get_source,
            },
            BuiltinTool {
                name: "search_notes",
                usage: r#"search_notes {"query": "关键词", "limit": 8} — 按标题/正文搜索用户的笔记."#,
                tier: "read",
                run: tool_search_notes,
            },
            BuiltinTool {
                name: "get_knowledge_tree",
                usage: r#"get_knowledge_tree {"subject_id": "…"} — 科目的主题树与各主题的来源数."#,
                tier: "read",
                run: tool_knowledge_tree,
            },
            BuiltinTool {
                name: "save_note",
                usage: r#"save_note {"title": "标题", "body": "正文", "subject_id": "可选", "topic_id": "可选"} — 新建一条笔记（write 权限）."#,
                tier: "write",
                run: tool_save_note,
            },
            BuiltinTool {
                name: "save_flashcards",
                usage: r#"save_flashcards {"subject_id": "…", "title": "可选", "topic_id": "可选", "cards": [{"front": "问", "back": "答"}]} — 保存 1-50 张问答卡（write 权限）."#,
                tier: "write",
                run: tool_save_flashcards,
            },
            BuiltinTool {
                name: "get_note",
                usage: r#"get_note {"note_id": "…"} — 读取一条笔记的完整正文（write 前必用）."#,
                tier: "read",
                run: tool_get_note,
            },
            BuiltinTool {
                name: "update_note",
                usage: r#"update_note {"note_id": "…", "find": "原文", "replace": "新文"} — 改一条已有笔记（write 权限）；整篇重写用 body，撤销用 restore_revision."#,
                tier: "write",
                run: tool_update_note,
            },
        ]
    })
}

// ------------------------------------------------------------ tool bodies

fn tool_list_notebooks(ctx: &ToolCtx, _args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let subjects = repo::list_subjects(c)?;
    let mut out = String::new();
    for s in subjects {
        let topics = repo::list_topics(c, &s.id)?;
        let names: Vec<&str> = topics.iter().map(|t| t.name.as_str()).collect();
        out.push_str(&format!(
            "- {}（{} 个主题）：{}\n",
            s.name,
            topics.len(),
            if names.is_empty() { "—".to_string() } else { names.join("、") }
        ));
    }
    Ok(if out.is_empty() { "（还没有任何科目）".into() } else { out })
}

fn tool_search_notebook(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.trim().is_empty() {
        return Ok("缺少 query 参数".into());
    }
    let subject = args.get("subject_id").and_then(|v| v.as_str());
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(TOOL_HITS as i64) as usize;
    let hits = repo::keyword_search_chunks(c, subject, None, query, limit.max(1))?;
    if hits.is_empty() {
        return Ok("没有命中。尝试更短或更常见的关键词。".into());
    }
    let mut out = String::new();
    for h in &hits {
        out.push_str(&format!(
            "- {} · 位置 {}：{}\n",
            h.source_name,
            h.loc.clone().unwrap_or_default(),
            snippet(&h.text, 200)
        ));
    }
    Ok(out)
}

fn tool_get_source(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let id = args.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
    if id.trim().is_empty() {
        return Ok("缺少 source_id 参数".into());
    }
    let s = repo::get_source(c, id)?;
    let text = s.content.as_deref().unwrap_or("(无提取文本)");
    Ok(format!("「{}」（kind={}, status={}）\n\n{}", s.name, s.kind, s.status, snippet(text, 3000)))
}

fn tool_search_notes(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.trim().is_empty() {
        return Ok("缺少 query 参数".into());
    }
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(TOOL_HITS as i64) as usize;
    let pat = format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = c.prepare(
        "SELECT id, title, substr(body, 1, 240) FROM notes \
         WHERE title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\' \
         ORDER BY updated_at DESC LIMIT ?2",
    )?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map(params![pat, limit.max(1) as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok("没有命中的笔记。".into());
    }
    // The id is in the line on purpose: get_note/update_note address a note by
    // id, and a search result without one leaves the model to guess by title.
    Ok(rows
        .iter()
        .map(|(id, t, b)| format!("- [{id}] {t}：{b}"))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn tool_knowledge_tree(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let id = args.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
    if id.trim().is_empty() {
        return Ok("缺少 subject_id 参数（可先用 list_notebooks 查到）".into());
    }
    let topics = repo::list_topics(c, id)?;
    if topics.is_empty() {
        return Ok("该科目还没有主题。".into());
    }
    let mut out = String::new();
    for t in &topics {
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM sources WHERE topic_id = ?1",
            params![t.id],
            |r| r.get(0),
        )?;
        out.push_str(&format!("- {}（{} 个来源）\n", t.name, n));
    }
    Ok(out)
}

/// Write tool: create a note. Append-only by design — no update/delete
/// surface, bounded sizes, optional subject/topic placement with the same
/// belongs-check the UI path enforces.
fn tool_save_note(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("").trim();
    if title.is_empty() || body.is_empty() {
        return Ok("缺少 title 或 body 参数，笔记未保存".into());
    }
    if title.chars().count() > 200 {
        return Ok("title 过长（上限 200 字），请缩短后重试".into());
    }
    if body.chars().count() > 20_000 {
        return Ok("body 过长（上限 20000 字），请拆成多条笔记".into());
    }
    let subject_id = args.get("subject_id").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
    if let Some(sid) = subject_id {
        if repo::get_subject(c, sid).is_err() {
            return Ok(format!("subject_id {sid} 不存在，笔记未保存（可先用 list_notebooks 查到）"));
        }
    }
    let topic_id = args.get("topic_id").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
    if let (Some(tid), Some(sid)) = (topic_id, subject_id) {
        let belongs: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM topics WHERE id=?1 AND subject_id=?2)",
            params![tid, sid],
            |r| r.get(0),
        )?;
        if !belongs {
            return Ok("topic_id 不属于给定的 subject_id，笔记未保存".into());
        }
    } else if topic_id.is_some() {
        return Ok("提供了 topic_id 但缺少 subject_id，笔记未保存".into());
    }
    let id = repo::insert_note(c, subject_id, topic_id, title, body)?;
    // The row is only the index; the Markdown file is the note. A create that
    // skipped the file would look fine until the next reindex read the disk and
    // made the note disappear from search.
    crate::note_links::rebuild(c, &id, body)?;
    if let Err(e) = crate::note_knowledge::rebuild(c, &id) {
        eprintln!("[coverage] could not index agent note {id}: {e}");
    }
    let note = repo::get_note(c, &id)?;
    let on_disk = mirror_note(ctx, &note);
    Ok(format!(
        "已保存笔记（id={id}）：「{title}」{}",
        if on_disk { "" } else { "（数据库已写入，但 vault 文件写入失败——下次启动会补写）" }
    ))
}

/// Write tool: save a Q/A deck as a flashcards material — the same
/// `repo::save_material` path the UI's save_flashcards command uses, so the
/// result appears in the normal materials list and SRS seeding flows.
fn tool_save_flashcards(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let subject_id = args.get("subject_id").and_then(|v| v.as_str()).unwrap_or("").trim();
    if subject_id.is_empty() {
        return Ok("缺少 subject_id 参数（可先用 list_notebooks 查到），卡片未保存".into());
    }
    if repo::get_subject(c, subject_id).is_err() {
        return Ok(format!("subject_id {subject_id} 不存在，卡片未保存（可先用 list_notebooks 查到）"));
    }
    let topic_id = args.get("topic_id").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
    if let Some(tid) = topic_id {
        let belongs: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM topics WHERE id=?1 AND subject_id=?2)",
            params![tid, subject_id],
            |r| r.get(0),
        )?;
        if !belongs {
            return Ok("topic_id 不属于给定的 subject_id，卡片未保存".into());
        }
    }
    let Some(cards) = args.get("cards").and_then(|v| v.as_array()) else {
        return Ok("缺少 cards 参数（数组，每项含 front/back），卡片未保存".into());
    };
    let mut clean: Vec<serde_json::Value> = Vec::new();
    for card in cards {
        let front = card.get("front").and_then(|v| v.as_str()).unwrap_or("").trim();
        let back = card.get("back").and_then(|v| v.as_str()).unwrap_or("").trim();
        if !front.is_empty() && !back.is_empty() {
            clean.push(serde_json::json!({"q": front, "a": back}));
        }
    }
    if clean.is_empty() {
        return Ok("cards 里没有一张完整的问答卡（front/back 都不能为空）".into());
    }
    if clean.len() > 50 {
        return Ok("一次最多保存 50 张卡片，请分批保存".into());
    }
    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
    let title: String = if title.is_empty() {
        "Agent flashcards".into()
    } else {
        title.chars().take(120).collect()
    };
    let count = clean.len();
    let meta = format!("{count} cards · agent");
    repo::save_material(c, subject_id, topic_id, "flashcards", &title, &meta, &serde_json::Value::Array(clean))?;
    Ok(format!("已保存 {count} 张卡片到「{title}」"))
}

// ------------------------------------------- reading and editing a note

/// Push a note to its Markdown file. Returns whether the file is now on disk:
/// the caller reports a failure instead of swallowing it, because a note that
/// exists only in SQLite will vanish from search at the next reindex.
fn mirror_note(ctx: &ToolCtx, note: &Note) -> bool {
    let Some(root) = crate::vault::root_for(ctx.conn, ctx.data_dir) else {
        return false;
    };
    match crate::vault::write_note(ctx.conn, &root, note) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("[vault] agent write could not mirror note {}: {e}", note.id);
            false
        }
    }
}

/// Which note a call means. `note_id` is preferred; an exact title is accepted
/// because a model that just ran `search_notes` has the title in hand, and an
/// id is one more thing for it to get wrong. Ambiguity is refused rather than
/// guessed — two notes can share a title, and editing the wrong one is not
/// something the caller can notice afterwards.
fn resolve_note(c: &Connection, args: &serde_json::Value) -> std::result::Result<Note, String> {
    let id = args
        .get("note_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (id, title) {
        (Some(_), Some(_)) => Err("note_id 和 title 只能给一个（title 仅在不知道 id 时用）".into()),
        (None, None) => Err("缺少 note_id（可先用 search_notes 查到，结果行首就是 id）".into()),
        (Some(id), None) => repo::get_note(c, id)
            .map_err(|_| format!("note_id {id} 不存在（可先用 search_notes 查到）")),
        (None, Some(want)) => {
            let mut hits: Vec<Note> = repo::list_notes(c, None)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|n| n.title == want)
                .collect();
            match hits.len() {
                0 => Err(format!("没有标题为「{want}」的笔记（可先用 search_notes 查）")),
                1 => Ok(hits.remove(0)),
                n => Err(format!(
                    "有 {n} 条笔记都叫「{want}」，无法确定改哪一条；请改用 note_id"
                )),
            }
        }
    }
}

/// Read tool: one note's text. The body is head-truncated with the real length
/// stated, because the transcript budget truncates every tool result at 2400
/// chars anyway — better to say "only the first N of M chars" than to hand back
/// a silently clipped note the model believes it has read in full.
fn tool_get_note(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let note = match resolve_note(c, args) {
        Ok(n) => n,
        Err(msg) => return Ok(msg),
    };
    let mut out = format!("笔记 {} 「{}」\n", note.id, note.title);
    if let Some(sid) = note.subject_id.as_deref() {
        let name: Option<String> = c
            .query_row("SELECT name FROM subjects WHERE id=?1", params![sid], |r| r.get(0))
            .ok();
        out.push_str(&format!("科目：{}\n", name.unwrap_or_else(|| sid.to_string())));
    }
    if let Some(vp) = note.vault_path.as_deref() {
        out.push_str(&format!("文件：{vp}\n"));
    }
    let total = note.body.chars().count();
    out.push_str(&format!("正文（{total} 字）：\n{}\n", snippet(&note.body, 1800)));
    if total > 1800 {
        out.push_str(&format!(
            "（只显示了前 1800 字，共 {total} 字。改动用 find/replace 逐字匹配，不要凭这段节选整篇重写）\n"
        ));
    }
    let revs = repo::note_revisions(c, &note.id, 5)?;
    if !revs.is_empty() {
        out.push_str("\n修订记录（新→旧，撤销用 update_note restore_revision）：\n");
        for r in &revs {
            out.push_str(&format!(
                "- {} · 改前 {} 字 · {}\n",
                r.id,
                r.body.chars().count(),
                r.reason.as_deref().unwrap_or("—")
            ));
        }
    }
    Ok(out)
}

/// Write tool: edit an existing note — the first write surface that is not
/// append-only, so it is the first one that has to be undoable.
///
/// Three ways to say what to change, exactly one per call:
/// `find`+`replace` (targeted, the default), `body` (whole rewrite),
/// `restore_revision` (undo). Two at once is a caller that is guessing, and a
/// refusal is the honest answer to a guess.
///
/// Before anything is written the current title and body are snapshotted, so
/// every edit can be undone from the tool itself. `find` must match exactly
/// once: zero matches means the model's memory of the text is wrong, and
/// multiple matches means it did not read enough context — in both cases
/// writing would be inventing an edit the caller did not describe.
fn tool_update_note(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let note = match resolve_note(c, args) {
        Ok(n) => n,
        Err(msg) => return Ok(msg),
    };

    let body_arg = args.get("body").and_then(|v| v.as_str());
    let find = args.get("find").and_then(|v| v.as_str());
    let replace = args.get("replace").and_then(|v| v.as_str());
    let restore = args
        .get("restore_revision")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let new_title = args
        .get("new_title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let requested = [body_arg.is_some(), find.is_some() || replace.is_some(), restore.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if requested != 1 {
        return Ok("请给且只给一种改法：\n- 精确替换：find + replace\n- 整篇重写：body\n- 撤销：restore_revision".into());
    }
    if find.is_some() != replace.is_some() {
        return Ok("find 和 replace 必须成对出现（删掉一段就 replace 成空串）".into());
    }

    // Plan first, write second: the plan is what every refusal above inspects.
    let (planned_title, planned_body, what) = if let Some(rev_id) = restore {
        let rev = match repo::get_note_revision(c, rev_id) {
            Ok(r) => r,
            Err(_) => return Ok(format!("修订 {rev_id} 不存在，未改动")),
        };
        if rev.note_id != note.id {
            return Ok(format!("修订 {rev_id} 不属于这条笔记，未改动"));
        }
        (rev.title, rev.body, format!("撤销到修订 {rev_id}"))
    } else if let Some(b) = body_arg {
        (
            new_title.unwrap_or(note.title.as_str()).to_string(),
            b.to_string(),
            "整篇重写".to_string(),
        )
    } else {
        let f = find.unwrap_or_default();
        let r = replace.unwrap_or_default();
        if f.is_empty() {
            return Ok("find 不能是空串，未改动".into());
        }
        let hits = note.body.matches(f).count();
        if hits == 0 {
            return Ok(format!(
                "正文里找不到「{}」——未改动。先用 get_note 看清原文，不要凭记忆改写",
                snippet(f, 60)
            ));
        }
        if hits > 1 {
            return Ok(format!(
                "「{}」在正文里出现 {hits} 次，不唯一，未改动。请把 find 扩到能唯一定位的一段",
                snippet(f, 60)
            ));
        }
        (
            new_title.unwrap_or(note.title.as_str()).to_string(),
            note.body.replacen(f, r, 1),
            "替换 1 处".to_string(),
        )
    };

    if planned_title.is_empty() || planned_title.chars().count() > 200 {
        return Ok("标题为空或超过 200 字，未改动".into());
    }
    if planned_body.trim().is_empty() {
        return Ok("改完正文为空，未改动（要删笔记请用界面，工具不删）".into());
    }
    if planned_body.chars().count() > 20_000 {
        return Ok("改完正文超过 20000 字，未改动（请拆成多条笔记）".into());
    }
    if planned_body == note.body && planned_title == note.title {
        return Ok("内容没有变化，未改动，也没有产生修订".into());
    }

    let before = note.body.chars().count();
    let after = planned_body.chars().count();
    let actor = format!("agent:{}", ctx.run_id);
    let rev = repo::insert_note_revision(
        c,
        &note.id,
        &note.title,
        &note.body,
        Some(&actor),
        Some(&what),
    )?;
    repo::update_note(c, &note.id, &planned_title, &planned_body)?;
    crate::note_links::rebuild(c, &note.id, &planned_body)?;
    if let Err(e) = crate::note_knowledge::rebuild(c, &note.id) {
        eprintln!("[coverage] could not reindex note {}: {e}", note.id);
    }
    let saved = repo::get_note(c, &note.id)?;
    let on_disk = mirror_note(ctx, &saved);

    Ok(format!(
        "已改「{title}」：{what}，正文 {before} → {after} 字。\n改前全文存在修订 {rev}，撤销：update_note {{\"note_id\":\"{id}\",\"restore_revision\":\"{rev}\"}}{}",
        if on_disk { "" } else { "\n注意：数据库已改，但 vault 文件写入失败——下次启动会补写" },
        title = short_title(&saved.title),
        id = saved.id,
    ))
}

fn short_title(t: &str) -> String {
    snippet(t, 80)
}

/// Char-boundary-safe truncation with an ellipsis marker.
fn snippet(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

// ------------------------------------------------------------- the loop

/// Load the tool implementations the agent may call: its whitelist ∩ the
/// enabled built-in registry ∩ the agent's permission tier. Tier-filtering
/// here (not just at dispatch) means a read-tier agent never even sees
/// write tools in its prompt — no wasted round-trips into a denial.
/// An agent with no tools (or all-disabled tools) still works — it just
/// answers in one completion.
fn resolve_tools(c: &Connection, user_id: &str, agent: &Agent) -> Result<Vec<&'static BuiltinTool>> {
    let enabled: std::collections::HashSet<String> = crate::agents::list_tools(c, user_id)?
        .into_iter()
        .filter(|t| t.enabled && (t.kind == "read" || t.kind == "write"))
        .map(|t| t.name)
        .collect();
    Ok(builtin_tools()
        .iter()
        .filter(|t| agent.tools.iter().any(|n| n == t.name) && enabled.contains(t.name))
        .filter(|t| crate::agents::tier_rank(t.tier) <= crate::agents::tier_rank(&agent.tool_tier))
        .collect())
}

/// C2 (4.4): MCP tools visible to this agent — registry rows with kind='mcp'
/// (synced from the configured servers by `mcp_sync_tools`) that the agent's
/// whitelist grants. The `description` column carries the pre-rendered usage
/// line; server/tool split out of the `mcp__<server>__<tool>` name.
struct McpToolInfo {
    name: String,
    usage: String,
    server: String,
    tool: String,
}

fn resolve_mcp_tools(c: &Connection, user_id: &str, agent: &Agent) -> Result<Vec<McpToolInfo>> {
    let mut stmt = c.prepare(
        "SELECT name, description FROM agent_tools WHERE user_id=?1 AND kind='mcp' AND enabled=1",
    )?;
    let rows = stmt
        .query_map(params![user_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(n, _)| agent.tools.iter().any(|a| a == n))
        .filter_map(|(n, usage)| {
            let (server, tool) = crate::mcp_client::split_mcp_tool_name(&n)?;
            Some(McpToolInfo { name: n, usage, server, tool })
        })
        .collect())
}

fn skill_bodies(c: &Connection, user_id: &str, agent: &Agent) -> Result<String> {
    let mut out = String::new();
    if agent.skills.is_empty() {
        return Ok(out);
    }
    for sk in crate::agents::list_skills(c, user_id)? {
        if agent.skills.contains(&sk.name) && sk.enabled {
            out.push_str(&format!("\n## 技能：{}\n{}\n", sk.name, sk.body));
        }
    }
    Ok(out)
}

fn system_prompt(
    tools: &[&BuiltinTool],
    mcp_tools: &[McpToolInfo],
    skills: &str,
    agent: &Agent,
) -> String {
    let mut s = agent.system_prompt.clone();
    if !tools.is_empty() || !mcp_tools.is_empty() {
        s.push_str("\n\n## 可用工具\n每次回复只能是下面两种格式之一：\n\
            1. `TOOL 工具名 {json参数}` —— 调用一次工具，结果会在下一轮给你；\n\
            2. `ANSWER: 最终回答` —— 结束运行。\n\
            先用工具查证，再回答；不要编造来源。可用工具：\n");
        for t in tools {
            s.push_str(&format!("- {}\n", t.usage));
        }
        for t in mcp_tools {
            s.push_str(&format!("- {}\n", t.usage));
        }
    } else {
        s.push_str("\n\n没有可用工具，直接给出最终回答，并以 `ANSWER: ` 开头。");
    }
    // 分级权限：让模型知道自己的权限上限，避免它白费一轮调用被拒的工具。
    s.push_str(&format!(
        "\n\n## 工具权限\n你被授予 `{}` 级权限（read 只读 < write 应用写入 < network 外部网络，低含高）。超出权限的工具调用会被拒绝并记录在案。\n",
        crate::agents::normalize_tier(&agent.tool_tier)
    ));
    if !skills.is_empty() {
        s.push_str("\n## 可用技能（指令遵循）\n");
        s.push_str(skills);
    }
    s
}

/// Risk tier of a tool call. Built-in tools carry their tier in the registry
/// (the two 0076 writers are `write`); MCP tools run on external servers and
/// are classified `network` conservatively — the tier bounds what a prompt
/// injection inside fetched content could reach.
fn tool_tier(name: &str) -> &'static str {
    if let Some(t) = builtin_tools().iter().find(|t| t.name == name) {
        return t.tier;
    }
    if name.starts_with("mcp__") {
        "network"
    } else {
        "read"
    }
}

/// Permission gate at dispatch: denies the call, records the denial in the
/// transcript (the audit trail), and lets the run continue.
fn tier_allows(agent: &Agent, name: &str) -> bool {
    crate::agents::tier_rank(tool_tier(name)) <= crate::agents::tier_rank(&agent.tool_tier)
}

fn transcript(messages: &[(String, String)]) -> String {
    messages
        .iter()
        .map(|(role, content)| match role.as_str() {
            "tool" => format!("[工具结果]\n{content}"),
            "assistant" => format!("[助手] {content}"),
            "user" => format!("[用户] {content}"),
            other => format!("[{other}] {content}"),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Parse `TOOL <name> {json}` → (name, args). Tolerates missing braces and
/// trailing prose after the JSON object (models add it constantly).
fn parse_tool_call(reply: &str) -> Option<(String, serde_json::Value)> {
    let rest = reply.trim().strip_prefix("TOOL")?.trim_start();
    let (name, after) = rest.split_once(char::is_whitespace)?;
    // First '{' to its matching last '}': brace counting, not regex.
    let start = after.find('{')?;
    let bytes = after.as_bytes();
    let mut depth = 0usize;
    let mut end = None;
    for (i, b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let json = &after[start..end?];
    let args = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    Some((name.trim().to_string(), args))
}

/// Execute a run end-to-end. The run row must already exist (via
/// [`crate::agents::start_run`]); this appends the assistant/tool messages and
/// finishes the run. Returns the finished run.
pub fn execute_run(
    c: &Connection,
    user_id: &str,
    agent: &Agent,
    run_id: &str,
    model: &dyn Llm,
    data_dir: Option<&std::path::Path>,
) -> Result<crate::agents::AgentRun> {
    let tools = resolve_tools(c, user_id, agent)?;
    let mcp_tools = resolve_mcp_tools(c, user_id, agent)?;
    let skills = skill_bodies(c, user_id, agent)?;
    let system = system_prompt(&tools, &mcp_tools, &skills, agent);
    // Server configs resolved once; sessions spawn lazily on first call —
    // a run that never touches MCP boots nothing.
    let mcp_cfgs: std::collections::HashMap<String, crate::mcp_client::McpServerCfg> =
        crate::mcp_client::parse_server_cfgs(
            repo::get_setting(c, "mcp_servers").ok().flatten().as_deref(),
        )
        .into_iter()
        .map(|cfg| (cfg.name.clone(), cfg))
        .collect();
    let mut mcp_sessions: HashMap<String, crate::mcp_client::McpSession> = HashMap::new();

    let ctx = ToolCtx::new(c, data_dir, run_id);
    let mut final_answer: Option<String> = None;
    let mut run_error: Option<String> = None;

    for _step in 0..agent.max_steps.max(1) {
        // Transcript = every message so far, including tool results.
        // Thread-aware transcript: switching agents mid-conversation keeps
        // every earlier agent's messages as this run's context (0047).
        let history: Vec<(String, String)> = crate::agents::history_for_run(c, user_id, run_id, 400)?
            .into_iter()
            .map(|m| (m.role, m.content))
            .collect();
        let user = transcript(&history);

        let reply = match model.complete(&system, &user) {
            Ok(r) => {
                crate::tokens::record(c, "agent", &model.name(), &format!("{system}\n{user}"), &r, None);
                r
            }
            Err(e) => {
                run_error = Some(format!("模型调用失败：{e}"));
                break;
            }
        };

        if let Some((name, args)) = parse_tool_call(&reply) {
            let impl_tool = tools.iter().find(|t| t.name == name);
            append_message(c, run_id, "assistant", &reply, Some(&name))?;
            // 分级权限：超出授予等级的调用直接拒绝，拒绝记录进 transcript。
            if !tier_allows(agent, &name) {
                let denial = format!(
                    "权限拒绝：{name} 需要 {} 级权限，本智能体只授予 {} 级（read<write<network）。如确有必要，在智能体设置里调高权限等级后重试。",
                    tool_tier(&name),
                    crate::agents::normalize_tier(&agent.tool_tier)
                );
                append_message(c, run_id, "tool", &denial, Some(&name))?;
                continue;
            }
            let output = if let Some(t) = impl_tool {
                match (t.run)(&ctx, &args) {
                    Ok(o) => o,
                    Err(e) => format!("工具执行失败：{e}"),
                }
            } else if let Some(mt) = mcp_tools.iter().find(|t| t.name == name) {
                // MCP tool: lazily boot the owning server's session, then call.
                let output = (|| -> std::result::Result<String, String> {
                    if !mcp_sessions.contains_key(&mt.server) {
                        let cfg = mcp_cfgs.get(&mt.server).ok_or_else(|| {
                            format!("MCP server {} 未在设置中配置", mt.server)
                        })?;
                        if !cfg.enabled {
                            return Err(format!("MCP server {} 已停用", mt.server));
                        }
                        let session = crate::mcp_client::McpSession::spawn(cfg)
                            .map_err(|e| e.to_string())?;
                        mcp_sessions.insert(mt.server.clone(), session);
                    }
                    mcp_sessions
                        .get_mut(&mt.server)
                        .expect("session just inserted")
                        .call_tool(&mt.tool, args)
                        .map_err(|e| e.to_string())
                })();
                match output {
                    Ok(o) => o,
                    Err(e) => format!("MCP 工具执行失败：{e}"),
                }
            } else {
                format!(
                    "没有名为 {name} 的可用工具。可用：{}",
                    tools
                        .iter()
                        .map(|t| t.name)
                        .chain(mcp_tools.iter().map(|t| t.name.as_str()))
                        .collect::<Vec<_>>()
                        .join("、")
                )
            };
            append_message(c, run_id, "tool", &snippet(&output, TOOL_RESULT_CHARS), Some(&name))?;
            continue;
        }

        // Anything else is the final answer (with or without the prefix).
        let answer = reply
            .trim()
            .strip_prefix("ANSWER:")
            .map(str::trim)
            .unwrap_or(reply.trim())
            .to_string();
        append_message(c, run_id, "assistant", &answer, None)?;
        final_answer = Some(answer);
        break;
    }

    match (final_answer, run_error) {
        (Some(a), _) => {
            finish_run(c, run_id, "success", Some(&a), None)?;
        }
        (None, Some(e)) => {
            finish_run(c, run_id, "error", None, Some(&e))?;
        }
        // Loop exhausted without an answer — not an error of the platform,
        // but an honest "hit the ceiling" result.
        (None, None) => {
            let msg = "已达最大步数，未给出最终回答。考虑提高 max_steps 或收窄任务。".to_string();
            finish_run(c, run_id, "error", None, Some(&msg))?;
        }
    }
    crate::agents::list_runs(c, user_id, 1)
        .ok()
        .and_then(|v| v.into_iter().find(|r| r.id == run_id))
        .ok_or_else(|| crate::error::Error::NotFound(format!("run {run_id}")))
    // list_runs orders by started_at DESC — the just-finished run is the
    // newest for this user, so the find() above is exact, not heuristic.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{list_messages, start_run, upsert_agent};

    /// A scripted Llm: returns queued replies, recording every prompt.
    struct Scripted {
        replies: std::sync::Mutex<std::collections::VecDeque<String>>,
        prompts: std::sync::Mutex<Vec<String>>,
    }
    impl Llm for Scripted {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            self.prompts.lock().unwrap().push(user.to_string());
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| crate::error::Error::Other("script exhausted".into()))
        }
        fn name(&self) -> String {
            "scripted".into()
        }
    }

    fn agent_with_tools(c: &Connection, tools: &[&str]) -> Agent {
        let tool_names: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
        upsert_agent(
            c,
            crate::agents::DEFAULT_USER,
            None,
            "研究员",
            None,
            "你是一个研究助手。",
            &[],
            &tool_names,
            None,
            0.3,
            6,
            true,
            "network",
        )
        .unwrap()
    }

    /// Tool bodies need a context; tests run without a data dir (nothing is
    /// mirrored to disk) and with a fixed run id (revisions are attributable).
    fn tctx<'a>(c: &'a Connection) -> ToolCtx<'a> {
        ToolCtx::new(c, None, "test-run")
    }

    fn agent_with(c: &Connection, tools: &[&str], skills: &[&str], max_steps: i64) -> Agent {
        upsert_agent(
            c,
            crate::agents::DEFAULT_USER,
            None,
            "研究员",
            None,
            "你是一个研究助手。",
            &skills.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &tools.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            None,
            0.3,
            max_steps,
            true,
            "read",
        )
        .unwrap()
    }

    #[test]
    fn tier_gate_blocks_network_for_read_agents() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let ro = upsert_agent(&c, crate::agents::DEFAULT_USER, None, "RO", None, "p", &[], &[], None, 0.3, 5, true, "read").unwrap();
        let net = upsert_agent(&c, crate::agents::DEFAULT_USER, None, "NET", None, "p", &[], &[], None, 0.3, 5, true, "network").unwrap();
        assert_eq!(ro.tool_tier, "read");
        assert_eq!(net.tool_tier, "network");
        // built-ins are read-tier: everyone may call them
        assert!(tier_allows(&ro, "list_notebooks"));
        assert!(tier_allows(&ro, "search_notebook"));
        // the 0076 writers are write-tier: denied for read agents
        assert_eq!(tool_tier("save_note"), "write");
        assert_eq!(tool_tier("save_flashcards"), "write");
        assert!(!tier_allows(&ro, "save_note"));
        // MCP tools are network-tier: read agents are denied, network agents allowed
        assert_eq!(tool_tier("mcp__kb__search"), "network");
        assert!(!tier_allows(&ro, "mcp__kb__search"));
        assert!(tier_allows(&net, "mcp__kb__search"));
        // a write-tier agent is granted read+write but not network
        let wr = upsert_agent(&c, crate::agents::DEFAULT_USER, None, "WR", None, "p", &[], &[], None, 0.3, 5, true, "write").unwrap();
        assert!(tier_allows(&wr, "list_notebooks"));
        assert!(tier_allows(&wr, "save_note"));
        assert!(!tier_allows(&wr, "mcp__kb__search"));
    }

    #[test]
    fn write_tools_create_notes_and_flashcard_decks() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let s = repo::insert_subject(&c, "线代", Some("math"), Some("◆"), None).unwrap();
        let t = repo::insert_topic(&c, &s, "特征值", None, &[]).unwrap();

        // save_note: full placement path
        let out = tool_save_note(&tctx(&c), &serde_json::json!({
            "title": "特征值笔记", "body": "特征值是不变方向。",
            "subject_id": s, "topic_id": t,
        }))
        .unwrap();
        assert!(out.contains("已保存笔记"));
        let notes = repo::list_notes(&c, Some(&s)).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "特征值笔记");
        assert_eq!(notes[0].topic_id.as_deref(), Some(t.as_str()));

        // save_flashcards: same payload shape the UI saves
        let out = tool_save_flashcards(&tctx(&c), &serde_json::json!({
            "subject_id": s, "title": "Agent 卡组",
            "cards": [
                {"front": "特征值是什么", "back": "不变方向"},
                {"front": "", "back": "空卡应被丢弃"},
                {"front": "对角化", "back": "分解为特征对"},
            ],
        }))
        .unwrap();
        assert!(out.contains("已保存 2 张卡片"));
        let mats = repo::list_materials(&c, &s).unwrap();
        let deck = mats.iter().find(|m| m.kind == "flashcards").expect("deck saved");
        assert_eq!(deck.title, "Agent 卡组");
        let payload = deck.payload.as_array().unwrap();
        assert_eq!(payload.len(), 2);
        assert_eq!(payload[0]["q"], "特征值是什么");

        // validation: topic/subject mismatch, missing cards, bad subject
        let out = tool_save_note(&tctx(&c), &serde_json::json!({"title": "t", "body": "b", "subject_id": "nope"})).unwrap();
        assert!(out.contains("未保存") || out.contains("失败"), "{out}");
        let out = tool_save_flashcards(&tctx(&c), &serde_json::json!({"subject_id": s, "cards": []})).unwrap();
        assert!(out.contains("没有一张完整"));
        let out = tool_save_flashcards(&tctx(&c), &serde_json::json!({"subject_id": s, "cards": [{"front": "a"}]})).unwrap();
        assert!(out.contains("没有一张完整"));
    }

    #[test]
    fn resolve_tools_hides_write_tools_from_read_agents() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let both: Vec<String> = vec!["search_notebook".into(), "save_note".into()];
        let ro = upsert_agent(&c, crate::agents::DEFAULT_USER, None, "RO", None, "p", &[], &both, None, 0.3, 5, true, "read").unwrap();
        let wr = upsert_agent(&c, crate::agents::DEFAULT_USER, None, "WR", None, "p", &[], &both, None, 0.3, 5, true, "write").unwrap();
        let ro_tools = resolve_tools(&c, crate::agents::DEFAULT_USER, &ro).unwrap();
        let wr_tools = resolve_tools(&c, crate::agents::DEFAULT_USER, &wr).unwrap();
        assert_eq!(ro_tools.len(), 1, "read agent never sees the whitelisted write tool");
        assert_eq!(ro_tools[0].name, "search_notebook");
        assert_eq!(wr_tools.len(), 2, "write agent gets both");
        // and the read agent's prompt carries no save_note usage line
        let sp = system_prompt(&ro_tools, &[], "", &ro);
        assert!(!sp.contains("save_note"));
        assert!(sp.contains("read"));
    }

    #[test]
    fn write_agent_persists_a_note_inside_the_loop() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let s = repo::insert_subject(&c, "线代", Some("math"), Some("◆"), None).unwrap();
        let agent = upsert_agent(
            &c, crate::agents::DEFAULT_USER, None, "书记员", None, "把用户口述整理成笔记并保存。",
            &[], &["save_note".to_string()], None, 0.3, 6, true, "write",
        )
        .unwrap();
        let run = start_run(&c, crate::agents::DEFAULT_USER, &agent.id, "记一下：逆矩阵存在当且仅当行列式非零").unwrap();
        let tool_reply = format!(
            r#"TOOL save_note {{"title": "逆矩阵条件", "body": "逆矩阵存在当且仅当行列式非零。", "subject_id": "{}"}}"#,
            s
        );
        let scripted = Scripted {
            replies: std::sync::Mutex::new(
                vec![tool_reply, "ANSWER: 已保存。".to_string()].into_iter().collect(),
            ),
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = execute_run(&c, crate::agents::DEFAULT_USER, &agent, &run.id, &scripted, None).unwrap();
        assert_eq!(done.status, "success");
        let notes = repo::list_notes(&c, Some(&s)).unwrap();
        assert_eq!(notes.len(), 1, "the agent's tool call wrote a real note");
        assert!(notes[0].body.contains("行列式非零"));
        // audit trail: tool call + tool confirmation in the transcript
        let msgs = list_messages(&c, crate::agents::DEFAULT_USER, &run.id, 50).unwrap();
        let tool_msgs: Vec<_> = msgs.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(tool_msgs.len(), 1);
        assert!(tool_msgs[0].content.contains("已保存笔记"));
        assert_eq!(msgs[1].tool_name.as_deref(), Some("save_note"));
    }

    #[test]
    fn tool_call_loop_answers_from_the_kb() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        // Seed one searchable chunk.
        let s = repo::insert_subject(&c, "线代", Some("math"), Some("◆"), None).unwrap();
        let src = repo::insert_source(&c, &s, None, "特征值讲义.md", "md", None).unwrap();
        repo::insert_chunk(
            &c, &src, &s, None, 0,
            "特征值描述线性变换的不变方向，对角化把矩阵分解为特征值与特征向量。",
            None, 10, &crate::vector::f32s_to_blob(&[0.0; 4]), "",
        )
        .unwrap();

        let agent = agent_with_tools(&c, &["search_notebook"]);
        let run = start_run(&c, crate::agents::DEFAULT_USER, &agent.id, "特征值是什么？").unwrap();
        let scripted = Scripted {
            replies: std::sync::Mutex::new(
                vec![
                    r#"TOOL search_notebook {"query": "特征值"}"#,
                    "ANSWER: 特征值描述线性变换的不变方向，见《特征值讲义.md》。",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = execute_run(&c, crate::agents::DEFAULT_USER, &agent, &run.id, &scripted, None).unwrap();
        assert_eq!(done.status, "success");
        assert!(done.result.unwrap().contains("特征值讲义"));
        let msgs = list_messages(&c, crate::agents::DEFAULT_USER, &run.id, 50).unwrap();
        // user, assistant(tool call), tool(result), assistant(answer)
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[2].role, "tool");
        assert!(msgs[2].content.contains("特征值"));
        // The second prompt must contain the tool result (loop continuity).
        assert!(scripted.prompts.lock().unwrap()[1].contains("[工具结果]"));
    }

    #[test]
    fn unknown_tool_is_reported_and_the_loop_continues() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let agent = agent_with_tools(&c, &["search_notebook"]);
        let run = start_run(&c, crate::agents::DEFAULT_USER, &agent.id, "hi").unwrap();
        let scripted = Scripted {
            replies: std::sync::Mutex::new(
                vec![
                    r#"TOOL delete_everything {}"#,
                    "ANSWER: 好的。",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = execute_run(&c, crate::agents::DEFAULT_USER, &agent, &run.id, &scripted, None).unwrap();
        assert_eq!(done.status, "success");
        let msgs = list_messages(&c, crate::agents::DEFAULT_USER, &run.id, 50).unwrap();
        let tool_msgs: Vec<_> = msgs.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(tool_msgs.len(), 1);
        assert!(tool_msgs[0].content.contains("没有名为 delete_everything"));
    }

    #[test]
    fn exhausted_steps_finish_the_run_with_an_error() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        // max_steps=1: the single step is consumed by a tool call, so the loop
        // ends without an ANSWER — an honest ceiling, not a crash.
        let agent = agent_with(&c, &["search_notebook"], &[], 1);
        let run = start_run(&c, crate::agents::DEFAULT_USER, &agent.id, "hi").unwrap();
        let scripted = Scripted {
            replies: std::sync::Mutex::new(
                vec![r#"TOOL search_notebook {"query": "x"}"#].into_iter().map(str::to_string).collect(),
            ),
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = execute_run(&c, crate::agents::DEFAULT_USER, &agent, &run.id, &scripted, None).unwrap();
        assert_eq!(done.status, "error");
        assert!(done.error.unwrap().contains("最大步数"));
    }

    #[test]
    fn parse_tool_call_tolerates_prose_and_missing_braces() {
        let (name, args) =
            parse_tool_call(r#"TOOL search_notebook {"query": "特征值"} 我先查一下。"#).unwrap();
        assert_eq!(name, "search_notebook");
        assert_eq!(args["query"], "特征值");
        assert!(parse_tool_call("TOOL noargs").is_none());
        assert!(parse_tool_call("ANSWER: done").is_none());
    }

    #[test]
    fn skills_and_tools_land_in_the_system_prompt() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        crate::agents::upsert_skill(
            &c, crate::agents::DEFAULT_USER, None, "引用优先", "cite", "回答必须引用来源文件名。", true,
        )
        .unwrap();
        let agent = agent_with(&c, &["search_notebook"], &["引用优先"], 6);
        let tools = resolve_tools(&c, crate::agents::DEFAULT_USER, &agent).unwrap();
        assert_eq!(tools.len(), 1);
        let sp = system_prompt(&tools, &[], &skill_bodies(&c, crate::agents::DEFAULT_USER, &agent).unwrap(), &agent);
        assert!(sp.contains("search_notebook"));
        assert!(sp.contains("回答必须引用来源文件名"));
        assert!(sp.contains("ANSWER"));
    }

    #[test]
    fn mcp_tools_resolve_and_inject_into_the_prompt() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let now = crate::db::now_ms();
        let usage = crate::mcp_client::usage_for(
            &crate::mcp_client::mcp_tool_name("homelab", "query_db"),
            "Query the homelab database.",
            &serde_json::json!({"type":"object","properties":{"sql":{"type":"string"}},"required":["sql"]}),
        );
        for (name, kind, enabled) in [
            (crate::mcp_client::mcp_tool_name("homelab", "query_db"), "mcp", 1),
            (crate::mcp_client::mcp_tool_name("fs", "read_file"), "mcp", 1),
            (crate::mcp_client::mcp_tool_name("off", "dead"), "mcp", 0), // disabled server
            ("list_notebooks".to_string(), "read", 1),
        ] {
            // description carries the usage line in the real sync flow
            let desc = if name == "mcp__homelab__query_db" { usage.clone() } else { "x".into() };
            c.execute(
                "INSERT OR IGNORE INTO agent_tools (id, user_id, name, description, kind, schema_json, enabled, built_in, created_at)
                 VALUES (?1,?2,?3,?4,?5,'{}',?6,0,?7)",
                params![crate::db::new_id(), crate::agents::DEFAULT_USER, name, desc, kind, enabled, now],
            )
            .unwrap();
        }
        // agent whitelists one MCP tool + one builtin; the disabled one must not resolve
        let agent = agent_with(&c, &["mcp__homelab__query_db", "list_notebooks"], &[], 4);
        let mcp = resolve_mcp_tools(&c, crate::agents::DEFAULT_USER, &agent).unwrap();
        assert_eq!(mcp.len(), 1, "only the whitelisted+enabled MCP tool resolves");
        assert_eq!(mcp[0].server, "homelab");
        assert_eq!(mcp[0].tool, "query_db");
        let tools = resolve_tools(&c, crate::agents::DEFAULT_USER, &agent).unwrap();
        let sp = system_prompt(&tools, &mcp, "", &agent);
        assert!(sp.contains("mcp__homelab__query_db"), "MCP usage lands in prompt");
        assert!(sp.contains(r#""sql": string*"#), "schema summary in usage: {}", sp);
        assert!(sp.contains("list_notebooks"));
    }

    // ------------------------------------------------- note read / edit

    fn note_fixture(c: &Connection, title: &str, body: &str) -> String {
        repo::insert_note(c, None, None, title, body).unwrap()
    }

    #[test]
    fn update_note_rewrites_one_place_and_keeps_the_previous_text() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = note_fixture(&c, "极限", "极限是局部的。\nε-δ 定义说明了趋近。");
        let out = tool_update_note(
            &tctx(&c),
            &serde_json::json!({
                "note_id": id, "find": "极限是局部的。", "replace": "极限只关心某点附近。",
            }),
        )
        .unwrap();
        assert!(out.contains("替换 1 处"), "{out}");
        assert!(out.contains("撤销"), "the receipt must name the undo path: {out}");

        let note = repo::get_note(&c, &id).unwrap();
        assert!(note.body.contains("某点附近"));
        assert!(note.body.contains("ε-δ 定义"), "the rest of the note is untouched");

        // The pre-edit text survives, attributed to the run that replaced it.
        let revs = repo::note_revisions(&c, &id, 10).unwrap();
        assert_eq!(revs.len(), 1);
        assert!(revs[0].body.contains("极限是局部的。"));
        assert_eq!(revs[0].actor.as_deref(), Some("agent:test-run"));
    }

    #[test]
    fn update_note_refuses_an_ambiguous_find_instead_of_guessing() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = note_fixture(&c, "重复", "同一句。\n同一句。");
        let out = tool_update_note(
            &tctx(&c),
            &serde_json::json!({"note_id": id, "find": "同一句。", "replace": "改掉的。"}),
        )
        .unwrap();
        assert!(out.contains("出现 2 次"), "{out}");
        assert_eq!(repo::get_note(&c, &id).unwrap().body, "同一句。\n同一句。");
        assert!(repo::note_revisions(&c, &id, 10).unwrap().is_empty(), "a refusal writes nothing, not even a revision");
    }

    #[test]
    fn update_note_refuses_a_find_the_model_misremembered() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = note_fixture(&c, "记忆", "原文是这样写的。");
        let out = tool_update_note(
            &tctx(&c),
            &serde_json::json!({"note_id": id, "find": "原文似乎是这样写", "replace": "x"}),
        )
        .unwrap();
        assert!(out.contains("找不到"), "{out}");
        assert_eq!(repo::get_note(&c, &id).unwrap().body, "原文是这样写的。");
    }

    #[test]
    fn update_note_accepts_exactly_one_way_of_saying_what_changed() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = note_fixture(&c, "两种", "正文");
        let out = tool_update_note(
            &tctx(&c),
            &serde_json::json!({"note_id": id, "body": "重写", "find": "正文", "replace": "改"}),
        )
        .unwrap();
        assert!(out.contains("只给一种改法"), "{out}");
        let out = tool_update_note(&tctx(&c), &serde_json::json!({"note_id": id})).unwrap();
        assert!(out.contains("只给一种改法"), "{out}");
        let out = tool_update_note(&tctx(&c), &serde_json::json!({"note_id": id, "find": "正文"})).unwrap();
        assert!(out.contains("成对出现"), "{out}");
        assert_eq!(repo::get_note(&c, &id).unwrap().body, "正文");
    }

    #[test]
    fn update_note_can_undo_its_own_edit() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = note_fixture(&c, "可撤销", "原始正文");
        tool_update_note(
            &tctx(&c),
            &serde_json::json!({"note_id": id, "find": "原始正文", "replace": "被改坏的正文"}),
        )
        .unwrap();
        let rev = repo::note_revisions(&c, &id, 1).unwrap().remove(0);

        let out = tool_update_note(
            &tctx(&c),
            &serde_json::json!({"note_id": id, "restore_revision": rev.id}),
        )
        .unwrap();
        assert!(out.contains("撤销到修订"), "{out}");
        assert_eq!(repo::get_note(&c, &id).unwrap().body, "原始正文");

        // The undo is itself an edit: the bad version is still recoverable.
        let revs = repo::note_revisions(&c, &id, 10).unwrap();
        assert_eq!(revs.len(), 2);
        assert_eq!(revs[0].body, "被改坏的正文");
    }

    #[test]
    fn update_note_refuses_a_revision_from_another_note() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = note_fixture(&c, "甲", "甲的正文");
        let b = note_fixture(&c, "乙", "乙的正文");
        tool_update_note(
            &tctx(&c),
            &serde_json::json!({"note_id": a, "find": "甲的正文", "replace": "甲改"}),
        )
        .unwrap();
        let rev_a = repo::note_revisions(&c, &a, 1).unwrap().remove(0);

        let out = tool_update_note(
            &tctx(&c),
            &serde_json::json!({"note_id": b, "restore_revision": rev_a.id}),
        )
        .unwrap();
        assert!(out.contains("不属于这条笔记"), "{out}");
        assert_eq!(repo::get_note(&c, &b).unwrap().body, "乙的正文");
    }

    #[test]
    fn get_note_refuses_a_title_two_notes_share() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        note_fixture(&c, "同名", "第一份");
        note_fixture(&c, "同名", "第二份");
        let out = tool_get_note(&tctx(&c), &serde_json::json!({"title": "同名"})).unwrap();
        assert!(out.contains("无法确定"), "{out}");
        // A unique title is enough — the id is not required when it is unambiguous.
        let one = note_fixture(&c, "唯一", "只有这一份");
        let out = tool_get_note(&tctx(&c), &serde_json::json!({"title": "唯一"})).unwrap();
        assert!(out.contains("只有这一份") && out.contains(&one), "{out}");
    }

    #[test]
    fn get_note_admits_when_it_only_shows_the_head() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let long: String = "长".repeat(3000);
        let id = note_fixture(&c, "长笔记", &long);
        let out = tool_get_note(&tctx(&c), &serde_json::json!({"note_id": id})).unwrap();
        assert!(out.contains("共 3000 字"), "{out}");
        assert!(out.contains("只显示了前 1800 字"), "a clipped read must say so: {out}");
    }

    #[test]
    fn update_note_writes_the_vault_file_not_just_the_row() {
        let dir = std::env::temp_dir().join(format!("cortex-agent-note-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = note_fixture(&c, "极限", "旧正文");
        let ctx = ToolCtx::new(&c, Some(&dir), "run-1");
        let out = tool_update_note(
            &ctx,
            &serde_json::json!({"note_id": id, "find": "旧正文", "replace": "新正文"}),
        )
        .unwrap();
        assert!(!out.contains("写入失败"), "{out}");
        // The Markdown file is the note; a row-only edit would be undone by the
        // next reindex, so this is the assertion that matters.
        let file = dir.join("vault").join("notes").join("极限.md");
        assert!(file.exists(), "expected {file:?}");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("新正文") && !text.contains("旧正文"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn revisions_are_pruned_to_the_newest_twenty() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = note_fixture(&c, "剪枝", "正文");
        for i in 0..25 {
            repo::insert_note_revision(&c, &id, "剪枝", &format!("v{i}"), None, None).unwrap();
        }
        let revs = repo::note_revisions(&c, &id, 100).unwrap();
        assert_eq!(revs.len(), repo::NOTE_REVISION_KEEP as usize);
        assert_eq!(revs[0].body, "v24", "newest first");
        assert!(revs.iter().all(|r| r.body != "v0"), "the oldest were pruned");
    }

    #[test]
    fn the_loop_can_edit_a_note_it_just_read() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let id = note_fixture(&c, "口述笔记", "逆矩阵存在当且仅当行列式非零。");
        // Seeded by 0086 — if the registry rows are missing the tools never
        // resolve and the model is never told they exist.
        let agent = upsert_agent(
            &c,
            crate::agents::DEFAULT_USER,
            None,
            "编辑",
            None,
            "修正笔记里的笔误。",
            &[],
            &["get_note".to_string(), "update_note".to_string()],
            None,
            0.3,
            6,
            true,
            "write",
        )
        .unwrap();
        assert_eq!(
            resolve_tools(&c, crate::agents::DEFAULT_USER, &agent).unwrap().len(),
            2,
            "0086 must seed both tools into agent_tools"
        );
        let run = start_run(&c, crate::agents::DEFAULT_USER, &agent.id, "把「行列式」改成「det」").unwrap();
        let scripted = Scripted {
            replies: std::sync::Mutex::new(
                vec![
                    format!(r#"TOOL get_note {{"note_id": "{id}"}}"#),
                    format!(r#"TOOL update_note {{"note_id": "{id}", "find": "当且仅当行列式非零", "replace": "当且仅当 det 非零"}}"#),
                    "ANSWER: 已改。".to_string(),
                ]
                .into_iter()
                .collect(),
            ),
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = execute_run(&c, crate::agents::DEFAULT_USER, &agent, &run.id, &scripted, None).unwrap();
        assert_eq!(done.status, "success");
        assert_eq!(
            repo::get_note(&c, &id).unwrap().body,
            "逆矩阵存在当且仅当 det 非零。"
        );
        let msgs = list_messages(&c, crate::agents::DEFAULT_USER, &run.id, 50).unwrap();
        assert!(
            msgs.iter().any(|m| m.role == "tool" && m.content.contains("已改")),
            "the edit is in the audit trail"
        );
    }
}
