//! Editable prompts.
//!
//! Prompt text used to live as a `const` at its call site: invisible from the
//! UI, unversioned, and changeable only by recompiling. Every prompt worth
//! tuning is registered here under a stable key. The module that *uses* a
//! prompt still owns its default text — this file only points at it, so the
//! text exists once.
//!
//! An override lives in `settings` under `prompt.<key>`. Resolution goes
//! through a process-wide snapshot rather than taking a `Connection`: prompts
//! are read deep inside model calls that have no `Conn` and should not grow
//! one. The snapshot refreshes at startup and after every save/reset, which is
//! enough for a single-user local-first app.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use rusqlite::Connection;

use crate::error::{Error, Result};

// ---- keys (stable; they are also the settings suffix) --------------------

pub const RESEARCH_CHAT: &str = "research.chat";
pub const LIT_PLAN: &str = "literature.plan";
pub const LIT_RERANK: &str = "literature.rerank";
pub const SUMMARY_PART: &str = "notes.summary.part";
pub const SUMMARY_SOURCE: &str = "notes.summary.source";
pub const SUMMARY_REDUCE: &str = "notes.summary.reduce";
pub const SUMMARY_GROUP: &str = "notes.summary.group";
pub const SUMMARY_SUBJECT: &str = "notes.summary.subject";
pub const CHEATSHEET_MAP: &str = "cheatsheet.map";
pub const SKILL_DISTILL: &str = "skill.distill";
pub const TENSION_ANALYZE: &str = "tension.analyze";
pub const NODE_DOC: &str = "node.doc";
pub const TUTOR_TURN: &str = "tutor.turn";
pub const VOCAB_MINE: &str = "vocab.mine";
pub const REDTEAM_REVIEW: &str = "redteam.review";

const PREFIX: &str = "prompt.";

// ---- registry -----------------------------------------------------------

pub struct Spec {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    /// The built-in text. Also the fallback when an override fails to load.
    pub default: &'static str,
    /// A realistic input the lab can run without needing your data — a prompt
    /// you cannot try once is a prompt you cannot tune.
    pub sample: &'static str,
}

static SPECS: &[Spec] = &[
    Spec {
        key: RESEARCH_CHAT,
        label: "研究对话",
        group: "研究",
        default: crate::research_agent::SYSTEM,
        sample: "最近有哪些团队在做检索增强生成（RAG）？给我几个具体的人和他们的代表作。",
    },
    Spec {
        key: TENSION_ANALYZE,
        label: "观点分歧分析",
        group: "研究",
        default: crate::tensions::SYSTEM,
        sample: "PAPERS:\n### PAPER 1 (title: Scaling laws for retrieval)\nScaling retrieval helps up to 10^9 tokens; beyond that gains flatten.\n\n### PAPER 2 (title: Retrieval is not all you need)\nLong-context models match retrieval pipelines at comparable cost.\n\nReturn the tensions JSON now.",
    },
    Spec {
        key: REDTEAM_REVIEW,
        label: "顶刊红队评审",
        group: "研究",
        default: crate::redteam::SYSTEM,
        sample: "VENUE: 顶刊（NeurIPS / CVPR 级）\nPAPER TITLE: TinyBERT-12: A 12-layer student distilled from BERT-base\nPAPER CARD:\nWe distill BERT-base into a 12-layer student. On GLUE, TinyBERT-12 reaches 96.8% of the teacher's score with 7.5x smaller size and 9.4x faster inference. We evaluate on GLUE dev sets only.\n\nReturn the review JSON now.",
    },
    Spec {
        key: LIT_PLAN,
        label: "深度检索 · 拆题",
        group: "文献检索",
        default: crate::literature::PLAN_SYS,
        sample: "间隔重复在二语词汇长期记忆中的效果，实证研究",
    },
    Spec {
        key: LIT_RERANK,
        label: "深度检索 · 重排",
        group: "文献检索",
        default: crate::literature::RERANK_SYS,
        sample: "Question: Does spaced repetition help long-term vocabulary retention?\n\n1. [2019] Spaced retrieval practice and L2 vocabulary (Language Learning) — 60 learners, 12-week retention test.\n2. [2021] Massed vs distributed practice in adult learners (JEP: General) — 3 experiments, motor tasks.\n3. [2016] Sleep-dependent consolidation of word memories (Cortex) — overnight consolidation of new vocabulary.",
    },
    Spec {
        key: SUMMARY_PART,
        label: "片段要点",
        group: "材料摘要",
        default: crate::summary_tree::PART_SUM_SYS,
        sample: "间隔重复（spaced repetition）指把复习分散到逐渐拉长的时间间隔中，而不是集中突击。Ebbinghaus 在 1885 年提出的遗忘曲线显示，新学材料的遗忘在前 24 小时最陡。现代实现通常用 FSRS 或 SM-2 之类的算法，根据每次回忆的难度动态调整下一次复习的间隔。实证研究普遍发现，间隔练习在延迟测验上的保持率显著高于集中练习，但在即时测验上优势不明显，甚至略差。",
    },
    Spec {
        key: SUMMARY_SOURCE,
        label: "单篇摘要",
        group: "材料摘要",
        default: crate::summary_tree::SOURCE_SUM_SYS,
        sample: "本章讨论检索增强生成的三种失败模式：检索到但不相关、相关但被忽略、以及引用错位。作者用 240 个问答对做了消融实验，发现重排阶段贡献了 60% 的端到端提升，而扩大召回数量在超过 20 条后几乎没有边际收益。作者建议先修重排，再考虑扩容。",
    },
    Spec {
        key: SUMMARY_REDUCE,
        label: "单篇合并",
        group: "材料摘要",
        default: crate::summary_tree::REDUCE_SUM_SYS,
        sample: "「片段 1」间隔练习在延迟测验上优于集中练习，即时测验上无优势。\n「片段 2」FSRS 依据回忆难度动态调整间隔，比固定间隔提高约 20% 的保持率。\n「片段 3」作者提醒：间隔过长会导致遗忘到无法回忆，间隔存在最优区间。",
    },
    Spec {
        key: SUMMARY_GROUP,
        label: "主题摘要",
        group: "材料摘要",
        default: crate::summary_tree::GROUP_SUM_SYS,
        sample: "「教材第 3 章」间隔重复的算法实现与参数选择。\n「论文 A」间隔练习与集中练习的对照实验，延迟测验效应量 d=0.6。\n「笔记」我自己的复习节奏：每天 20 分钟，卡片到期就复习。",
    },
    Spec {
        key: SUMMARY_SUBJECT,
        label: "科目总览",
        group: "材料摘要",
        default: crate::summary_tree::SUBJECT_SUM_SYS,
        sample: "「记忆与复习」间隔重复的原理、算法与实证结论。\n「检索与重排」多源检索、RRF 融合与 LLM 重排的分工。\n「学习方法」费曼技巧、刻意练习与元认知监控。",
    },
    Spec {
        key: CHEATSHEET_MAP,
        label: "速查表提炼",
        group: "卡片与笔记",
        default: crate::commands::CHEATSHEET_MAP_SYSTEM,
        sample: "考纲条目：TCP 三次握手、拥塞控制、慢启动、快重传\n材料节选：TCP 建立连接需要三次握手，SYN、SYN-ACK、ACK。拥塞控制包含慢启动、拥塞避免、快重传与快恢复四个阶段。慢启动阶段拥塞窗口指数增长，直到达到慢启动阈值。",
    },
    Spec {
        key: SKILL_DISTILL,
        label: "方法卡提炼",
        group: "卡片与笔记",
        default: crate::skill_cards::SYSTEM,
        sample: "CONCEPT: 间隔重复\nAPPEARS IN (3 books): 《认知天性》、《Make It Stick》、《学习之道》\nReturn the skill card JSON now.",
    },
    Spec {
        key: NODE_DOC,
        label: "知识点讲解",
        group: "知识框架",
        default: crate::node_docs::SYSTEM,
        sample: "知识点：拥塞控制（topic）\n层号：第 1 层（0 为基础层，没有前置）\n框架里的摘要：发送方如何感知网络容量并调整发送速率\n前置知识点（按依赖顺序）：TCP 握手\n\n用户笔记里提到它的原文：\n—— 《BBR 笔记》\nBBR 的瓶颈带宽估计很关键，它不再把丢包当信号。\n\n按系统提示的规则写讲解材料，结尾用 ---WHY--- 分隔顺序说明。",
    },
    Spec {
        key: TUTOR_TURN,
        label: "陪练回合",
        group: "陪练",
        default: crate::tutor::SYSTEM,
        sample: "陪练对象：TCP 握手\n当前档位：第 3 档「提问引导」\n学生信心：4 / 5\n\n最近的对话：\n教练：第一题：SYN 在握手里做什么？\n学生：同步序列号吧\n\n学生这一轮的回答：同步序列号吧\n\n按规则输出 VERDICT 行和你的回复。",
    },
    Spec {
        key: VOCAB_MINE,
        label: "语料采词",
        group: "卡片与笔记",
        default: crate::vocab::MINE_SYS,
        sample: "材料：\nSpaced repetition exploits the spacing effect: distributing study over time improves long-term retention compared with massed practice. The scheduler adjusts each interval from the learner's recall difficulty.\n\n返回 JSON。",
    },
];

/// The whole registry, in display order.
pub fn specs() -> &'static [Spec] {
    SPECS
}

pub fn spec(key: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.key == key)
}

// ---- resolution ---------------------------------------------------------

fn cache() -> &'static RwLock<HashMap<String, String>> {
    static OVERRIDES: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
    OVERRIDES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// The text a model call should use: the override when one is set, otherwise
/// the built-in default. Unknown keys return an empty string rather than a
/// guess — a prompt key is always a constant from this module, so an empty
/// result is a bug worth noticing, not a behaviour worth inventing.
pub fn text(key: &str) -> String {
    if let Ok(guard) = cache().read() {
        if let Some(v) = guard.get(key) {
            return v.clone();
        }
    }
    spec(key).map(|s| s.default.to_string()).unwrap_or_default()
}

pub fn is_custom(key: &str) -> bool {
    cache().read().map(|g| g.contains_key(key)).unwrap_or(false)
}

/// Load every override from `settings`. Called once at startup and again after
/// any save/reset, so the snapshot never depends on a lock being held across a
/// model call.
pub fn refresh(conn: &Connection) -> Result<()> {
    let mut next: HashMap<String, String> = HashMap::new();
    let mut st = conn.prepare("SELECT key, value FROM settings WHERE key LIKE 'prompt.%'")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (full, value) = row?;
        let Some(key) = full.strip_prefix(PREFIX) else { continue };
        // A row for a retired key (or an empty body) is ignored rather than
        // resurrected — the registry is the source of truth for what exists.
        if value.trim().is_empty() || spec(key).is_none() {
            continue;
        }
        next.insert(key.to_string(), value);
    }
    *cache().write().unwrap() = next;
    Ok(())
}

pub fn save(conn: &Connection, key: &str, body: &str) -> Result<()> {
    let s = spec(key).ok_or_else(|| Error::Other(format!("unknown prompt key: {key}")))?;
    let body = body.trim_end();
    // Saving the default back is the same as not having an override; keeping a
    // copy would make "is this customized?" lie after the next app update.
    if body.trim().is_empty() || body == s.default.trim_end() {
        return reset(conn, key);
    }
    crate::repo::set_setting(conn, &format!("{PREFIX}{key}"), body)?;
    refresh(conn)
}

pub fn reset(conn: &Connection, key: &str) -> Result<()> {
    if spec(key).is_none() {
        return Err(Error::Other(format!("unknown prompt key: {key}")));
    }
    conn.execute("DELETE FROM settings WHERE key = ?1", [format!("{PREFIX}{key}")])?;
    refresh(conn)
}

// ---- what the lab shows -------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct PromptInfo {
    pub key: String,
    pub label: String,
    pub group: String,
    /// Effective text (override if any) — what the next model call will use.
    pub body: String,
    pub default_body: String,
    pub sample: String,
    pub custom: bool,
}

pub fn list() -> Vec<PromptInfo> {
    SPECS
        .iter()
        .map(|s| PromptInfo {
            key: s.key.to_string(),
            label: s.label.to_string(),
            group: s.group.to_string(),
            body: text(s.key),
            default_body: s.default.to_string(),
            sample: s.sample.to_string(),
            custom: is_custom(s.key),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> crate::db::AppState {
        crate::db::AppState::in_memory().unwrap()
    }

    const ALL_KEYS: &[&str] = &[
        RESEARCH_CHAT,
        LIT_PLAN,
        LIT_RERANK,
        SUMMARY_PART,
        SUMMARY_SOURCE,
        SUMMARY_REDUCE,
        SUMMARY_GROUP,
        SUMMARY_SUBJECT,
        CHEATSHEET_MAP,
        SKILL_DISTILL,
        TENSION_ANALYZE,
        NODE_DOC,
        TUTOR_TURN,
        VOCAB_MINE,
        REDTEAM_REVIEW,
    ];

    #[test]
    fn every_key_is_registered_exactly_once() {
        for key in ALL_KEYS {
            assert_eq!(specs().iter().filter(|s| s.key == *key).count(), 1, "{key}");
            let s = spec(key).unwrap();
            assert!(!s.default.trim().is_empty(), "{key} has no default text");
            assert!(!s.sample.trim().is_empty(), "{key} has no sample input");
        }
        assert_eq!(specs().len(), ALL_KEYS.len(), "unregistered or duplicate specs");
    }

    /// Keys are persisted in `settings`, so renaming one silently orphans a
    /// user's override. Pin them.
    #[test]
    fn keys_are_stable_strings() {
        let mut keys: Vec<&str> = SPECS.iter().map(|s| s.key).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "cheatsheet.map",
                "literature.plan",
                "literature.rerank",
                "node.doc",
                "notes.summary.group",
                "notes.summary.part",
                "notes.summary.reduce",
                "notes.summary.source",
                "notes.summary.subject",
                "redteam.review",
                "research.chat",
                "skill.distill",
                "tension.analyze",
                "tutor.turn",
                "vocab.mine",
            ]
        );
    }

    #[test]
    fn unknown_keys_fall_back_to_nothing_rather_than_a_guess() {
        assert_eq!(text("nope.not.a.prompt"), "");
        assert!(spec("nope.not.a.prompt").is_none());
    }

    #[test]
    fn an_override_survives_a_refresh_and_reset_drops_it() {
        let st = state();
        let c = st.db.lock().unwrap();
        refresh(&c).unwrap();
        let original = text(LIT_PLAN);

        save(&c, LIT_PLAN, "You plan searches. Output a JSON array.").unwrap();
        assert_eq!(text(LIT_PLAN), "You plan searches. Output a JSON array.");
        assert!(is_custom(LIT_PLAN));

        // A fresh snapshot from the DB must see the same thing — this is what
        // happens on the next app start.
        *cache().write().unwrap() = HashMap::new();
        refresh(&c).unwrap();
        assert_eq!(text(LIT_PLAN), "You plan searches. Output a JSON array.");

        reset(&c, LIT_PLAN).unwrap();
        assert_eq!(text(LIT_PLAN), original);
        assert!(!is_custom(LIT_PLAN));
    }

    #[test]
    fn saving_the_default_is_not_an_override() {
        let st = state();
        let c = st.db.lock().unwrap();
        refresh(&c).unwrap();
        let default = spec(LIT_PLAN).unwrap().default.to_string();
        save(&c, LIT_PLAN, &default).unwrap();
        assert!(!is_custom(LIT_PLAN), "an override equal to the default is noise");

        // Blanking the box is also a reset, not an empty prompt sent to a model.
        save(&c, LIT_PLAN, "   \n ").unwrap();
        assert!(!is_custom(LIT_PLAN));
        assert_eq!(text(LIT_PLAN), default);
    }

    #[test]
    fn a_retired_key_left_in_settings_is_ignored() {
        let st = state();
        let c = st.db.lock().unwrap();
        crate::repo::set_setting(&c, "prompt.gone.forever", "stale body").unwrap();
        refresh(&c).unwrap();
        assert_eq!(text("gone.forever"), "");
        assert!(list().iter().all(|p| p.key != "gone.forever"));
    }

    #[test]
    fn every_registry_entry_has_a_group_and_a_label() {
        for s in specs() {
            assert!(!s.group.trim().is_empty(), "{}", s.key);
            assert!(!s.label.trim().is_empty(), "{}", s.key);
        }
    }
}
