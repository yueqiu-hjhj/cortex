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
//! enabled built-in registry (all `read` kind — no write/shell tool exists to
//! whitelist), every step is appended to `agent_messages`, and the loop is
//! capped by the agent's `max_steps`.

use crate::agents::{append_message, finish_run, Agent};
use crate::error::Result;
use crate::llm::Llm;
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
    run: fn(&Connection, &serde_json::Value) -> Result<String>,
}

/// The five read-only tools seeded by migration 0030. Keep the names in sync
/// with that migration — the DB rows are the registry of record for the UI;
/// this table is the dispatch. `&'static` so the executor can hold borrows.
fn builtin_tools() -> &'static [BuiltinTool] {
    static TOOLS: std::sync::OnceLock<Vec<BuiltinTool>> = std::sync::OnceLock::new();
    TOOLS.get_or_init(|| {
        vec![
            BuiltinTool {
                name: "list_notebooks",
                usage: r#"list_notebooks {} — 列出所有科目及其主题."#,
                run: tool_list_notebooks,
            },
            BuiltinTool {
                name: "search_notebook",
                usage: r#"search_notebook {"query": "关键词", "subject_id": "可选", "limit": 8} — 全文检索知识库分块."#,
                run: tool_search_notebook,
            },
            BuiltinTool {
                name: "get_source",
                usage: r#"get_source {"source_id": "…"} — 读取一个来源的名称与全文（截断）."#,
                run: tool_get_source,
            },
            BuiltinTool {
                name: "search_notes",
                usage: r#"search_notes {"query": "关键词", "limit": 8} — 按标题/正文搜索用户的笔记."#,
                run: tool_search_notes,
            },
            BuiltinTool {
                name: "get_knowledge_tree",
                usage: r#"get_knowledge_tree {"subject_id": "…"} — 科目的主题树与各主题的来源数."#,
                run: tool_knowledge_tree,
            },
        ]
    })
}

// ------------------------------------------------------------ tool bodies

fn tool_list_notebooks(c: &Connection, _args: &serde_json::Value) -> Result<String> {
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

fn tool_search_notebook(c: &Connection, args: &serde_json::Value) -> Result<String> {
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

fn tool_get_source(c: &Connection, args: &serde_json::Value) -> Result<String> {
    let id = args.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
    if id.trim().is_empty() {
        return Ok("缺少 source_id 参数".into());
    }
    let s = repo::get_source(c, id)?;
    let text = s.content.as_deref().unwrap_or("(无提取文本)");
    Ok(format!("「{}」（kind={}, status={}）\n\n{}", s.name, s.kind, s.status, snippet(text, 3000)))
}

fn tool_search_notes(c: &Connection, args: &serde_json::Value) -> Result<String> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.trim().is_empty() {
        return Ok("缺少 query 参数".into());
    }
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(TOOL_HITS as i64) as usize;
    let pat = format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = c.prepare(
        "SELECT title, substr(body, 1, 240) FROM notes \
         WHERE title LIKE ?1 ESCAPE '\\' OR body LIKE ?1 ESCAPE '\\' \
         ORDER BY updated_at DESC LIMIT ?2",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![pat, limit.max(1) as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok("没有命中的笔记。".into());
    }
    Ok(rows.iter().map(|(t, b)| format!("- {t}：{b}")).collect::<Vec<_>>().join("\n"))
}

fn tool_knowledge_tree(c: &Connection, args: &serde_json::Value) -> Result<String> {
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
/// enabled built-in registry. An agent with no tools (or all-disabled tools)
/// still works — it just answers in one completion.
fn resolve_tools(c: &Connection, user_id: &str, agent: &Agent) -> Result<Vec<&'static BuiltinTool>> {
    let enabled: std::collections::HashSet<String> = crate::agents::list_tools(c, user_id)?
        .into_iter()
        .filter(|t| t.enabled && t.kind == "read")
        .map(|t| t.name)
        .collect();
    Ok(builtin_tools()
        .iter()
        .filter(|t| agent.tools.iter().any(|n| n == t.name) && enabled.contains(t.name))
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
    if !skills.is_empty() {
        s.push_str("\n## 可用技能（指令遵循）\n");
        s.push_str(skills);
    }
    s
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
            let output = if let Some(t) = impl_tool {
                match (t.run)(c, &args) {
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
        replies: std::sync::Mutex<std::collections::VecDeque<&'static str>>,
        prompts: std::sync::Mutex<Vec<String>>,
    }
    impl Llm for Scripted {
        fn complete(&self, _system: &str, user: &str) -> Result<String> {
            self.prompts.lock().unwrap().push(user.to_string());
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .map(str::to_string)
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
        )
        .unwrap()
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
        )
        .unwrap()
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
            None, 10, &crate::vector::f32s_to_blob(&[0.0; 4]),
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
                .collect(),
            ),
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = execute_run(&c, crate::agents::DEFAULT_USER, &agent, &run.id, &scripted).unwrap();
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
                .collect(),
            ),
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = execute_run(&c, crate::agents::DEFAULT_USER, &agent, &run.id, &scripted).unwrap();
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
                vec![r#"TOOL search_notebook {"query": "x"}"#].into_iter().collect(),
            ),
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = execute_run(&c, crate::agents::DEFAULT_USER, &agent, &run.id, &scripted).unwrap();
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
}
