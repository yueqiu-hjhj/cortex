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
use rusqlite::{params, Connection};
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
                temperature, max_steps, enabled, created_at, updated_at
           FROM agents WHERE user_id=?1 ORDER BY updated_at DESC"
    )?;
    let rows = stmt.query_map([user_id], parse_agent)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn get_agent(conn: &Connection, user_id: &str, id: &str) -> Result<Agent> {
    conn.query_row(
        "SELECT id, user_id, name, description, system_prompt, skills, tools, model,
                temperature, max_steps, enabled, created_at, updated_at
           FROM agents WHERE user_id=?1 AND id=?2",
        [user_id, id],
        parse_agent,
    ).map_err(|e| Error::NotFound(format!("agent {id} ({e})")))
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
) -> Result<Agent> {
    if name.trim().is_empty() { return Err(Error::Other("agent name is required".into())); }
    if !(0.0..=2.0).contains(&temperature) { return Err(Error::Other("temperature must be 0..=2".into())); }
    if !(1..=64).contains(&max_steps) { return Err(Error::Other("max_steps must be 1..=64".into())); }
    let skills = serde_json::to_string(skills)?;
    let tools = serde_json::to_string(tools)?;
    let now = crate::db::now_ms();
    let id = match id {
        Some(existing) => {
            let updated = conn.execute(
                "UPDATE agents SET name=?1, description=?2, system_prompt=?3, skills=?4,
                  tools=?5, model=?6, temperature=?7, max_steps=?8, enabled=?9, updated_at=?10
                  WHERE user_id=?11 AND id=?12",
                params![name, description, system_prompt, skills, tools, model, temperature, max_steps, enabled as i64, now, user_id, existing],
            )?;
            if updated == 0 { return Err(Error::NotFound(format!("agent {existing}"))); }
            existing.to_string()
        }
        None => {
            let id = crate::db::new_id();
            conn.execute(
                "INSERT INTO agents (id, user_id, name, description, system_prompt, skills, tools,
                    model, temperature, max_steps, enabled, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)",
                params![id, user_id, name, description, system_prompt, skills, tools, model, temperature, max_steps, enabled as i64, now],
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
        thread_id: thread })
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

pub fn list_runs(conn: &Connection, user_id: &str, limit: i64) -> Result<Vec<AgentRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, user_id, status, input, result, error, steps, started_at, finished_at, thread_id
           FROM agent_runs WHERE user_id=?1 ORDER BY started_at DESC LIMIT ?2"
    )?;
    let rows = stmt.query_map(params![user_id, limit], |r| {
        Ok(AgentRun {
            id: r.get(0)?, agent_id: r.get(1)?, user_id: r.get(2)?,
            status: r.get(3)?, input: r.get(4)?, result: r.get(5)?, error: r.get(6)?,
            steps: r.get(7)?, started_at: r.get(8)?, finished_at: r.get(9)?,
            thread_id: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
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
        "SELECT id, user_id, name, description, body, enabled, created_at, updated_at
           FROM agent_skills WHERE user_id=?1 ORDER BY name"
    )?;
    let rows = stmt.query_map([user_id], |r| Ok(Skill {
        id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, description: r.get(3)?,
        body: r.get(4)?, enabled: r.get(5)?, created_at: r.get(6)?, updated_at: r.get(7)?,
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
                "INSERT INTO agent_skills (id, user_id, name, description, body, enabled, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
                params![id, user_id, name, description, body, enabled as i64, now],
            )?;
            id
        }
    };
    conn.query_row(
        "SELECT id, user_id, name, description, body, enabled, created_at, updated_at
           FROM agent_skills WHERE id=?1", [id], |r| Ok(Skill {
            id: r.get(0)?, user_id: r.get(1)?, name: r.get(2)?, description: r.get(3)?,
            body: r.get(4)?, enabled: r.get(5)?, created_at: r.get(6)?, updated_at: r.get(7)?,
        }),
    ).map_err(Into::into)
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
        created_at: r.get(11)?, updated_at: r.get(12)?,
    })
}


    #[test]
    fn thread_links_runs_and_shares_context_across_agents() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = upsert_agent(&c, "u1", None, "课程助教", Some("tutor"), "p", &[], &[], None, 0.5, 8, true).unwrap();
        let b = upsert_agent(&c, "u1", None, "复习教练", Some("coach"), "p", &[], &[], None, 0.5, 8, true).unwrap();

        // Agent A opens the thread.
        let r1 = start_run(&c, "u1", &a.id, "特征值是什么？").unwrap();
        assert_eq!(r1.thread_id, "", "legacy start_run stays ungrouped");
        // Explicit thread id: the first run DEFINED it (we pass the run's own id prefix? no —
        // a caller-generated id); here we use the run's id as the thread handle.
        let thread = "th-1";
        let r2 = start_run_in_thread(&c, "u1", &a.id, "特征值是什么？", Some(thread)).unwrap();
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
    fn agent_run_isolated_by_user() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let a = upsert_agent(&c, "u1", None, "Tutor", None, "Be helpful", &["skill-a".into()], &["tool-list-notebooks".into()], None, 0.3, 5, true).unwrap();
        let b = upsert_agent(&c, "u2", None, "Tutor2", None, "Be helpful", &[], &[], None, 0.3, 5, true).unwrap();
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
}
