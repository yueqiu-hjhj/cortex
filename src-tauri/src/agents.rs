//! Agent platform: custom agents, runs, and the skill/tool registry.
//!
//! Each agent is owned by exactly one user. Runs and messages are append-only
//! audit logs. The platform's safety story is "every tool is explicit and every
//! run is observable": a `read` tool can't mutate state, a `shell` tool requires
//! a per-run allow-list, and the full transcript is queryable from the UI.
//!
//! The skill registry holds the instructions the agent can pull in mid-run via
//! the `select_skill` pseudo-tool. Users define their own skills; built-in
//! skills (e.g. "answer with citations only") are seeded in 0034.

use crate::error::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const DEFAULT_USER: &str = "u-default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: String,
    pub skills: Vec<String>,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub temperature: f64,
    pub max_steps: i64,
    pub enabled: bool,
    /// Highest tool tier this agent may use: read < write < network (cumulative).
    pub tool_tier: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub agent_id: String,
    pub user_id: String,
    pub status: String,
    pub input: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub steps: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// Shared-conversation id (0047). Empty = legacy ungrouped run.
    #[serde(default)]
    pub thread_id: String,
    /// Parent run when this one was spawned by `delegate` (0089). Empty for
    /// top-level runs.
    #[serde(default)]
    pub parent_run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub run_id: String,
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub created_at: i64,
    /// Set for thread queries (0047): which agent produced this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: String,
    pub body: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// local | bundled | imported (0090) — where this skill came from.
    #[serde(default = "default_skill_source")]
    pub source: String,
    /// Revision carried by an import; 1 for a fresh local skill.
    #[serde(default = "default_skill_version")]
    pub version: i64,
}

fn default_skill_source() -> String {
    "local".to_string()
}

fn default_skill_version() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub schema_json: String,
    pub enabled: bool,
    pub built_in: bool,
    pub created_at: i64,
}

pub fn list_agents(conn: &Connection, user_id: &str) -> Result<Vec<Agent>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_id, name, description, system_prompt, skills, tools, model,
                temperature, max_steps, enabled, tool_tier, created_at, updated_at
           FROM agents WHERE user_id=?1 ORDER BY updated_at DESC"
    )?;
    let rows = stmt.query_map([user_id], parse_agent)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn get_agent(conn: &Connection, user_id: &str, id: &str) -> Result<Agent> {
    conn.query_row(
        "SELECT id, user_id, name, description, system_prompt, skills, tools, model,
                temperature, max_steps, enabled, tool_tier, created_at, updated_at
           FROM agents WHERE user_id=?1 AND id=?2",
        [user_id, id],
        parse_agent,
    ).map_err(|e| Error::NotFound(format!("agent {id} ({e})")))
}

pub const TOOL_TIERS: &[&str] = &["read", "write", "network", "files", "shell"];

/// Normalize a stored/requested tier; garbage falls back to the safe default.
pub fn normalize_tier(s: &str) -> &'static str {
    match s.trim() {
        "write" => "write",
        "network" => "network",
        // File access is the highest rung on purpose: it is the only tier that
        // can touch anything outside the database, so it must be granted
        // explicitly — a network agent does NOT get it by default.
        "files" => "files",
        // Shell is above files: running a command can do anything the file
        // tools can, plus execute arbitrary code. It is opt-in twice (tier +
        // a command allowlist in settings).
        "shell" => "shell",
        _ => "read",
    }
}

pub fn tier_rank(t: &str) -> u8 {
    match normalize_tier(t) {
        "write" => 1,
        "network" => 2,
        "files" => 3,
        "shell" => 4,
        _ => 0,
    }
}

pub fn upsert_agent(
    conn: &Connection,
    user_id: &str,
    id: Option<&str>,
    name: &str,
    description: Option<&str>,
    system_prompt: &str,
    skills: &[String],
    tools: &[String],
    model: Option<&str>,
    temperature: f64,
    max_steps: i64,
    enabled: bool,
    tool_tier: &str,
) -> Result<Agent> {
    if name.trim().is_empty() { return Err(Error::Other("agent name is required".into())); }
    if !(0.0..=2.0).contains(&temperature) { return Err(Error::Other("temperature must be 0..=2".into())); }
    if !(1..=64).contains(&max_steps) { return Err(Error::Other("max_steps must be 1..=64".into())); }
    if !TOOL_TIERS.contains(&tool_tier) {
        return Err(Error::Other(format!("tool_tier 必须是 {}", TOOL_TIERS.join("/"))));
    }
    let skills = serde_json::to_string(skills)?;
    let tools = serde_json::to_string(tools)?;
    let now = crate::db::now_ms();
    let id = match id {
        Some(existing) => {
            let updated = conn.execute(
                "UPDATE agents SET name=?1, description=?2, system_prompt=?3, skills=?4,
                  tools=?5, model=?6, temperature=?7, max_steps=?8, enabled=?9, tool_tier=?10, updated_at=?11
                  WHERE user_id=?12 AND id=?13",
                params![name, description, system_prompt, skills, tools, model, temperature, max_steps, enabled as i64, tool_tier, now, user_id, existing],
            )?;
            if updated == 0 { return Err(Error::NotFound(format!("agent {existing}"))); }
            existing.to_string()
        }
        None => {
            let id = crate::db::new_id();
            conn.execute(
                "INSERT INTO agents (id, user_id, name, description, system_prompt, skills, tools,
                    model, temperature, max_steps, enabled, tool_tier, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",
                params![id, user_id, name, description, system_prompt, skills, tools, model, temperature, max_steps, enabled as i64, tool_tier, now],
            )?;
            id
        }
    };
    get_agent(conn, user_id, &id)
}

pub fn delete_agent(conn: &Connection, user_id: &str, id: &str) -> Result<()> {
    let n = conn.execute("DELETE FROM agents WHERE user_id=?1 AND id=?2", [user_id, id])?;
    if n == 0 { return Err(Error::NotFound(format!("agent {id}"))); }
    Ok(())
}

pub fn start_run(conn: &Connection, user_id: &str, agent_id: &str, input: &str) -> Result<AgentRun> {
    start_run_in_thread(conn, user_id, agent_id, input, None)
}

/// Start a run; `thread_id` joins an existing shared conversation (any agent's
/// messages in it become this run's context). None = a fresh ungrouped run.
pub fn start_run_in_thread(
    conn: &Connection,
    user_id: &str,
    agent_id: &str,
    input: &str,
    thread_id: Option<&str>,
) -> Result<AgentRun> {
    if input.trim().is_empty() { return Err(Error::Other("run input is empty".into())); }
    let agent = get_agent(conn, user_id, agent_id)?;
    if !agent.enabled { return Err(Error::Other("agent is disabled".into())); }
    // Thread must belong to this user (checked via its first run).
    let thread = match thread_id.filter(|t| !t.trim().is_empty()) {
        Some(t) => {
            let owner: Option<String> = conn
                .query_row(
                    "SELECT user_id FROM agent_runs WHERE thread_id=?1 LIMIT 1",
                    [t],
                    |r| r.get(0),
                )
                .ok();
            match owner {
                Some(u) if u == user_id => t.to_string(),
                Some(_) => return Err(Error::NotFound(format!("thread {t}"))),
                None => t.to_string(), // fresh thread id — first run defines it
            }
        }
        None => String::new(),
    };
    let id = crate::db::new_id();
    let now = crate::db::now_ms();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO agent_runs (id, agent_id, user_id, status, input, started_at, thread_id)
         VALUES (?1,?2,?3,'pending',?4,?5,?6)",
        params![id, agent.id, user_id, input, now, thread],
    )?;
    tx.execute(
        "INSERT INTO agent_messages (id, run_id, role, content, created_at)
         VALUES (?1,?2,'user',?3,?4)",
        params![crate::db::new_id(), id, input, now],
    )?;
    tx.commit()?;
    Ok(AgentRun { id, agent_id: agent.id, user_id: user_id.into(), status: "pending".into(),
        input: input.into(), result: None, error: None, steps: 0, started_at: now, finished_at: None,
        thread_id: thread, parent_run_id: String::new() })
}

pub fn append_message(conn: &Connection, run_id: &str, role: &str, content: &str, tool_name: Option<&str>) -> Result<()> {
    if !matches!(role, "system" | "user" | "assistant" | "tool") {
        return Err(Error::Other(format!("invalid role {role}")));
    }
    conn.execute(
        "INSERT INTO agent_messages (id, run_id, role, content, tool_name, created_at)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![crate::db::new_id(), run_id, role, content, tool_name, crate::db::now_ms()],
    )?;
    conn.execute("UPDATE agent_runs SET steps = steps + 1 WHERE id=?1", [run_id])?;
    Ok(())
}

pub fn finish_run(conn: &Connection, run_id: &str, status: &str, result: Option<&str>, error: Option<&str>) -> Result<()> {
    if !matches!(status, "success" | "error" | "cancelled") {
        return Err(Error::Other(format!("invalid status {status}")));
    }
    conn.execute(
        "UPDATE agent_runs SET status=?1, result=?2, error=?3, finished_at=?4 WHERE id=?5",
        params![status, result, error, crate::db::now_ms(), run_id],
    )?;
    Ok(())
}

/// Start a run spawned by another run's `delegate` tool: a real run with its
/// own thread and transcript, plus the parent link so the UI can nest it.
pub fn start_child_run(
    conn: &Connection,
    user_id: &str,
    agent_id: &str,
    input: &str,
    parent_run_id: &str,
) -> Result<AgentRun> {
    let mut run = start_run_in_thread(conn, user_id, agent_id, input, None)?;
    conn.execute(
        "UPDATE agent_runs SET parent_run_id=?2 WHERE id=?1",
        params![run.id, parent_run_id],
    )?;
    run.parent_run_id = parent_run_id.to_string();
    Ok(run)
}

/// One run by id, scoped to its owner. Used by the executor to return its own
/// finished run — the old "newest run for this user" heuristic breaks as soon
/// as a delegated child is newer than its parent.
pub fn get_run(conn: &Connection, user_id: &str, id: &str) -> Result<AgentRun> {
    conn.query_row(
        "SELECT id, agent_id, user_id, status, input, result, error, steps, started_at, \
                finished_at, thread_id, parent_run_id \
           FROM agent_runs WHERE id=?1 AND user_id=?2",
        params![id, user_id],
        |r| {
            Ok(AgentRun {
                id: r.get(0)?,
                agent_id: r.get(1)?,
                user_id: r.get(2)?,
                status: r.get(3)?,
                input: r.get(4)?,
                result: r.get(5)?,
                error: r.get(6)?,
                steps: r.get(7)?,
                started_at: r.get(8)?,
                finished_at: r.get(9)?,
                thread_id: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                parent_run_id: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
            })
        },
    )
    .map_err(|_| Error::NotFound(format!("run {id}")))
}

pub fn list_runs(conn: &Connection, user_id: &str, limit: i64) -> Result<Vec<AgentRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, user_id, status, input, result, error, steps, started_at, finished_at, thread_id, parent_run_id
           FROM agent_runs WHERE user_id=?1 ORDER BY started_at DESC LIMIT ?2"
    )?;
    let rows = stmt.query_map(params![user_id, limit], |r| {
        Ok(AgentRun {
            id: r.get(0)?, agent_id: r.get(1)?, user_id: r.get(2)?,
            status: r.get(3)?, input: r.get(4)?, result: r.get(5)?, error: r.get(6)?,
            steps: r.get(7)?, started_at: r.get(8)?, finished_at: r.get(9)?,
            thread_id: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
            parent_run_id: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
        })
    })?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn list_messages(conn: &Connection, user_id: &str, run_id: &str, limit: i64) -> Result<Vec<AgentMessage>> {
    let run_owner: Option<String> = conn.query_row(
        "SELECT user_id FROM agent_runs WHERE id=?1", [run_id], |r| r.get(0),
    ).ok();
    if run_owner.as_deref() != Some(user_id) {
        return Err(Error::NotFound(format!("run {run_id}")));
    }
    let mut stmt = conn.prepare(
        "SELECT id, run_id, role, content, tool_name, created_at FROM agent_messages
           WHERE run_id=?1 ORDER BY created_at ASC LIMIT ?2"
    )?;
    let rows = stmt.query_map(params![run_id, limit], |r| {
        Ok(AgentMessage {
            id: r.get(0)?, run_id: r.get(1)?, role: r.get(2)?,
            content: r.get(3)?, tool_name: r.get(4)?, created_at: r.get(5)?,
            agent_id: None, agent_name: None,
        })
    })?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Every message in a shared conversation (all runs of the thread), oldest
/// first, with agent attribution — the multi-agent shared-context transcript.
pub fn thread_messages(
    conn: &Connection,
    user_id: &str,
    thread_id: &str,
    limit: i64,
) -> Result<Vec<AgentMessage>> {
    if thread_id.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Ownership via any run of the thread.
    let owner: Option<String> = conn
        .query_row(
            "SELECT user_id FROM agent_runs WHERE thread_id=?1 LIMIT 1",
            [thread_id],
            |r| r.get(0),
        )
        .ok();
    if owner.as_deref() != Some(user_id) {
        return Err(Error::NotFound(format!("thread {thread_id}")));
    }
    let mut stmt = conn.prepare(
        "SELECT m.id, m.run_id, m.role, m.content, m.tool_name, m.created_at,
                r.agent_id, a.name
           FROM agent_messages m
           JOIN agent_runs r ON r.id = m.run_id
           LEFT JOIN agents a ON a.id = r.agent_id
          WHERE r.thread_id = ?1
          ORDER BY m.created_at ASC
          LIMIT ?2"
    )?;
    let rows = stmt
        .query_map(params![thread_id, limit], |r| {
            Ok(AgentMessage {
                id: r.get(0)?,
                run_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                tool_name: r.get(4)?,
                created_at: r.get(5)?,
                agent_id: r.get(6)?,
                agent_name: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The context a run's loop actually reads: the whole thread when the run
/// belongs to one (switch-agent keeps context), otherwise just its own
/// messages (legacy ungrouped runs).
pub fn history_for_run(conn: &Connection, user_id: &str, run_id: &str, limit: i64) -> Result<Vec<AgentMessage>> {
    let thread: Option<String> = conn
        .query_row(
            "SELECT r.thread_id FROM agent_runs r WHERE r.id=?1 AND r.user_id=?2",
            params![run_id, user_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    match thread.filter(|t| !t.is_empty()) {
        Some(t) => thread_messages(conn, user_id, &t, limit),
        None => list_messages(conn, user_id, run_id, limit),
    }
}

pub fn list_skills(conn: &Connection, user_id: &str) -> Result<Vec<Skill>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_id, name, description, body, enabled, created_at, updated_at, source, version
           FROM agent_skills WHERE user_id=?1 ORDER BY name"
    )?;
    let rows = stmt.query_map([user_id], |r| Ok(Skill {
        id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, description: r.get(3)?,
        body: r.get(4)?, enabled: r.get(5)?, created_at: r.get(6)?, updated_at: r.get(7)?,
        source: r.get(8)?, version: r.get(9)?,
    }))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn upsert_skill(
    conn: &Connection,
    user_id: &str,
    id: Option<&str>,
    name: &str,
    description: &str,
    body: &str,
    enabled: bool,
) -> Result<Skill> {
    if name.trim().is_empty() || body.trim().is_empty() {
        return Err(Error::Other("skill name and body are required".into()));
    }
    let now = crate::db::now_ms();
    let id = match id {
        Some(existing) => {
            let n = conn.execute(
                "UPDATE agent_skills SET name=?1, description=?2, body=?3, enabled=?4, updated_at=?5
                  WHERE user_id=?6 AND id=?7",
                params![name, description, body, enabled as i64, now, user_id, existing],
            )?;
            if n == 0 { return Err(Error::NotFound(format!("skill {existing}"))); }
            existing.to_string()
        }
        None => {
            let id = crate::db::new_id();
            conn.execute(
                "INSERT INTO agent_skills (id, user_id, name, description, body, enabled, created_at, updated_at, source, version)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?7,'local',1)",
                params![id, user_id, name, description, body, enabled as i64, now],
            )?;
            id
        }
    };
    conn.query_row(
        "SELECT id, user_id, name, description, body, enabled, created_at, updated_at, source, version
           FROM agent_skills WHERE id=?1", [id], |r| Ok(Skill {
            id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, description: r.get(3)?,
            body: r.get(4)?, enabled: r.get(5)?, created_at: r.get(6)?, updated_at: r.get(7)?,
            source: r.get(8)?, version: r.get(9)?,
        }),
    ).map_err(Into::into)
}

/// C3 (3.9): bundled personal skills — the workflows this user actually runs
/// on repeat, seeded as ready-to-enable templates. Seeded ONCE by (user, name):
/// the user's copy is theirs to edit/disable/delete, and upgrades never
/// overwrite it. `enabled = 0` on insert — opting in is one toggle in the
/// Agents view, and an agent that references skills by name stays unaffected.
const BUNDLED_SKILLS: &[(&str, &str, &str)] = &[
    (
        "文献综述骨架",
        "给定主题生成可执行的综述写作骨架：主题地图 → 观点对比 → 缺口",
        "你是科研写作助手。用户给出一个综述主题后，按以下结构输出：\n\
         1. **检索策略**：列出 3 组中英文检索词（同义词/上下位词），标注适合的数据库。\n\
         2. **主题地图**：把该领域分成 3-5 个子主题，每个子主题一句话说明它回答什么问题、与相邻子主题的边界。\n\
         3. **观点对比**：每个子主题下，列出代表性立场/流派（占位：[作者 年份]），用表格对比其方法、样本、结论强度。\n\
         4. **矛盾与缺口**：明确指出哪两个结论相互冲突、哪些问题尚无人做。\n\
         5. **骨架**：给出综述的节标题层级（H2/H3），每节 2-3 句要点，供直接扩写。\n\
         引用一律用 [作者 年份] 占位，禁止编造具体文献细节；不确定就写「待核实」。",
    ),
    (
        "实验复现清单",
        "从论文方法部分生成逐步复现清单：环境/数据/超参/验证/坑",
        "你是复现实验的工程师。用户给出一篇论文的方法描述（或摘要）后，输出：\n\
         1. **环境**：语言/框架/硬件需求推测，给 pip/conda 安装命令占位。\n\
         2. **数据**：训练/评测数据集、划分方式、预处理步骤（逐条可勾选）。\n\
         3. **超参表**：论文出现的全部超参，表格列出名称、值、出处章节；缺失的标「论文未给，需实验」。\n\
         4. **实现步骤**：按依赖顺序编号（先数据管线→模型→训练循环→评测），每步一句验收标准。\n\
         5. **验证**：复现成功的判定指标与论文报告值的对照表。\n\
         6. **常见坑**：根据方法类型列出 3-5 个易错点（随机种子、数据泄漏、数值稳定性等）。\n\
         论文没写的细节必须标注「推测」，不许编造成论文原文。",
    ),
    (
        "章节草稿起草",
        "按章节号+要点+引用生成学术语气草稿，引用留占位",
        "你是学术写作助手。用户给出「章节号 + 本章要点 + 可用参考文献列表」后：\n\
         1. 先用 3-5 行列出本章的逻辑推进顺序（段落规划），等用户确认或直接续写。\n\
         2. 正文：学术语气、每段一个论点、论点句在段首；句间用「因此/然而/具体而言」显式衔接。\n\
         3. 引用：只允许使用用户给的参考文献，标注为 [n]（编号对应其列表序号）；需要支撑但没有对应文献的句子，在句尾加 ⚠ 标记。\n\
         4. 术语：首次出现给中英对照；保持全章一致。\n\
         5. 结尾给「本节小结」3 句以内 + 下一章衔接句。\n\
         不堆砌形容词，不写「众所周知」，每段 4-7 句。",
    ),
    (
        "公式推导逐步走",
        "从假设出发逐步推导，每步标注依据，末尾给直观解释",
        "你是数学推导助手。用户给出一个定理/公式/题目后：\n\
         1. **已知与目标**：先列出全部假设、记号约定、要证的目标，单独成行。\n\
         2. **推导**：每步只做一件事（代入/移项/用某定理/恒等变形），公式用 LaTeX（$$ 块），行内短式用 $...$；每步右侧或下方用「依据：」标注所用定义/定理/前一步编号。\n\
         3. **关键跳跃**：凡是非显然的一步，先写一句「为什么可以这样做」再写式子。\n\
         4. **结论**：把证得的结论整理成 boxed 形式，与目标逐条对照。\n\
         5. **直观解释**：用一段平话解释这个结果的含义和一个具体例子（带数字）。\n\
         推导卡住时明确说「此处需要 XX 引理，正文未给」，不许跳步伪装完成。",
    ),
];

pub fn seed_bundled_skills(conn: &Connection) -> Result<usize> {
    let mut seeded = 0;
    for (name, desc, body) in BUNDLED_SKILLS {
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM agent_skills WHERE user_id=?1 AND name=?2",
            params![DEFAULT_USER, name],
            |r| r.get(0),
        )?;
        if exists == 0 {
            let now = crate::db::now_ms();
            conn.execute(
                "INSERT INTO agent_skills (id, user_id, name, description, body, enabled, created_at, updated_at, source, version)
                 VALUES (?1,?2,?3,?4,?5,0,?6,?6,'bundled',1)",
                params![crate::db::new_id(), DEFAULT_USER, name, desc, body, now],
            )?;
            seeded += 1;
        }
    }
    Ok(seeded)
}

/// File tools (the Claude-Code-shaped ones) are seeded at runtime rather than
/// in a migration: the registry is UI data, and INSERT OR IGNORE keeps this
/// idempotent on every launch. They are gated by the `files` tier, which no
/// existing agent has, so seeding alone grants nobody access.
pub fn seed_file_tools(conn: &Connection) -> Result<usize> {
    const TOOLS: &[(&str, &str, &str, &str)] = &[
        ("tool-read-file", "read_file", "Read a text file inside the agent workspace, with line numbers.", "files"),
        ("tool-write-file", "write_file", "Create or overwrite a text file inside the workspace.", "files"),
        ("tool-edit-file", "edit_file", "Replace one exact, unique string in a file (preferred for code edits).", "files"),
        ("tool-list-dir", "list_dir", "List files and folders inside the workspace.", "files"),
        ("tool-search-files", "search_files", "Recursively search file contents inside the workspace.", "files"),
        ("tool-delegate", "delegate", "Delegate a focused task to another agent. The child runs with its own tools and cannot exceed this agent's permission tier.", "read"),
        ("tool-run-command", "run_command", "Run one allowlisted command inside the workspace (shell tier; no shell metacharacters).", "shell"),
    ];
    let now = crate::db::now_ms();
    let mut seeded = 0;
    for (id, name, desc, kind) in TOOLS {
        seeded += conn.execute(
            "INSERT OR IGNORE INTO agent_tools \
             (id, user_id, name, description, kind, enabled, schema_json, built_in, created_at) \
             VALUES (?1,?2,?3,?4,?5,1,'{}',1,?6)",
            params![id, DEFAULT_USER, name, desc, kind, now],
        )?;
    }
    Ok(seeded)
}

// ---- 技能可移植：导出/导入（0090） -----------------------------------------
//
// A skill is the reusable part of an agent, so it should be a file you can
// carry between machines — not a row trapped in one database. The file carries
// a schema tag, a version and where it came from; import validates all three
// before writing anything.

pub const SKILL_SCHEMA: &str = "cortex-skill/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFile {
    pub schema: String,
    #[serde(default = "default_skill_version")]
    pub version: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub body: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub exported_at: Option<i64>,
}

fn skill_by_id(conn: &Connection, user_id: &str, id: &str) -> Result<Skill> {
    conn.query_row(
        "SELECT id, user_id, name, description, body, enabled, created_at, updated_at, source, version
           FROM agent_skills WHERE id=?1 AND user_id=?2",
        params![id, user_id],
        |r| Ok(Skill {
            id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, description: r.get(3)?,
            body: r.get(4)?, enabled: r.get(5)?, created_at: r.get(6)?, updated_at: r.get(7)?,
            source: r.get(8)?, version: r.get(9)?,
        }),
    )
    .map_err(|_| Error::NotFound(format!("skill {id}")))
}

pub fn skill_to_json(skill: &Skill) -> Result<String> {
    let file = SkillFile {
        schema: SKILL_SCHEMA.to_string(),
        version: skill.version.max(1),
        name: skill.name.clone(),
        description: skill.description.clone(),
        body: skill.body.clone(),
        source: skill.source.clone(),
        exported_at: Some(crate::db::now_ms()),
    };
    serde_json::to_string_pretty(&file)
        .map_err(|e| Error::Other(format!("技能序列化失败：{e}")))
}

/// Validate a file before it can touch the database. Pure, so the tests drive
/// it directly: wrong schema, empty name/body, absurd length.
pub fn parse_skill_file(raw: &str) -> Result<SkillFile> {
    let f: SkillFile = serde_json::from_str(raw)
        .map_err(|e| Error::Other(format!("技能文件不是合法 JSON：{e}")))?;
    if f.schema != SKILL_SCHEMA {
        return Err(Error::Other(format!(
            "技能文件格式不匹配（期望 schema={SKILL_SCHEMA}，实际 {}）",
            if f.schema.is_empty() { "(空)" } else { &f.schema }
        )));
    }
    let name = f.name.trim().to_string();
    let body = f.body.trim().to_string();
    if name.is_empty() {
        return Err(Error::Other("技能文件缺少 name".into()));
    }
    if body.is_empty() {
        return Err(Error::Other("技能文件缺少 body（技能正文）".into()));
    }
    if name.chars().count() > 80 {
        return Err(Error::Other("技能名太长（上限 80 字）".into()));
    }
    if body.chars().count() > 40_000 {
        return Err(Error::Other("技能正文太长（上限 40000 字）".into()));
    }
    Ok(SkillFile {
        schema: f.schema,
        version: f.version.max(1),
        name,
        description: f.description.trim().to_string(),
        body,
        source: f.source,
        exported_at: f.exported_at,
    })
}

pub fn export_skill(
    conn: &Connection,
    user_id: &str,
    skill_id: &str,
    dest: &std::path::Path,
) -> Result<String> {
    let skill = skill_by_id(conn, user_id, skill_id)?;
    let json = skill_to_json(&skill)?;
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(dest, json).map_err(|e| Error::Other(format!("写入失败：{e}")))?;
    Ok(dest.display().to_string())
}

/// Import a skill file. Same name is a conflict unless `overwrite` is set —
/// silently replacing a skill the user tuned is exactly the kind of surprise
/// this feature must not have.
pub fn import_skill(
    conn: &Connection,
    user_id: &str,
    path: &std::path::Path,
    overwrite: bool,
) -> Result<Skill> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::Other(format!("读取技能文件失败：{e}")))?;
    let f = parse_skill_file(&raw)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM agent_skills WHERE user_id=?1 AND name=?2",
            params![user_id, f.name],
            |r| r.get(0),
        )
        .optional()?;
    let now = crate::db::now_ms();
    let id = match existing {
        Some(id) => {
            if !overwrite {
                return Err(Error::Other(format!(
                    "同名技能「{}」已存在；覆盖导入需要显式确认",
                    f.name
                )));
            }
            conn.execute(
                "UPDATE agent_skills SET description=?2, body=?3, source='imported', \
                 version=?4, enabled=1, updated_at=?5 WHERE id=?1",
                params![id, f.description, f.body, f.version, now],
            )?;
            id
        }
        None => {
            let id = crate::db::new_id();
            conn.execute(
                "INSERT INTO agent_skills \
                 (id, user_id, name, description, body, enabled, created_at, updated_at, source, version) \
                 VALUES (?1,?2,?3,?4,?5,1,?6,?6,'imported',?7)",
                params![id, user_id, f.name, f.description, f.body, now, f.version],
            )?;
            id
        }
    };
    skill_by_id(conn, user_id, &id)
}

#[tauri::command]
pub fn skill_export(
    state: tauri::State<'_, crate::AppState>,
    skill_id: String,
    dest: String,
) -> Result<String> {
    let c = state.db.lock().unwrap();
    export_skill(&c, DEFAULT_USER, &skill_id, std::path::Path::new(&dest))
}

#[tauri::command]
pub fn skill_import(
    state: tauri::State<'_, crate::AppState>,
    path: String,
    overwrite: bool,
) -> Result<Skill> {
    let c = state.db.lock().unwrap();
    import_skill(&c, DEFAULT_USER, std::path::Path::new(&path), overwrite)
}

// ---- 预置角色库：角色 = 技能 + 提示词 + 工具集 + 权限等级 ----------------------
#[derive(Debug, Clone, serde::Serialize)]
pub struct PresetRole {
    pub id: String,
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    /// Bundled skill names referenced by the role (enabled on instantiate).
    pub skills: Vec<String>,
    pub tools: Vec<String>,
    pub tool_tier: String,
}

/// Ready-to-instantiate roles. Each pairs one bundled skill with a fitting
/// prompt and a minimal toolset — a role is a starting point, not a cage:
/// the instantiated agent is a normal editable row.
pub fn preset_roles() -> Vec<PresetRole> {
    let read_tools = |names: &[&str]| names.iter().map(|s| s.to_string()).collect();
    vec![
        PresetRole {
            id: "code-assistant".into(),
            name: "代码助手".into(),
            description: "读写工作目录：列目录、搜代码、读文件、精确改文件。files 权限（最高档）；工作目录在页面顶部设置。".into(),
            system_prompt: "你是代码助手，工作方式像 Claude Code：\n\
1. 先用 list_dir / search_files 找到相关文件，不要猜路径；\n\
2. 用 read_file 读够上下文再动手；\n\
3. 改代码用 edit_file（old_string 必须逐字一致且唯一，含缩进）；只有整篇重写才用 write_file；\n\
4. 改完用 read_file 复核改后的片段；\n\
5. 不能执行 shell 命令，也不要声称运行过测试；需要用户执行时明确写出命令。\n\
回答用中文，改动要说清「哪个文件、改了什么」。".into(),
            skills: vec![],
            tools: read_tools(&[
                "list_dir",
                "search_files",
                "read_file",
                "edit_file",
                "write_file",
                "run_command",
                "search_notes",
            ]),
            tool_tier: "shell".into(),
        },
        PresetRole {
            id: "review-scholar".into(),
            name: "综述学者".into(),
            description: "文献综述写作：检索策略 → 主题地图 → 观点对比 → 缺口骨架。绑「文献综述骨架」技能。".into(),
            system_prompt: "你是科研综述助手。先盘点用户库里已有的相关来源（用检索工具），再按「文献综述骨架」技能的结构产出综述骨架；库里没有的文献用 [作者 年份] 占位并标注待核实。回答用中文。".into(),
            skills: vec!["文献综述骨架".into()],
            tools: read_tools(&["list_notebooks", "search_notebook", "get_source", "search_notes", "get_knowledge_tree"]),
            tool_tier: "read".into(),
        },
        PresetRole {
            id: "repro-engineer".into(),
            name: "复现工程师".into(),
            description: "从论文方法部分生成逐步复现清单：环境/数据/超参/验证/坑。绑「实验复现清单」技能。".into(),
            system_prompt: "你是实验复现工程师。先用 get_source 读取用户指定的论文来源，再按「实验复现清单」技能输出复现清单；论文未给的细节一律标「推测」。回答用中文。".into(),
            skills: vec!["实验复现清单".into()],
            tools: read_tools(&["list_notebooks", "search_notebook", "get_source", "get_knowledge_tree"]),
            tool_tier: "read".into(),
        },
        PresetRole {
            id: "chapter-writer".into(),
            name: "章节写手".into(),
            description: "学术语气章节草稿：段落规划 → 逐段起草 → 小结衔接；write 权限，成稿以提案交给用户确认后入库。绑「章节草稿起草」技能。".into(),
            system_prompt: "你是学术写作助手。按「章节草稿起草」技能起草章节；引用只允许用户提供的文献或用检索工具在库里查到的内容，查不到支撑的句子标 ⚠。用户要求保存草稿时，用 propose_note 把成稿作为草稿提交（mode=new 时给 title 与 subject 学科名，append/replace 时给 note_id；不确定科目归属就先问，不要猜）。propose_note 只是排队待审，笔记与 .md 文件都没有写入 —— 用户在对话里点「采纳并写入」之后才算保存，所以回复时必须说「草稿已提交待确认」，绝不能说「已保存」。回答用中文。".into(),
            skills: vec!["章节草稿起草".into()],
            tools: read_tools(&["list_notebooks", "search_notebook", "search_notes", "get_source", "get_note", "propose_note"]),
            tool_tier: "write".into(),
        },
        PresetRole {
            id: "math-tutor".into(),
            name: "推导陪练".into(),
            description: "数学逐步推导：每步标依据，卡住明说。绑「公式推导逐步走」技能。".into(),
            system_prompt: "你是数学推导陪练。按「公式推导逐步走」技能逐步推导；用户库里的讲义可以用检索工具核对记号与定理编号。回答用中文。".into(),
            skills: vec!["公式推导逐步走".into()],
            tools: read_tools(&["list_notebooks", "search_notebook", "get_knowledge_tree"]),
            tool_tier: "read".into(),
        },
        PresetRole {
            id: "kb-butler".into(),
            name: "知识库管家".into(),
            description: "通用问答：先查用户库再回答，引用来源，查不到就明说。".into(),
            system_prompt: "你是学习库管家。回答前先用检索工具查用户的知识库与笔记，答案引用来源（文件名+位置）；库里没有的内容要明说「库中未找到」。回答用中文。".into(),
            skills: vec![],
            tools: read_tools(&["list_notebooks", "search_notebook", "get_source", "search_notes", "get_knowledge_tree"]),
            tool_tier: "read".into(),
        },
        PresetRole {
            id: "web-researcher".into(),
            name: "外联研究员".into(),
            description: "唯一能直接上网的角色：内置 web_search 搜 SearXNG、fetch_page 读原文，也可加你在 MCP 注册的外部工具（联网检索/记忆库等）。库外的问题交给它。".into(),
            system_prompt: "你是外联研究员。先用库内检索工具查用户的知识库；库里没有、需要外部信息时，用 web_search 搜索（SearXNG），摘要不够再用 fetch_page 读原文；也可以调用已授权的 MCP 外部工具。回答必须注明每条信息来自库内还是哪个网址，引用给出链接。库里没有、外部也查不到就明说。回答用中文。".into(),
            skills: vec![],
            tools: read_tools(&["list_notebooks", "search_notebook", "get_source", "web_search", "fetch_page"]),
            tool_tier: "network".into(),
        },
    ]
}

#[tauri::command]
pub fn roles_list() -> Result<Vec<PresetRole>> {
    Ok(preset_roles())
}

/// Instantiate a preset as a normal editable agent. Bundled skills referenced
/// by the role get enabled (seeded disabled by default — opting in is the
/// point of choosing a role that names one).
#[tauri::command]
pub fn roles_instantiate(
    state: tauri::State<'_, crate::AppState>,
    preset_id: String,
    name: Option<String>,
) -> Result<Agent> {
    let preset = preset_roles()
        .into_iter()
        .find(|r| r.id == preset_id)
        .ok_or_else(|| Error::Other(format!("没有 id 为 {preset_id} 的预置角色")))?;
    let c = state.db.lock().unwrap();
    for skill in &preset.skills {
        c.execute(
            "UPDATE agent_skills SET enabled=1 WHERE user_id=?1 AND name=?2",
            params![DEFAULT_USER, skill],
        )?;
    }
    upsert_agent(
        &c,
        DEFAULT_USER,
        None,
        name.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&preset.name),
        Some(&preset.description),
        &preset.system_prompt,
        &preset.skills,
        &preset.tools,
        None,
        0.3,
        8,
        true,
        &preset.tool_tier,
    )
}

pub fn list_tools(conn: &Connection, user_id: &str) -> Result<Vec<Tool>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_id, name, description, kind, schema_json, enabled, built_in, created_at
           FROM agent_tools WHERE user_id=?1 OR built_in=1 ORDER BY built_in DESC, name"
    )?;
    let rows = stmt.query_map([user_id], |r| Ok(Tool {
        id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, description: r.get(3)?,
        kind: r.get(4)?, schema_json: r.get(5)?, enabled: r.get(6)?, built_in: r.get(7)?,
        created_at: r.get(8)?,
    }))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn set_tool_enabled(conn: &Connection, user_id: &str, name: &str, enabled: bool) -> Result<()> {
    let n = conn.execute(
        "UPDATE agent_tools SET enabled=?1 WHERE user_id=?2 AND name=?3",
        params![enabled as i64, user_id, name],
    )?;
    if n == 0 { return Err(Error::NotFound(format!("tool {name}"))); }
    Ok(())
}

fn parse_agent(r: &rusqlite::Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, description: r.get(3)?,
        system_prompt: r.get(4)?,
        skills: serde_json::from_str(r.get::<_, String>(5)?.as_str()).unwrap_or_default(),
        tools: serde_json::from_str(r.get::<_, String>(6)?.as_str()).unwrap_or_default(),
        model: r.get(7)?, temperature: r.get(8)?, max_steps: r.get(9)?, enabled: r.get(10)?,
        tool_tier: normalize_tier(r.get::<_, String>(11)?.as_str()).to_string(),
        created_at: r.get(12)?, updated_at: r.get(13)?,
    })
}


    #[test]
    fn thread_links_runs_and_shares_context_across_agents() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = upsert_agent(&c, "u1", None, "课程助教", Some("tutor"), "p", &[], &[], None, 0.5, 8, true, "read").unwrap();
        let b = upsert_agent(&c, "u1", None, "复习教练", Some("coach"), "p", &[], &[], None, 0.5, 8, true, "read").unwrap();

        // Agent A opens the thread.
        let r1 = start_run(&c, "u1", &a.id, "特征值是什么？").unwrap();
        assert_eq!(r1.thread_id, "", "legacy start_run stays ungrouped");
        // Explicit thread id: the first run DEFINED it (we pass the run's own id prefix? no —
        // a caller-generated id); here we use the run's id as the thread handle.
        let thread = "th-1";
        let _r2 = start_run_in_thread(&c, "u1", &a.id, "特征值是什么？", Some(thread)).unwrap();
        let r3 = start_run_in_thread(&c, "u1", &b.id, "换成复习教练：刚才的内容怎么考？", Some(thread)).unwrap();
        assert_eq!(r3.thread_id, thread);
        assert_ne!(r3.agent_id, a.id);

        // The transcript holds BOTH agents' messages, oldest first, attributed.
        let msgs = thread_messages(&c, "u1", thread, 100).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].agent_id.as_deref(), Some(a.id.as_str()));
        assert_eq!(msgs[0].agent_name.as_deref(), Some("课程助教"));
        assert_eq!(msgs[1].agent_id.as_deref(), Some(b.id.as_str()));
        assert_eq!(msgs[1].content, "换成复习教练：刚才的内容怎么考？");

        // The second run's CONTEXT is the whole thread — the coach sees the tutor.
        let hist = history_for_run(&c, "u1", &r3.id, 100).unwrap();
        assert_eq!(hist.len(), 2);
        assert!(hist.iter().any(|m| m.content.contains("特征值是什么？")));
        // Legacy ungrouped run keeps its own messages only.
        let hist1 = history_for_run(&c, "u1", &r1.id, 100).unwrap();
        assert_eq!(hist1.len(), 1);
        // Another user cannot read the thread.
        assert!(thread_messages(&c, "u2", thread, 10).is_err());
        // A thread owned by someone else cannot be joined.
        assert!(start_run_in_thread(&c, "u2", &a.id, "x", Some(thread)).is_err());
    }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preset_roles_are_consistent() {
        let roles = preset_roles();
        assert!(roles.len() >= 5);
        let mut ids: Vec<&str> = roles.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), roles.len(), "preset ids must be unique");
        let bundled: std::collections::HashSet<&str> =
            BUNDLED_SKILLS.iter().map(|(n, _, _)| *n).collect();
        for r in &roles {
            assert!(
                TOOL_TIERS.contains(&r.tool_tier.as_str()),
                "{} has invalid tier {}",
                r.id,
                r.tool_tier
            );
            assert!(!r.system_prompt.trim().is_empty());
            for sk in &r.skills {
                assert!(bundled.contains(sk.as_str()), "{} references unknown skill {}", r.id, sk);
            }
        }
        // every role only whitelists built-in (non-mcp) tools
        for r in &roles {
            assert!(r.tools.iter().all(|t| !t.starts_with("mcp__")), "{} has mcp tool", r.id);
        }
    }

    #[test]
    fn agent_run_isolated_by_user() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = upsert_agent(&c, "u1", None, "Tutor", None, "Be helpful", &["skill-a".into()], &["tool-list-notebooks".into()], None, 0.3, 5, true, "read").unwrap();
        let b = upsert_agent(&c, "u2", None, "Tutor2", None, "Be helpful", &[], &[], None, 0.3, 5, true, "read").unwrap();
        assert!(a.id != b.id);
        assert_eq!(list_agents(&c, "u1").unwrap().len(), 1);
        let r = start_run(&c, "u1", &a.id, "hi").unwrap();
        assert!(list_messages(&c, "u2", &r.id, 100).is_err());
        append_message(&c, &r.id, "assistant", "hi back", None).unwrap();
        append_message(&c, &r.id, "tool", "ok", Some("search")).unwrap();
        finish_run(&c, &r.id, "success", Some("hi back"), None).unwrap();
        let listed = list_runs(&c, "u1", 10).unwrap();
        assert_eq!(listed[0].status, "success");
        let tools = list_tools(&c, "u1").unwrap();
        assert!(tools.iter().any(|t| t.name == "list_notebooks"));
    }
    #[test]
    fn upsert_skill_validates_body() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        assert!(upsert_skill(&c, "u1", None, "x", "", "", true).is_err());
        let s = upsert_skill(&c, "u1", None, "ok", "d", "b", true).unwrap();
        assert_eq!(s.name, "ok");
    }

    #[test]
    fn bundled_skills_seed_once_and_respect_user_edits() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        // first seed inserts every template, disabled
        let n = seed_bundled_skills(&c).unwrap();
        assert_eq!(n, BUNDLED_SKILLS.len());
        let skills = list_skills(&c, DEFAULT_USER).unwrap();
        assert_eq!(skills.len(), BUNDLED_SKILLS.len());
        assert!(skills.iter().all(|s| !s.enabled), "templates start disabled");
        // every template body is non-trivial (a typo'd template is useless)
        for s in &skills {
            assert!(s.body.len() > 100, "template body too short: {}", s.name);
        }

        // user edits one copy's body and enables it
        let edited = skills.iter().find(|s| s.name == "文献综述骨架").unwrap();
        upsert_skill(&c, DEFAULT_USER, Some(&edited.id), &edited.name,
                     &edited.description, "我的定制版本", true).unwrap();

        // second seed is a no-op — the edited, enabled copy survives untouched
        assert_eq!(seed_bundled_skills(&c).unwrap(), 0);
        let after = list_skills(&c, DEFAULT_USER).unwrap();
        assert_eq!(after.len(), BUNDLED_SKILLS.len());
        let mine = after.iter().find(|s| s.name == "文献综述骨架").unwrap();
        assert_eq!(mine.body, "我的定制版本");
        assert!(mine.enabled);
    }
    // ---- skill portability (0090) ------------------------------------------

    fn skill_temp_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cortex-skill-{}", crate::db::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn skill_file_rejects_wrong_schema_and_missing_body() {
        let bad = r#"{"schema":"other/v1","name":"x","body":"y"}"#;
        assert!(parse_skill_file(bad)
            .unwrap_err()
            .to_string()
            .contains("格式不匹配"));
        let no_body = r#"{"schema":"cortex-skill/v1","name":"x","body":"  "}"#;
        assert!(parse_skill_file(no_body)
            .unwrap_err()
            .to_string()
            .contains("body"));
        let ok = r#"{"schema":"cortex-skill/v1","version":2,"name":" 模板 ","body":" 步骤 "}"#;
        let f = parse_skill_file(ok).unwrap();
        assert_eq!(f.name, "模板");
        assert_eq!(f.body, "步骤");
        assert_eq!(f.version, 2);
    }

    #[test]
    fn skill_export_then_import_roundtrips_version_and_source() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sk = upsert_skill(&c, DEFAULT_USER, None, "推导模板", "描述", "第一步…", true).unwrap();
        assert_eq!(sk.source, "local");
        assert_eq!(sk.version, 1);

        let dir = skill_temp_dir();
        let dest = dir.join("s.json");
        export_skill(&c, DEFAULT_USER, &sk.id, &dest).unwrap();
        let raw = std::fs::read_to_string(&dest).unwrap();
        assert!(raw.contains("cortex-skill/v1"), "{raw}");

        let bumped = raw.replace("\"version\": 1", "\"version\": 3");
        assert!(bumped.contains("\"version\": 3"), "{bumped}");
        std::fs::write(&dest, bumped).unwrap();
        let imported = import_skill(&c, DEFAULT_USER, &dest, true).unwrap();
        assert_eq!(imported.source, "imported");
        assert_eq!(imported.version, 3);
        assert_eq!(imported.body, "第一步…");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_import_refuses_same_name_without_overwrite() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        upsert_skill(&c, DEFAULT_USER, None, "同名", "", "旧正文", true).unwrap();
        let dir = skill_temp_dir();
        let dest = dir.join("s.json");
        std::fs::write(
            &dest,
            r#"{"schema":"cortex-skill/v1","name":"同名","body":"新正文"}"#,
        )
        .unwrap();
        let err = import_skill(&c, DEFAULT_USER, &dest, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("已存在"), "{err}");
        let out = import_skill(&c, DEFAULT_USER, &dest, true).unwrap();
        assert_eq!(out.body, "新正文");
        assert_eq!(out.source, "imported");
        let _ = std::fs::remove_dir_all(&dir);
    }

}
