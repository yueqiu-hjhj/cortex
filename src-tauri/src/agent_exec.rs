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
use crate::error::{Error, Result};
use crate::llm::Llm;
use crate::models::Note;
use crate::repo;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

/// Tool output is truncated before it enters the transcript — a 200 KB PDF
/// dump would eat the context window of every following step.
const TOOL_RESULT_CHARS: usize = 2400;
/// File tools get a larger (still bounded) budget: a 2.4k-char read is useless
/// for code. Everything else keeps the tight default.
const FILE_RESULT_CHARS: usize = 12_000;
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
            BuiltinTool {
                name: "propose_note",
                usage: r#"propose_note {"mode": "new|append|replace", "title": "标题", "note_id": "追加/改写时必填", "subject": "学科名", "body": "正文", "rationale": "可选"} — 提交笔记草稿给用户审阅（write 权限）。不会直接写入：草稿以 diff 进入「待确认提案」，用户采纳后才写进笔记与 .md。"#,
                tier: "write",
                run: tool_propose_note,
            },
            BuiltinTool {
                name: "web_search",
                usage: r#"web_search {"query": "关键词", "categories": "可选，默认 general"} — 通过已配置的 SearXNG 搜互联网，返回标题/站点/链接/摘要（network 权限）。库外的问题才用它；引用要带 URL。"#,
                tier: "network",
                run: tool_web_search,
            },
            BuiltinTool {
                name: "fetch_page",
                usage: r#"fetch_page {"url": "https://…"} — 打开一个网页读正文（network 权限）；搜索摘要不够用、要引用原文时再读。"#,
                tier: "network",
                run: tool_fetch_page,
            },
            BuiltinTool {
                name: "read_file",
                usage: r#"read_file {"path": "src/lib.rs", "offset": 1, "limit": 400} — 读取工作目录内的文本文件，返回带行号的内容（files 权限）."#,
                tier: "files",
                run: tool_read_file,
            },
            BuiltinTool {
                name: "write_file",
                usage: r#"write_file {"path": "notes/a.md", "content": "…"} — 新建或整篇覆盖工作目录内的文件，父目录自动创建（files 权限）；改代码优先用 edit_file."#,
                tier: "files",
                run: tool_write_file,
            },
            BuiltinTool {
                name: "edit_file",
                usage: r#"edit_file {"path": "src/lib.rs", "old_string": "原文", "new_string": "新文"} — 精确替换一处（old_string 必须逐字一致且唯一，含缩进）（files 权限）."#,
                tier: "files",
                run: tool_edit_file,
            },
            BuiltinTool {
                name: "list_dir",
                usage: r#"list_dir {"path": "."} — 列出工作目录内某个目录的文件与子目录（files 权限）."#,
                tier: "files",
                run: tool_list_dir,
            },
            BuiltinTool {
                name: "search_files",
                usage: r#"search_files {"pattern": "关键词或正则", "path": ".", "glob": "*.rs", "limit": 50} — 在工作目录内递归搜索文件内容，返回 路径:行号: 片段（files 权限）."#,
                tier: "files",
                run: tool_search_files,
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

/// Write tool: propose a note draft for the user to accept. Same queue the MCP
/// client uses (0060), so both paths share one diff renderer and one apply
/// path — but unlike save_note this writes nothing now: the draft waits as a
/// pending proposal tagged with the run/thread that asked for it (0087), and
/// only the user's 采纳 turns it into a note plus a vault file.
fn tool_propose_note(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let c = ctx.conn;
    let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("new").trim();
    let kind = match mode {
        "new" => crate::proposals::NOTE_NEW,
        "append" => crate::proposals::NOTE_APPEND,
        "replace" => crate::proposals::NOTE_REPLACE,
        other => {
            return Ok(format!(
                "mode「{other}」无效 —— 只支持 new / append / replace，草稿未提交"
            ));
        }
    };
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    if body.trim().is_empty() {
        return Ok("缺少 body（要写入的正文），草稿未提交".into());
    }
    // Provenance: the run row is the only place that knows which shared thread
    // and agent the user was talking to. A missing row means a hand-built run
    // (tests); the proposal still queues, just without conversation linkage.
    let origin: Option<(String, String, String)> = c
        .query_row(
            "SELECT r.thread_id, r.agent_id, a.name FROM agent_runs r JOIN agents a ON a.id=r.agent_id WHERE r.id=?1",
            [ctx.run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let author = match &origin {
        Some((_, _, name)) => name.clone(),
        None => "agent".to_string(),
    };
    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let note_id = args
        .get("note_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    let subject = args
        .get("subject")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let rationale = args.get("rationale").and_then(|v| v.as_str()).unwrap_or("");

    let linkage = match &origin {
        Some((thread_id, agent_id, _)) => crate::proposals::Origin {
            run_id: Some(ctx.run_id),
            thread_id: Some(thread_id),
            agent_id: Some(agent_id),
        },
        None => crate::proposals::Origin {
            run_id: Some(ctx.run_id),
            thread_id: None,
            agent_id: None,
        },
    };
    let p = crate::proposals::create_with_origin(
        c,
        crate::proposals::Draft {
            kind,
            title,
            note_id,
            subject,
            text: body,
            rationale,
            author: &author,
        },
        linkage,
    )?;

    let target = match &p.note_id {
        Some(id) => format!("「{}」(note_id={id})", p.title),
        None => format!("新建笔记「{}」", p.title),
    };
    let mut out = format!(
        "已提交待确认提案 {}（status=pending，{mode} → {target}，+{} −{} 行）。",
        p.id, p.preview.added, p.preview.removed
    );
    out.push_str("\n注意：笔记与 .md 文件都还没有写入 —— 用户需要在智能体对话下方（或 笔记 → AI 提案）看到 diff 并点「采纳并写入」才会生效。");
    out.push_str("\n回复用户时请说明草稿待确认，不要说已经保存。");
    Ok(out)
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

/// Network tool: search the open web through the configured SearXNG. Same
/// backend and same offline guard as the Web view (commands::searxng_search), so
/// there is no second definition of "is the network allowed".
fn tool_web_search(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("").trim();
    if query.is_empty() {
        return Ok("缺少 query 参数，没有执行搜索".into());
    }
    let cats = args.get("categories").and_then(|v| v.as_str()).unwrap_or("general");
    let hits = match crate::commands::searxng_search(ctx.conn, query, cats) {
        Ok(v) => v,
        // Not an error the loop should die on: the model can explain the fix.
        Err(e) => {
            return Ok(format!(
                "联网检索不可用：{e}\n（需要先在 设置 → 数据与隐私 里配好 searxng_url；若离线模式开着，检索是被有意禁用的。）"
            ));
        }
    };
    if hits.is_empty() {
        return Ok(format!("SearXNG 没返回「{query}」的结果，换个说法或用更具体的关键词。"));
    }
    let mut out = format!("web_search「{query}」命中 {} 条：\n", hits.len());
    // The transcript truncates tool output at ~2.4k chars, so lead with what the
    // model can cite: host, title, url, and a one-line snippet.
    for h in hits.iter().take(8) {
        out.push_str(&format!(
            "- [{}] {}\n  {}\n  {}\n",
            h.host,
            snippet(&h.title, 90),
            h.url,
            snippet(h.snippet.replace('\n', " ").trim(), 220)
        ));
    }
    out.push_str("引用时写清来源域名与链接；需要原文细节再 fetch_page。");
    Ok(out)
}

/// Network tool: read one page's main text. Depth for the cases a search
/// snippet cannot answer ("what exactly does section 3 say?").
fn tool_fetch_page(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let _ = ctx;
    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("").trim();
    if url.is_empty() {
        return Ok("缺少 url 参数，没有打开任何页面".into());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Ok(format!("只接受 http/https 链接，收到的是「{url}」"));
    }
    match crate::commands::fetch_readable(url) {
        Ok(page) => {
            // Page text is mostly whitespace runs once tags are stripped; folding
            // them buys roughly a third more real content inside the transcript cap.
            let body: String = page
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let mut out = format!("# {}\n来源：{}\n", page.title, page.final_url);
            out.push_str(&snippet(&body, 2000));
            out.push_str("\n（正文已截断；引用这一页时请给出上面的链接。）");
            Ok(out)
        }
        Err(e) => Ok(format!("打开页面失败：{e}")),
    }
}
// ------------------------------------------------------- files (files tier)
//
// The Claude-Code-shaped tools: read, write, exact edit, list, search. Every
// path is resolved inside `agent_workspace` (a setting), canonicalized, and
// checked with `starts_with`, so `..` and symlinks cannot escape. No shell,
// no delete, no rename — and every call already lands in the transcript.

const FS_READ_CAP: u64 = 512 * 1024;
const FS_WRITE_CAP: usize = 1024 * 1024;
const FS_MAX_LINES: usize = 2000;
const FS_MAX_ENTRIES: usize = 500;
const FS_MAX_HITS: usize = 100;
const FS_WALK_LIMIT: usize = 20_000;

fn workspace_root(ctx: &ToolCtx) -> Result<std::path::PathBuf> {
    let raw = repo::get_setting(ctx.conn, "agent_workspace")?.unwrap_or_default();
    let raw = raw.trim().to_string();
    if raw.is_empty() {
        return Err(Error::Other(
            "还没有设置智能体工作目录：在「智能体」页顶部填一个目录并保存".into(),
        ));
    }
    let canon = std::path::PathBuf::from(&raw)
        .canonicalize()
        .map_err(|e| Error::Other(format!("工作目录不可用（{raw}）：{e}")))?;
    if !canon.is_dir() {
        return Err(Error::Other(format!("工作目录不是文件夹：{raw}")));
    }
    Ok(canon)
}

/// Existing paths are canonicalized; for a new file the parent is created and
/// canonicalized. Either way the result must still be under the root.
fn resolve_inside(ctx: &ToolCtx, raw: &str, for_write: bool) -> Result<std::path::PathBuf> {
    let root = workspace_root(ctx)?;
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::Other("缺少 path 参数".into()));
    }
    let joined = if std::path::Path::new(raw).is_absolute() {
        std::path::PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    let target = if joined.exists() {
        joined
            .canonicalize()
            .map_err(|e| Error::Other(format!("路径不可用：{e}")))?
    } else if for_write {
        let parent = joined
            .parent()
            .ok_or_else(|| Error::Other("路径没有父目录".into()))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Other(format!("创建目录失败：{e}")))?;
        let canon_parent = parent
            .canonicalize()
            .map_err(|e| Error::Other(format!("目录不可用：{e}")))?;
        let name = joined
            .file_name()
            .ok_or_else(|| Error::Other("路径缺少文件名".into()))?;
        canon_parent.join(name)
    } else {
        return Err(Error::Other(format!("文件不存在：{raw}")));
    };
    if !target.starts_with(&root) {
        return Err(Error::Other("拒绝访问工作目录之外的路径".into()));
    }
    Ok(target)
}

fn write_atomic(path: &std::path::Path, content: &str) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, content).map_err(|e| Error::Other(format!("写入失败：{e}")))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::Other(format!("替换失败：{e}")))
}

fn tool_read_file(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let path = resolve_inside(ctx, raw, false)?;
    let meta = std::fs::metadata(&path).map_err(|e| Error::Other(format!("读取失败：{e}")))?;
    if meta.len() > FS_READ_CAP {
        return Err(Error::Other(format!(
            "文件太大（{} KB，上限 {} KB）：先用 search_files 定位，或用 offset/limit 分段读",
            meta.len() / 1024,
            FS_READ_CAP / 1024
        )));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|_| Error::Other("不是文本文件（或不是 UTF-8）".into()))?;
    let total = text.lines().count();
    let offset = args
        .get("offset")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(FS_MAX_LINES as i64)
        .clamp(1, FS_MAX_LINES as i64) as usize;
    let mut out = format!("{} （共 {total} 行）\n", path.display());
    let mut shown = 0;
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&format!("{:>6}\t{}\n", i + 1, line));
        shown += 1;
    }
    if shown == 0 {
        out.push_str("(该范围内没有内容)\n");
    }
    Ok(out)
}

fn tool_write_file(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.len() > FS_WRITE_CAP {
        return Err(Error::Other(format!(
            "内容太大（{} KB，上限 {} KB）",
            content.len() / 1024,
            FS_WRITE_CAP / 1024
        )));
    }
    let path = resolve_inside(ctx, raw, true)?;
    write_atomic(&path, content)?;
    Ok(format!(
        "已写入 {}（{} 字节，{} 行）",
        path.display(),
        content.len(),
        content.lines().count()
    ))
}

fn tool_edit_file(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let old = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if old.is_empty() {
        return Err(Error::Other("old_string 不能为空".into()));
    }
    let path = resolve_inside(ctx, raw, false)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|_| Error::Other("不是文本文件（或不是 UTF-8）".into()))?;
    let count = text.matches(old).count();
    if count == 0 {
        return Err(Error::Other(
            "没有找到 old_string（必须逐字一致，含缩进与换行）".into(),
        ));
    }
    if count > 1 {
        return Err(Error::Other(format!(
            "old_string 出现 {count} 次，请多带几行上下文让它唯一"
        )));
    }
    let updated = text.replacen(old, new, 1);
    write_atomic(&path, &updated)?;
    Ok(format!("已修改 {}（替换 1 处）", path.display()))
}

fn tool_list_dir(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let path = resolve_inside(ctx, raw, false)?;
    if !path.is_dir() {
        return Err(Error::Other(format!("不是目录：{}", path.display())));
    }
    let mut entries: Vec<(String, bool, u64)> = Vec::new();
    let rd = std::fs::read_dir(&path).map_err(|e| Error::Other(format!("读取目录失败：{e}")))?;
    for e in rd.flatten() {
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        entries.push((
            e.file_name().to_string_lossy().to_string(),
            md.is_dir(),
            md.len(),
        ));
    }
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total = entries.len();
    let mut out = format!("{}（{total} 项）\n", path.display());
    for (name, is_dir, size) in entries.into_iter().take(FS_MAX_ENTRIES) {
        if is_dir {
            out.push_str(&format!("{name}/\n"));
        } else {
            out.push_str(&format!("{name}  {size} B\n"));
        }
    }
    if total > FS_MAX_ENTRIES {
        out.push_str(&format!("…（只显示前 {FS_MAX_ENTRIES} 项）\n"));
    }
    Ok(out)
}

fn looks_binary(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true,
    };
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

fn walk_files(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path) -> bool) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.')
            || matches!(
                name.as_str(),
                "node_modules" | "target" | "dist" | "build" | "__pycache__"
            )
        {
            continue;
        }
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let path = e.path();
        if md.is_dir() {
            walk_files(&path, f)?;
        } else if md.is_file() && !f(&path) {
            return Ok(());
        }
    }
    Ok(())
}

fn glob_to_regex(glob: &str) -> Result<regex::Regex> {
    let mut re = String::from("^");
    for ch in glob.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).map_err(|e| Error::Other(format!("glob 无效：{e}")))
}

fn tool_search_files(ctx: &ToolCtx, args: &serde_json::Value) -> Result<String> {
    let root = workspace_root(ctx)?;
    let raw_base = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let base = resolve_inside(ctx, raw_base, false)?;
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if pattern.is_empty() {
        return Err(Error::Other("缺少 pattern 参数".into()));
    }
    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => regex::Regex::new(&regex::escape(pattern))
            .map_err(|e| Error::Other(format!("搜索表达式无效：{e}")))?,
    };
    let glob = args.get("glob").and_then(|v| v.as_str()).unwrap_or("").trim();
    let glob_re = if glob.is_empty() {
        None
    } else {
        Some(glob_to_regex(glob)?)
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(50)
        .clamp(1, FS_MAX_HITS as i64) as usize;

    let mut hits: Vec<String> = Vec::new();
    let mut visited = 0usize;
    let mut truncated = false;
    walk_files(&base, &mut |path| {
        if hits.len() >= limit {
            truncated = true;
            return false;
        }
        visited += 1;
        if visited > FS_WALK_LIMIT {
            truncated = true;
            return false;
        }
        if let Some(g) = &glob_re {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !g.is_match(name) {
                return true;
            }
        }
        if looks_binary(path) {
            return true;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            return true;
        };
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string()
            .replace('\\', "/");
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                let snippet: String = line.trim().chars().take(200).collect();
                hits.push(format!("{rel}:{}: {snippet}", i + 1));
                if hits.len() >= limit {
                    truncated = true;
                    break;
                }
            }
        }
        true
    })?;

    if hits.is_empty() {
        return Ok(format!("没有匹配（搜索了 {visited} 个文件）"));
    }
    let mut out = format!("{} 处匹配（搜索了 {visited} 个文件）\n", hits.len());
    for h in &hits {
        out.push_str(h);
        out.push('\n');
    }
    if truncated {
        out.push_str("…（结果已截断，缩小 path/glob 或调大 limit）\n");
    }
    Ok(out)
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
        .filter(|t| t.enabled && matches!(t.kind.as_str(), "read" | "write" | "network" | "files"))
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
            let cap = if matches!(name.as_str(), "read_file" | "search_files" | "list_dir") {
                FILE_RESULT_CHARS
            } else {
                TOOL_RESULT_CHARS
            };
            append_message(c, run_id, "tool", &snippet(&output, cap), Some(&name))?;
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

    /// 0087: the built-in propose_note is the confirmation gate. It must queue
    /// a draft and write NOTHING — the note row, the vault file and the pending
    /// count only move when the user accepts.
    #[test]
    fn propose_note_queues_a_draft_without_touching_notes() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let before: i64 = c
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .unwrap();
        let out = tool_propose_note(
            &tctx(&c),
            &serde_json::json!({"mode": "new", "title": "TCP 笔记", "body": "# TCP\n三次握手"}),
        )
        .unwrap();
        assert!(out.contains("pending"), "reports pending: {out}");
        assert!(out.contains("还没有写入"), "must not claim a write: {out}");
        assert!(out.contains("不要说已经保存"), "{out}");
        let after: i64 = c
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, before, "nothing was written to notes");
        let p = crate::proposals::list(&c, Some(crate::proposals::PENDING), 10)
            .unwrap()
            .into_iter()
            .next()
            .expect("a queued draft");
        assert_eq!(p.title, "TCP 笔记");
        assert_eq!(p.status, crate::proposals::PENDING);
        assert!(p.preview.added > 0);

        // Only the accept path writes. Without a vault root the DB row still lands.
        let applied = crate::proposals::accept(&c, None, &p.id, false).unwrap();
        let note = repo::get_note(&c, &applied.note_id).unwrap();
        assert!(note.body.contains("三次握手"));
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            before + 1
        );
    }

    /// Provenance: an in-app draft remembers the run, the shared thread and the
    /// agent, so the conversation panel can find it (0087).
    #[test]
    fn propose_note_records_run_thread_and_agent() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let agent = agent_with_tools(&c, &["propose_note"]);
        let run = start_run(&c, crate::agents::DEFAULT_USER, &agent.id, "帮我起草").unwrap();
        // Simulate 0047 linkage: the run belongs to a shared conversation.
        c.execute(
            "UPDATE agent_runs SET thread_id=?1 WHERE id=?2",
            params!["thr-1", run.id],
        )
        .unwrap();
        tool_propose_note(
            &ToolCtx::new(&c, None, &run.id),
            &serde_json::json!({"mode": "new", "title": "带溯源的草稿", "body": "正文"}),
        )
        .unwrap();

        let by_run = crate::proposals::list_for_run(&c, Some(&run.id), None, 10).unwrap();
        assert_eq!(by_run.len(), 1, "found by run id");
        let p = &by_run[0];
        assert_eq!(p.run_id.as_deref(), Some(run.id.as_str()));
        assert_eq!(p.thread_id.as_deref(), Some("thr-1"));
        assert_eq!(p.agent_id.as_deref(), Some(agent.id.as_str()));
        assert_eq!(p.author, "研究员", "author is the agent name, not a bare id");

        let by_thread = crate::proposals::list_for_run(&c, None, Some("thr-1"), 10).unwrap();
        assert_eq!(by_thread.len(), 1, "found by thread id");
        assert!(crate::proposals::list_for_run(&c, Some("other-run"), None, 10)
            .unwrap()
            .is_empty());
    }

    /// MCP-queued drafts have no run: the columns stay NULL and the Notes inbox
    /// still lists them (0087 must not narrow the existing queue).
    #[test]
    fn mcp_style_drafts_have_no_run_linkage() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let p = crate::proposals::create(
            &c,
            crate::proposals::Draft {
                kind: crate::proposals::NOTE_NEW,
                title: "MCP 草稿",
                note_id: None,
                subject: None,
                text: "正文",
                rationale: "",
                author: "codex",
            },
        )
        .unwrap();
        assert!(p.run_id.is_none());
        assert!(p.thread_id.is_none());
        assert!(p.agent_id.is_none());
        assert_eq!(crate::proposals::pending_count(&c).unwrap(), 1);
    }

    /// Network tools are tier-gated like every other tool: a read-only agent
    /// that whitelists web_search must not see it in its prompt, and the
    /// dispatch gate must refuse the call if one is forced through.
    #[test]
    fn web_tools_are_invisible_to_read_and_write_agents() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let all = vec!["search_notebook".to_string(), "web_search".to_string(), "fetch_page".to_string()];
        let ro = upsert_agent(&c, crate::agents::DEFAULT_USER, None, "RO", None, "p", &[], &all, None, 0.3, 5, true, "read").unwrap();
        let wr = upsert_agent(&c, crate::agents::DEFAULT_USER, None, "WR", None, "p", &[], &all, None, 0.3, 5, true, "write").unwrap();
        let net = upsert_agent(&c, crate::agents::DEFAULT_USER, None, "NET", None, "p", &[], &all, None, 0.3, 5, true, "network").unwrap();

        let ro_names: Vec<&str> = resolve_tools(&c, crate::agents::DEFAULT_USER, &ro)
            .unwrap()
            .iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(ro_names, vec!["search_notebook"], "read agent sees no network tool");
        let wr_names: Vec<&str> = resolve_tools(&c, crate::agents::DEFAULT_USER, &wr)
            .unwrap()
            .iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(wr_names, vec!["search_notebook"], "write is below network");

        let net_names: Vec<&str> = resolve_tools(&c, crate::agents::DEFAULT_USER, &net)
            .unwrap()
            .iter()
            .map(|t| t.name)
            .collect();
        assert!(net_names.contains(&"web_search"), "network agent gets it: {net_names:?}");
        assert!(net_names.contains(&"fetch_page"), "{net_names:?}");

        // The dispatch gate is the real enforcement, independent of the prompt.
        assert!(!tier_allows(&ro, "web_search"));
        assert!(!tier_allows(&wr, "fetch_page"));
        assert!(tier_allows(&net, "web_search"));
        assert_eq!(tool_tier("web_search"), "network");
        assert_eq!(tool_tier("fetch_page"), "network");
    }

    /// A missing SearXNG URL must produce an explanation the model can relay,
    /// not a run-killing error and not a silent empty result.
    #[test]
    fn web_search_without_a_configured_instance_explains_itself() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let out = tool_web_search(&tctx(&c), &serde_json::json!({"query": "rust async runtime"})).unwrap();
        assert!(out.contains("联网检索不可用"), "{out}");
        assert!(out.contains("searxng_url"), "names the setting to fix: {out}");
        let empty = tool_web_search(&tctx(&c), &serde_json::json!({})).unwrap();
        assert!(empty.contains("缺少 query"), "{empty}");
    }

    /// Offline mode is an explicit user decision; a network tool must honour it
    /// even though the agent has network tier.
    #[test]
    fn web_search_refuses_while_offline_mode_is_on() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        repo::set_setting(&c, "offline_mode", "true").unwrap();
        let out = tool_web_search(&tctx(&c), &serde_json::json!({"query": "anything"})).unwrap();
        assert!(out.contains("离线"), "offline mode is honoured: {out}");
    }

    /// fetch_page refuses anything that is not http(s) before opening a socket.
    #[test]
    fn fetch_page_rejects_non_http_urls() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let out = tool_fetch_page(&tctx(&c), &serde_json::json!({"url": "file:///etc/passwd"})).unwrap();
        assert!(out.contains("只接受 http/https"), "{out}");
        let out = tool_fetch_page(&tctx(&c), &serde_json::json!({})).unwrap();
        assert!(out.contains("缺少 url"), "{out}");
    }
    // ---- files tier: workspace containment, round trip, search -------------

    fn workspace(c: &Connection) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cortex-fs-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        repo::set_setting(c, "agent_workspace", dir.to_str().unwrap()).unwrap();
        dir
    }

    #[test]
    fn file_tools_roundtrip_inside_the_workspace() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let dir = workspace(&c);
        let ctx = tctx(&c);

        tool_write_file(
            &ctx,
            &serde_json::json!({"path": "a/b.txt", "content": "hello\nworld\n"}),
        )
        .unwrap();
        let read = tool_read_file(&ctx, &serde_json::json!({"path": "a/b.txt"})).unwrap();
        assert!(read.contains("1\thello"), "{read}");

        tool_edit_file(
            &ctx,
            &serde_json::json!({"path": "a/b.txt", "old_string": "world", "new_string": "there"}),
        )
        .unwrap();
        let read = tool_read_file(&ctx, &serde_json::json!({"path": "a/b.txt"})).unwrap();
        assert!(read.contains("there"), "{read}");

        let listing = tool_list_dir(&ctx, &serde_json::json!({"path": "."})).unwrap();
        assert!(listing.contains("a/"), "{listing}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_tools_refuse_paths_outside_the_workspace() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let dir = workspace(&c);
        let ctx = tctx(&c);

        let outside = dir
            .parent()
            .unwrap()
            .join(format!("cortex-outside-{}.txt", crate::db::new_id()));
        std::fs::write(&outside, "secret").unwrap();
        let name = outside.file_name().unwrap().to_str().unwrap();
        let escaped = format!("../{name}");
        let err = tool_read_file(&ctx, &serde_json::json!({"path": escaped}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("工作目录之外"), "{err}");

        let err = tool_write_file(
            &ctx,
            &serde_json::json!({"path": "../escape.txt", "content": "x"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("工作目录之外"), "{err}");

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_files_finds_text_and_skips_binaries() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let dir = workspace(&c);
        let ctx = tctx(&c);

        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() { let needle = 1; }\n").unwrap();
        std::fs::write(dir.join("src/blob.bin"), [0u8, 1, 2, 3]).unwrap();

        let hits = tool_search_files(
            &ctx,
            &serde_json::json!({"pattern": "needle", "glob": "*.rs"}),
        )
        .unwrap();
        assert!(hits.contains("src/a.rs:1"), "{hits}");

        let none = tool_search_files(
            &ctx,
            &serde_json::json!({"pattern": "needle", "glob": "*.bin"}),
        )
        .unwrap();
        assert!(none.contains("没有匹配"), "{none}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_tools_are_registered_and_idempotently_seeded() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        crate::agents::seed_file_tools(&c).unwrap();
        let names: Vec<String> = crate::agents::list_tools(&c, crate::agents::DEFAULT_USER)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        for n in ["read_file", "write_file", "edit_file", "list_dir", "search_files"] {
            assert!(names.contains(&n.to_string()), "missing {n}");
        }
        assert_eq!(crate::agents::seed_file_tools(&c).unwrap(), 0, "seed must be idempotent");
    }

    #[test]
    fn files_is_the_top_tier_and_network_does_not_grant_it() {
        assert_eq!(crate::agents::normalize_tier("files"), "files");
        assert!(crate::agents::tier_rank("files") > crate::agents::tier_rank("network"));
    }

}
