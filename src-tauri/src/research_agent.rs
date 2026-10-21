//! 研究对话 — the literature, as a conversation.
//!
//! The app already had two ways to look for a paper. `literature_search` is a
//! query box: one string in, a ranked list out. `literature_ai_search` is a
//! smarter query box: the model plans several angles and reranks, but it still
//! answers once and forgets. Neither can do what a researcher actually does —
//! ask "who is pushing this at Stanford?", hear an answer, and then say "show
//! me their 2025 work".
//!
//! This module is that loop. It is a real (bounded) agent: the model chooses
//! among five tools, reads what they return, and only then writes an answer.
//! The tools span the two halves of the user's world:
//!
//!   search_literature  OpenAlex, with author / institution / year filters
//!   find_authors       "which person, which group" — profiles, not papers
//!   my_library         the papers already in this app
//!   read_paper         abstract + your notes + your excerpts, with pages
//!   search_notes       your own writing, so the answer can quote you back
//!
//! Protocol is the proven line contract from `agent_exec` (models hold it far
//! better than a JSON envelope): `TOOL <name> {json}` or `ANSWER: <text>`,
//! one action per turn. Every tool call is persisted as a `tool` row, so the
//! evidence behind any sentence in the answer is still there tomorrow.

use crate::db::{new_id, now_ms, AppState};
use crate::error::{Error, Result};
use crate::llm::Llm;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use tauri::{Manager, State};

/// Tool calls per user turn. Six is enough for the natural sequence
/// find_authors → search_literature → read_paper without letting a confused
/// model burn the user's tokens in a circle.
const MAX_STEPS: usize = 6;
/// One tool result as shown to the model. Lists stay readable; a 200 KB dump
/// would eat the context of every following step.
const TOOL_RESULT_CHARS: usize = 2600;
/// Arguments are small by construction (a query, a name, an id); clip anyway
/// so a model that pastes a paragraph into an arg cannot bloat the table.
const TOOL_ARGS_CHARS: usize = 600;
/// How many prior messages ride along as conversation memory.
const HISTORY_MESSAGES: usize = 14;

const OPENALEX_WORKS: &str = "https://api.openalex.org/works";
const OPENALEX_AUTHORS: &str = "https://api.openalex.org/authors";
const UA: &str = "Cortex/1.0 (research chat)";

pub(crate) const SYSTEM: &str = r#"你是 Cortex 的研究助手。你能真的去查资料，而不是凭训练记忆回答。

可用工具（每次只输出一个动作）：
TOOL search_literature {"query":"主题词，可空","author":"作者名，可空","institution":"机构/团队，可空","year_from":2020,"sort":"relevance|citations|newest","limit":8}
TOOL find_authors {"name":"人名，可空","institution":"机构/团队，可空","topic":"方向，可空","limit":6}
TOOL my_library {"query":"关键词，可空","author":"作者，可空","status":"to_read|reading|read|cited，可空","limit":10}
TOOL read_paper {"paper_id":"…或 cite_key 或 title"}
TOOL search_notes {"query":"关键词","limit":8}

行为准则：
1. 用户问「谁在做 / 哪个团队 / 哪个人」→ 先 find_authors；拿到人之后用 search_literature 的 author 参数看他们的具体工作。
2. 用户问「我库里有吗 / 我记过什么」→ 先 my_library；要看细节再 read_paper。用户问「我笔记里写过什么」→ search_notes。
3. 除了打招呼，回答前至少调用一次工具。不要凭空列出论文——没查到就直说没查到。
4. 回答用中文，给出具体标题、年份、作者；引用库里论文时写出 cite_key，方便我直接引用。
5. 结尾如果要给建议，说清楚下一步能查什么（换关键词、换机构、看某人的近作）。
6. 输出格式严格二选一：
   TOOL <工具名> {单行 JSON}
   ANSWER: <最终回答>
不要输出别的解释性前缀。"#;

// ------------------------------------------------------------------ types

#[derive(Debug, Clone, Serialize)]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Number of user turns, so the picker can show "3 轮".
    pub turns: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: String,
    pub thread_id: String,
    pub role: String,
    pub text: String,
    pub tool: Option<String>,
    /// The arguments the model passed to the tool (JSON), clipped. Without
    /// it the transcript shows what came back but not what was asked.
    pub args: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AskOutcome {
    pub thread_id: String,
    pub title: String,
    pub reply: String,
    /// Tool calls this turn made — the UI shows "查了 2 次" instead of a lie.
    pub steps: usize,
    pub took_ms: i64,
}

// -------------------------------------------------------------- persistence

pub fn list_threads(c: &Connection, limit: usize) -> Result<Vec<Thread>> {
    let mut st = c.prepare(
        "SELECT t.id, t.title, t.created_at, t.updated_at, \
            (SELECT COUNT(*) FROM research_messages m WHERE m.thread_id = t.id AND m.role = 'user') \
         FROM research_threads t ORDER BY t.updated_at DESC LIMIT ?1",
    )?;
    let rows = st
        .query_map(params![limit.clamp(1, 200) as i64], |r| {
            Ok(Thread {
                id: r.get(0)?,
                title: r.get(1)?,
                created_at: r.get(2)?,
                updated_at: r.get(3)?,
                turns: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn thread_messages(c: &Connection, thread_id: &str, limit: usize) -> Result<Vec<Message>> {
    // Newest N, then reversed: the transcript must read top-down, and a
    // timestamp tie must not reorder two messages written in the same ms.
    let mut st = c.prepare(
        "SELECT id, thread_id, role, text, tool, args, created_at FROM research_messages \
         WHERE thread_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
    )?;
    let mut rows = st
        .query_map(params![thread_id, limit.clamp(1, 500) as i64], |r| {
            Ok(Message {
                id: r.get(0)?,
                thread_id: r.get(1)?,
                role: r.get(2)?,
                text: r.get(3)?,
                tool: r.get(4)?,
                args: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    Ok(rows)
}

fn ensure_thread(c: &Connection, thread_id: Option<&str>, first_input: &str) -> Result<Thread> {
    if let Some(id) = thread_id.filter(|s| !s.trim().is_empty()) {
        if let Some(t) = get_thread(c, id)? {
            return Ok(t);
        }
    }
    let id = new_id();
    let now = now_ms();
    let title = title_from(first_input);
    c.execute(
        "INSERT INTO research_threads (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![id, title, now],
    )?;
    Ok(Thread {
        id,
        title,
        created_at: now,
        updated_at: now,
        turns: 0,
    })
}

fn get_thread(c: &Connection, id: &str) -> Result<Option<Thread>> {
    let t = c
        .query_row(
            "SELECT id, title, created_at, updated_at FROM research_threads WHERE id = ?1",
            [id],
            |r| {
                Ok(Thread {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    created_at: r.get(2)?,
                    updated_at: r.get(3)?,
                    turns: 0,
                })
            },
        )
        .optional()?;
    Ok(t)
}

fn append_message(
    c: &Connection,
    thread_id: &str,
    role: &str,
    text: &str,
    tool: Option<&str>,
    args: Option<&str>,
) -> Result<()> {
    c.execute(
        "INSERT INTO research_messages (id, thread_id, role, text, tool, args, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![new_id(), thread_id, role, text, tool, args, now_ms()],
    )?;
    Ok(())
}

pub fn delete_thread(c: &Connection, id: &str) -> Result<()> {
    // Messages go with it (ON DELETE CASCADE); without the pragma the DELETE
    // would orphan rows, so clean up explicitly as well.
    c.execute("DELETE FROM research_messages WHERE thread_id = ?1", [id])?;
    c.execute("DELETE FROM research_threads WHERE id = ?1", [id])?;
    Ok(())
}

/// A thread title is a promise about the first question — 26 chars is what
/// fits the picker without breaking a word in half.
fn title_from(input: &str) -> String {
    let flat = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(26).collect();
    if flat.chars().count() > 26 {
        out.push('…');
    }
    if out.trim().is_empty() {
        "新对话".into()
    } else {
        out
    }
}

// ------------------------------------------------------------------- loop

/// One user turn: append the question, let the model call tools, append the
/// answer. `thread_id = None` starts a new thread.
pub fn ask(
    c: &Connection,
    model: &mut dyn Llm,
    thread_id: Option<&str>,
    input: &str,
) -> Result<AskOutcome> {
    let input = input.trim();
    if input.is_empty() {
        return Err(Error::Other("问题是空的。".into()));
    }
    let started = std::time::Instant::now();
    // Read once per turn: a mid-turn edit of the prompt in the lab should not
    // change the instructions the loop is already following.
    let sys = crate::prompts::text(crate::prompts::RESEARCH_CHAT);
    let thread = ensure_thread(c, thread_id, input)?;
    append_message(c, &thread.id, "user", input, None, None)?;
    let history = thread_messages(c, &thread.id, HISTORY_MESSAGES)?;

    let mut scratch = String::new();
    let mut steps = 0usize;
    let mut answer: Option<String> = None;

    for _ in 0..MAX_STEPS {
        let prompt = build_prompt(&history, &scratch);
        let reply = model.complete(&sys, &prompt)?;
        crate::tokens::record(
            c,
            "research",
            &model.name(),
            &format!("{sys}\n{prompt}"),
            &reply,
            None,
        );
        match parse_action(&reply) {
            Action::Tool(name, args) => {
                steps += 1;
                let out = run_tool(c, &name, &args);
                let kept = clip(&out, TOOL_RESULT_CHARS);
                // Store the ask alongside the answer: judging a tool call
                // means seeing both halves of it.
                let asked = clip(&args.to_string(), TOOL_ARGS_CHARS);
                append_message(c, &thread.id, "tool", &kept, Some(&name), Some(&asked))?;
                scratch.push_str(&format!("{steps}. {name} {asked} → {}\n", clip(&out, 900)));
            }
            Action::Answer(text) => {
                answer = Some(text);
                break;
            }
        }
    }

    let reply = match answer {
        Some(a) => a,
        None => {
            // Tools exhausted without an answer. Ask once more with the door
            // closed — an honest partial answer beats "已达最大步数".
            let prompt = format!(
                "{}\n\n工具调用次数已到上限。不要再调用工具，直接用已有结果回答；缺什么就说明缺什么。",
                build_prompt(&history, &scratch)
            );
            let last = model.complete(&sys, &prompt)?;
            let text = match parse_action(&last) {
                Action::Answer(t) => t,
                Action::Tool(..) => last.trim().to_string(),
            };
            format!("{text}\n\n（本次最多查 {MAX_STEPS} 次，想继续可以接着问。）")
        }
    };
    append_message(c, &thread.id, "agent", &reply, None, None)?;

    Ok(AskOutcome {
        thread_id: thread.id,
        title: thread.title,
        reply,
        steps,
        took_ms: started.elapsed().as_millis() as i64,
    })
}

fn build_prompt(history: &[Message], scratch: &str) -> String {
    let mut out = String::from("对话记录（从早到晚）：\n");
    for m in history {
        match m.role.as_str() {
            "user" => out.push_str(&format!("用户：{}\n", m.text)),
            "agent" => out.push_str(&format!("你：{}\n", m.text)),
            "tool" => out.push_str(&format!(
                "工具结果[{}]：{}\n",
                m.tool.as_deref().unwrap_or("?"),
                clip(&m.text, 500)
            )),
            _ => {}
        }
    }
    if !scratch.is_empty() {
        out.push_str("\n本轮已执行：\n");
        out.push_str(scratch);
    }
    out.push_str(
        "\n下一步：输出 `TOOL <工具名> {单行 JSON}` 继续查，或 `ANSWER: <最终回答>`。只输出一个。",
    );
    out
}

enum Action {
    Tool(String, Value),
    Answer(String),
}

/// Parse one model turn. A tool call wins only when it appears before any
/// `ANSWER:` — a final answer that mentions the word "TOOL" stays an answer.
fn parse_action(reply: &str) -> Action {
    let answer_at = reply.find("ANSWER:");
    let tool_at = find_tool_call(reply);
    match (tool_at, answer_at) {
        (Some((at, name, args)), Some(ans)) if at < ans => Action::Tool(name, args),
        (Some((_, name, args)), None) => Action::Tool(name, args),
        (_, Some(ans)) => Action::Answer(clean_answer(&reply[ans + "ANSWER:".len()..])),
        _ => Action::Answer(clean_answer(reply)),
    }
}

/// Locate `TOOL name {json}` at the start of a line and parse the whole
/// remaining reply — a pretty-printed JSON object spans several lines, so the
/// brace scan must not be limited to the line the call started on.
fn find_tool_call(reply: &str) -> Option<(usize, String, Value)> {
    let mut offset = 0usize;
    for line in reply.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("TOOL ") {
            let at = offset + (line.len() - trimmed.len());
            if let Some((name, args)) = parse_tool_body(&reply[at + "TOOL ".len()..]) {
                return Some((at, name, args));
            }
        }
        offset += line.len();
    }
    None
}

fn clean_answer(s: &str) -> String {
    s.trim()
        .trim_start_matches('：')
        .trim_start_matches(':')
        .trim()
        .to_string()
}

/// `name {"json"}` — tolerant of a fenced JSON block or pretty-printed braces.
fn parse_tool_body(rest: &str) -> Option<(String, Value)> {
    let brace = rest.find('{');
    let name = match brace {
        Some(i) => rest[..i].trim(),
        None => rest.trim(),
    };
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    let Some(start) = brace else {
        return Some((name.to_string(), Value::Null));
    };
    let bytes: Vec<char> = rest[start..].chars().collect();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = None;
    for (i, ch) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if *ch == '\\' {
                esc = true;
            } else if *ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let json: String = bytes[..end?].iter().collect();
    let args = serde_json::from_str(&json).unwrap_or(Value::Null);
    Some((name.to_string(), args))
}

// ------------------------------------------------------------------ tools

fn run_tool(c: &Connection, name: &str, args: &Value) -> String {
    let out = match name {
        "search_literature" => tool_search_literature(c, args),
        "find_authors" => tool_find_authors(args),
        "my_library" => tool_my_library(c, args),
        "read_paper" => tool_read_paper(c, args),
        "search_notes" => tool_search_notes(c, args),
        _ => {
            return format!(
                "没有名为 {name} 的工具。可用：search_literature、find_authors、my_library、read_paper、search_notes。"
            )
        }
    };
    match out {
        Ok(s) => s,
        Err(e) => format!("工具执行失败：{e}"),
    }
}

fn arg_str<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn arg_usize(a: &Value, k: &str, default: usize) -> usize {
    a.get(k)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(default)
}

fn tool_search_literature(c: &Connection, args: &Value) -> Result<String> {
    let query = arg_str(args, "query").unwrap_or("");
    let author = arg_str(args, "author");
    let institution = arg_str(args, "institution");
    let year_from = args.get("year_from").and_then(|v| v.as_i64());
    let sort = arg_str(args, "sort").unwrap_or("relevance");
    let limit = arg_usize(args, "limit", 8).clamp(1, 20);
    if query.is_empty() && author.is_none() && institution.is_none() {
        return Ok("至少要给 query、author 或 institution 之一。".into());
    }
    let url = works_url(query, author, institution, year_from, sort, limit);
    let json = http_json(&url)?;
    let results = json["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        return Ok(format!("OpenAlex 没有命中（{}）。换个更宽的关键词，或去掉机构限制。", url));
    }
    let mut out = String::from("OpenAlex 命中：\n");
    for (i, w) in results.iter().enumerate() {
        let title = w["display_name"].as_str().unwrap_or("(untitled)");
        let year = w["publication_year"]
            .as_i64()
            .map(|y| y.to_string())
            .unwrap_or_else(|| "n.d.".into());
        let venue = w["primary_location"]["source"]["display_name"]
            .as_str()
            .unwrap_or("");
        let doi = w["doi"]
            .as_str()
            .map(|d| d.trim_start_matches("https://doi.org/").to_string())
            .unwrap_or_default();
        let cited = w["cited_by_count"].as_i64().unwrap_or(0);
        let oa = w["open_access"]["oa_url"].as_str().unwrap_or("");
        let authors = join_authors(w, 4);
        let id = w["id"]
            .as_str()
            .map(|s| s.trim_start_matches("https://openalex.org/"))
            .unwrap_or("");
        let mine = in_library(c, &doi, title)?;
        out.push_str(&format!(
            "{}. 《{}》 ({}) {} · {} · 被引 {} · {} · {}{}\n",
            i + 1,
            title,
            year,
            if venue.is_empty() { "" } else { venue },
            authors,
            cited,
            if doi.is_empty() { format!("id={id}") } else { format!("doi={doi}") },
            if oa.is_empty() { String::new() } else { format!(" · OA: {oa}") },
            if mine { " · 库里已有" } else { "" }
        ));
    }
    Ok(out)
}

fn tool_find_authors(args: &Value) -> Result<String> {
    let name = arg_str(args, "name").unwrap_or("");
    let institution = arg_str(args, "institution");
    let topic = arg_str(args, "topic");
    let limit = arg_usize(args, "limit", 6).clamp(1, 12);
    let search = [name, topic.unwrap_or(""), institution.unwrap_or("")]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if search.trim().is_empty() {
        return Ok("给一个 name、institution 或 topic，我才能找人。".into());
    }
    let url = format!(
        "{OPENALEX_AUTHORS}?search={}&per_page={limit}&mailto=cortex@local",
        enc(&search)
    );
    let json = http_json(&url)?;
    let results = json["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        return Ok(format!("没找到和「{search}」匹配的作者。试试英文姓名或机构名（Stanford、清华）。"));
    }
    let mut out = String::from("OpenAlex 作者：\n");
    for (i, a) in results.iter().enumerate() {
        let name = a["display_name"].as_str().unwrap_or("(unknown)");
        let works = a["works_count"].as_i64().unwrap_or(0);
        let cited = a["cited_by_count"].as_i64().unwrap_or(0);
        let h = a["summary_stats"]["h_index"].as_i64().unwrap_or(0);
        let insts = a["last_known_institutions"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|x| x["display_name"].as_str())
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
        let topics = a["topics"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|x| x["display_name"].as_str())
                    .take(3)
                    .collect::<Vec<_>>()
                    .join("、")
            })
            .unwrap_or_default();
        let id = a["id"]
            .as_str()
            .map(|s| s.trim_start_matches("https://openalex.org/"))
            .unwrap_or("");
        let id_suffix = if id.is_empty() {
            String::new()
        } else {
            format!(" · id={id}")
        };
        out.push_str(&format!(
            "{}. {}（{}）· {} 篇 · 被引 {} · h={} · 方向：{}{}\n",
            i + 1,
            name,
            if insts.is_empty() { "机构未标注" } else { &insts },
            works,
            cited,
            h,
            if topics.is_empty() { "—" } else { &topics },
            id_suffix
        ));
    }
    out.push_str("\n想看他/他们最近的工作：search_literature 里把 author 设成上面的名字。");
    Ok(out)
}

fn tool_my_library(c: &Connection, args: &Value) -> Result<String> {
    let query = arg_str(args, "query").unwrap_or("");
    let author = arg_str(args, "author").unwrap_or("");
    let status = arg_str(args, "status").unwrap_or("");
    let limit = arg_usize(args, "limit", 10).clamp(1, 30) as i64;
    let like = |s: &str| format!("%{}%", s.replace('%', "\\%").replace('_', "\\_"));
    let q = like(query);
    let a = like(author);
    let mut st = c.prepare(
        "SELECT COALESCE(p.cite_key,''), p.id, p.title, COALESCE(p.authors,''), p.year, p.status, \
            COALESCE(p.abstract,''), p.notes, \
            (SELECT COUNT(*) FROM excerpts e WHERE e.paper_id = p.id) \
         FROM papers p \
         WHERE (?1 = '%%' OR p.title LIKE ?1 ESCAPE '\\' OR p.abstract LIKE ?1 ESCAPE '\\' \
                OR p.notes LIKE ?1 ESCAPE '\\' OR p.authors LIKE ?1 ESCAPE '\\') \
           AND (?2 = '%%' OR p.authors LIKE ?2 ESCAPE '\\') \
           AND (?3 = '' OR p.status = ?3) \
         ORDER BY p.updated_at DESC LIMIT ?4",
    )?;
    let rows: Vec<String> = st
        .query_map(params![q, a, status, limit], |r| {
            let key: String = r.get(0)?;
            let id: String = r.get(1)?;
            let title: String = r.get(2)?;
            let authors: String = r.get(3)?;
            let year: Option<i64> = r.get(4)?;
            let status: String = r.get(5)?;
            let abstract_: String = r.get(6)?;
            let notes: String = r.get(7)?;
            let excerpts: i64 = r.get(8)?;
            Ok(format!(
                "- {} | {} ({}) | {} | {} 条摘录\n  status={} · paper_id={}\n  {}\n",
                if key.is_empty() { id.clone() } else { key },
                title,
                year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into()),
                one_line(&authors, 60),
                excerpts,
                status,
                id,
                one_line(
                    if abstract_.trim().is_empty() { &notes } else { &abstract_ },
                    180
                )
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok("你的论文库里没有匹配的条目。".into());
    }
    Ok(format!("本地论文库（{} 条）：\n{}", rows.len(), rows.concat()))
}

fn tool_read_paper(c: &Connection, args: &Value) -> Result<String> {
    let key = arg_str(args, "paper_id")
        .or_else(|| arg_str(args, "cite_key"))
        .or_else(|| arg_str(args, "id"))
        .or_else(|| arg_str(args, "title"));
    let Some(key) = key else {
        return Ok("给一个 paper_id、cite_key 或 title。".into());
    };
    let row = c
        .query_row(
            "SELECT id, COALESCE(cite_key,''), title, COALESCE(authors,''), year, \
                COALESCE(venue,''), COALESCE(doi,''), COALESCE(abstract,''), \
                COALESCE(notes,''), status, COALESCE(source_id,'') \
             FROM papers WHERE id = ?1 OR lower(COALESCE(cite_key,'')) = lower(?1) \
                OR lower(title) = lower(?1) OR title LIKE ?2 LIMIT 1",
            params![key, format!("%{key}%")],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?;
    let Some((id, cite, title, authors, year, venue, doi, abstract_, notes, status, source)) = row
    else {
        return Ok(format!("本地库没有《{key}》。先用 my_library 或 search_literature 找一下。"));
    };
    let mut out = format!(
        "《{title}》\n作者：{}\n年份：{} · 期刊/会议：{} · DOI：{}\n状态：{} · cite_key：{} · paper_id：{}\n",
        if authors.is_empty() { "—" } else { &authors },
        year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into()),
        if venue.is_empty() { "—" } else { &venue },
        if doi.is_empty() { "—" } else { &doi },
        status,
        if cite.is_empty() { "—" } else { &cite },
        id
    );
    if !abstract_.trim().is_empty() {
        out.push_str(&format!("\n摘要：{}\n", clip(&abstract_, 1500)));
    }
    if !notes.trim().is_empty() {
        out.push_str(&format!("\n我的笔记：{}\n", clip(&notes, 700)));
    }
    if !source.is_empty() {
        out.push_str(&format!("\n全文已入库（source_id={source}），可在阅读器里看。\n"));
    }
    let mut st = c.prepare(
        "SELECT quote, page FROM excerpts WHERE paper_id = ?1 ORDER BY created_at DESC LIMIT 12",
    )?;
    let ex: Vec<String> = st
        .query_map([&id], |r| {
            let q: String = r.get(0)?;
            let p: Option<i64> = r.get(1)?;
            Ok(format!(
                "· {}{}",
                match p {
                    Some(n) => format!("p.{n}："),
                    None => String::new(),
                },
                one_line(&q, 200)
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if ex.is_empty() {
        out.push_str("\n还没有摘录。");
    } else {
        out.push_str(&format!("\n我的摘录（{} 条）：\n{}", ex.len(), ex.join("\n")));
    }
    Ok(out)
}

fn tool_search_notes(c: &Connection, args: &Value) -> Result<String> {
    let query = arg_str(args, "query").unwrap_or("");
    if query.is_empty() {
        return Ok("给一个 query。".into());
    }
    let limit = arg_usize(args, "limit", 8).clamp(1, 20);
    let pat = format!(
        "%{}%",
        query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let mut st = c.prepare(
        "SELECT id, title, substr(body, 1, 280) FROM notes \
         WHERE title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\' \
         ORDER BY updated_at DESC LIMIT ?2",
    )?;
    let rows: Vec<String> = st
        .query_map(params![pat, limit as i64], |r| {
            let id: String = r.get(0)?;
            let title: String = r.get(1)?;
            let body: String = r.get(2)?;
            Ok(format!(
                "- 《{}》 · note_id={}\n  {}\n",
                if title.trim().is_empty() { "无标题" } else { &title },
                id,
                one_line(&body, 200)
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(format!("笔记里没有和「{query}」有关的内容。"));
    }
    Ok(format!("你的笔记（{} 条）：\n{}", rows.len(), rows.concat()))
}

// ------------------------------------------------------------ openalex i/o
fn works_url(
    query: &str,
    author: Option<&str>,
    institution: Option<&str>,
    year_from: Option<i64>,
    sort: &str,
    limit: usize,
) -> String {
    let mut params_v: Vec<String> = vec![
        format!("per_page={}", limit.clamp(1, 25)),
        "mailto=cortex@local".into(),
    ];
    if !query.trim().is_empty() {
        params_v.push(format!("search={}", enc(query)));
    }
    let mut filters: Vec<String> = Vec::new();
    if let Some(a) = author.filter(|s| !s.trim().is_empty()) {
        filters.push(format!("raw_author_name.search:{}", enc(a)));
    }
    if let Some(i) = institution.filter(|s| !s.trim().is_empty()) {
        filters.push(format!("raw_affiliation_strings.search:{}", enc(i)));
    }
    if let Some(y) = year_from.filter(|y| *y > 1800) {
        filters.push(format!("from_publication_date:{y}-01-01"));
    }
    if !filters.is_empty() {
        params_v.push(format!("filter={}", filters.join(",")));
    }
    match sort {
        "citations" => params_v.push("sort=cited_by_count:desc".into()),
        "newest" => params_v.push("sort=publication_date:desc".into()),
        _ => {}
    }
    format!("{OPENALEX_WORKS}?{}", params_v.join("&"))
}

fn http_json(url: &str) -> Result<Value> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(25))
        .user_agent(UA)
        .build()
        .map_err(|e| Error::Other(format!("HTTP 客户端初始化失败：{e}")))?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| Error::Other(format!("连不上 OpenAlex：{e}")))?;
    let status = resp.status();
    let body = resp
        .text()
        .map_err(|e| Error::Other(format!("读取 OpenAlex 响应失败：{e}")))?;
    if !status.is_success() {
        let hint = if status.as_u16() == 429 {
            "（限流：等一分钟再问）"
        } else {
            ""
        };
        return Err(Error::Other(format!(
            "OpenAlex 返回 {}{hint}：{}",
            status.as_u16(),
            one_line(&body, 180)
        )));
    }
    serde_json::from_str(&body).map_err(|e| Error::Other(format!("OpenAlex 返回的不是 JSON：{e}")))
}

fn in_library(c: &Connection, doi: &str, title: &str) -> Result<bool> {
    let hit: Option<i64> = c
        .query_row(
            "SELECT 1 FROM papers WHERE (?1 <> '' AND lower(COALESCE(doi,'')) = lower(?1)) \
                OR lower(trim(title)) = lower(trim(?2)) LIMIT 1",
            params![doi, title],
            |r| r.get(0),
        )
        .optional()?;
    Ok(hit.is_some())
}

fn join_authors(w: &Value, max: usize) -> String {
    let names: Vec<&str> = w["authorships"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|a| a["author"]["display_name"].as_str())
                .take(max)
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        return "作者未知".into();
    }
    let total = w["authorships"].as_array().map(|v| v.len()).unwrap_or(0);
    let mut s = names.join(", ");
    if total > names.len() {
        s.push_str(" 等");
    }
    s
}

// ---------------------------------------------------------------- textual

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.trim().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let out: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&flat, max)
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub fn research_threads(state: State<AppState>) -> Result<Vec<Thread>> {
    let c = state.db.lock().unwrap();
    list_threads(&c, 50)
}

#[tauri::command]
pub fn research_thread_messages(
    state: State<AppState>,
    thread_id: String,
    limit: Option<usize>,
) -> Result<Vec<Message>> {
    let c = state.db.lock().unwrap();
    thread_messages(&c, &thread_id, limit.unwrap_or(300))
}

#[tauri::command]
pub fn research_thread_delete(state: State<AppState>, thread_id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_thread(&c, &thread_id)
}

/// The turn runs on its own SQLite connection (same as `agents_execute_run`):
/// a network-bound agent loop must not hold the UI's mutex for 30 seconds.
#[tauri::command]
pub async fn research_ask(
    app: tauri::AppHandle,
    thread_id: Option<String>,
    input: String,
) -> Result<AskOutcome> {
    tauri::async_runtime::spawn_blocking(move || -> Result<AskOutcome> {
        let state = app.state::<AppState>();
        let (mut model, db_path) = {
            let c = state.db.lock().unwrap();
            let keys = crate::commands::read_keys(&c)?;
            let mut m = crate::commands::load_model(&c, &keys, "chat")?;
            crate::commands::apply_budget(&mut m, &c, "chat");
            let path = state
                .data_dir
                .as_ref()
                .map(|d| d.join("cortex.db"))
                .ok_or_else(|| Error::Other("内存模式没有研究对话数据库".into()))?;
            (m, path)
        };
        let conn = Connection::open(&db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        ask(&conn, model.as_mut(), thread_id.as_deref(), &input)
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};

    struct Scripted {
        replies: Mutex<Vec<String>>,
        at: AtomicUsize,
    }

    impl Scripted {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().map(str::to_string).collect()),
                at: AtomicUsize::new(0),
            }
        }
    }

    impl Llm for Scripted {
        fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            let i = self.at.fetch_add(1, Ordering::SeqCst);
            let r = self.replies.lock().unwrap();
            Ok(r.get(i).cloned().unwrap_or_else(|| "ANSWER: 没有了".into()))
        }
        fn name(&self) -> String {
            "scripted".into()
        }
    }

    #[test]
    fn parse_action_reads_tool_then_answer() {
        match parse_action("TOOL my_library {\"query\":\"扩散\"}") {
            Action::Tool(name, args) => {
                assert_eq!(name, "my_library");
                assert_eq!(args["query"], "扩散");
            }
            _ => panic!("expected a tool call"),
        }
        match parse_action("ANSWER: 找到了两篇。") {
            Action::Answer(t) => assert_eq!(t, "找到了两篇。"),
            _ => panic!("expected an answer"),
        }
    }

    #[test]
    fn parse_action_tolerates_prose_and_fences() {
        // A fenced, pretty-printed call is still a call.
        let reply = "我先查一下：\n```json\nTOOL find_authors {\n  \"name\": \"Percy Liang\"\n}\n```";
        match parse_action(reply) {
            Action::Tool(name, args) => {
                assert_eq!(name, "find_authors");
                assert_eq!(args["name"], "Percy Liang");
            }
            _ => panic!("expected a tool call"),
        }
        // Plain prose is the answer, never an error.
        match parse_action("这个人最近在做什么？我不确定。") {
            Action::Answer(t) => assert!(t.contains("不确定")),
            _ => panic!("expected an answer"),
        }
    }

    #[test]
    fn an_answer_that_mentions_a_tool_is_still_an_answer() {
        let reply = "ANSWER: 我用了 TOOL search_literature 查过，没有结果。";
        match parse_action(reply) {
            Action::Answer(t) => assert!(t.starts_with("我用了")),
            _ => panic!("tool mention inside an answer must not win"),
        }
    }

    #[test]
    fn works_url_carries_author_institution_and_year() {
        let url = works_url("retrieval augmented", Some("Percy Liang"), Some("Stanford University"), Some(2023), "citations", 8);
        assert!(url.contains("search=retrieval%20augmented"));
        assert!(url.contains("raw_author_name.search:Percy%20Liang"));
        assert!(url.contains("raw_affiliation_strings.search:Stanford%20University"));
        assert!(url.contains("from_publication_date:2023-01-01"));
        assert!(url.contains("sort=cited_by_count:desc"));
        assert!(url.contains("per_page=8"));
    }

    #[test]
    fn title_is_flat_and_bounded() {
        assert_eq!(title_from("  谁  在做\nRAG  "), "谁 在做 RAG");
        assert!(title_from(&"长".repeat(60)).chars().count() <= 27);
        assert_eq!(title_from("   "), "新对话");
    }

    #[test]
    fn ask_runs_a_tool_then_answers_and_remembers_the_thread() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let mut llm = Scripted::new(vec![
            r#"TOOL my_library {"query":"扩散"}"#,
            "ANSWER: 你的库里还没有扩散模型相关的论文。",
        ]);
        let out = ask(&c, &mut llm, None, "我库里扩散模型相关的有哪些？").unwrap();
        assert_eq!(out.steps, 1);
        assert!(out.reply.contains("还没有"));
        assert_eq!(out.title, "我库里扩散模型相关的有哪些？");

        let msgs = thread_messages(&c, &out.thread_id, 50).unwrap();
        let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "tool", "agent"]);
        assert_eq!(msgs[1].tool.as_deref(), Some("my_library"));
        // The ask is stored next to the answer: judging a tool call means
        // seeing what was searched, not only what came back.
        assert!(msgs[1].args.as_deref().unwrap_or("").contains("扩散"));
        // The thread is listed and survives the turn.
        assert!(list_threads(&c, 10).unwrap().iter().any(|t| t.id == out.thread_id));
        assert_eq!(list_threads(&c, 10).unwrap()[0].turns, 1);
    }

    #[test]
    fn a_second_turn_reuses_the_same_thread() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let mut first = Scripted::new(vec!["ANSWER: 我在。"]);
        let out = ask(&c, &mut first, None, "你好").unwrap();
        let mut second = Scripted::new(vec!["ANSWER: 刚才我们打了个招呼。"]);
        let again = ask(&c, &mut second, Some(&out.thread_id), "我们刚才聊了什么？").unwrap();
        assert_eq!(again.thread_id, out.thread_id);
        let msgs = thread_messages(&c, &out.thread_id, 50).unwrap();
        assert_eq!(msgs.iter().filter(|m| m.role == "user").count(), 2);
        assert_eq!(msgs.iter().filter(|m| m.role == "agent").count(), 2);
    }

    #[test]
    fn deleting_a_thread_takes_its_messages_with_it() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let mut llm = Scripted::new(vec!["ANSWER: ok"]);
        let out = ask(&c, &mut llm, None, "test").unwrap();
        delete_thread(&c, &out.thread_id).unwrap();
        assert!(thread_messages(&c, &out.thread_id, 50).unwrap().is_empty());
        assert!(list_threads(&c, 10).unwrap().is_empty());
    }

    #[test]
    fn search_notes_quotes_the_users_own_writing() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let now = now_ms();
        c.execute(
            "INSERT INTO notes (id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params!["n1", "扩散模型笔记", "DDPM 的前向过程是加噪，反向是学去噪。", now],
        )
        .unwrap();
        let out = tool_search_notes(&c, &serde_json::json!({"query": "去噪"})).unwrap();
        assert!(out.contains("扩散模型笔记"), "{out}");
        assert!(out.contains("note_id=n1"), "{out}");
    }

    /// Real OpenAlex, real network. Off by default so the suite stays offline:
    ///   RESEARCH_E2E=1 cargo test --lib live_openalex -- --ignored
    #[test]
    #[ignore = "network call — set RESEARCH_E2E=1 to run"]
    fn live_openalex_answers_who_and_what() {
        if std::env::var("RESEARCH_E2E").ok().as_deref() != Some("1") {
            return;
        }
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let authors = tool_find_authors(&serde_json::json!({
            "name": "Percy Liang", "limit": 3
        }))
        .unwrap();
        assert!(authors.contains("Percy Liang"), "{authors}");
        assert!(authors.contains("Stanford") || authors.contains("Cornell"), "{authors}");

        let papers = tool_search_literature(
            &c,
            &serde_json::json!({
                "query": "retrieval augmented generation",
                "author": "Percy Liang",
                "year_from": 2022,
                "limit": 3
            }),
        )
        .unwrap();
        assert!(papers.contains("OpenAlex 命中"), "{papers}");
    }
}
