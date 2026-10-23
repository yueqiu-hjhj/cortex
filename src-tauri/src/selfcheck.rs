//! Startup self-check. Runs after migrations are applied and the DB is
//! opened, but BEFORE the UI starts fetching data. The check is non-fatal:
//! every problem becomes a Check { ok: false, warn: false, name, hint } row that the
//! frontend can show as a banner, but the app still starts.
//!
//! Why non-fatal: a single missing ffmpeg binary should never block the
//! user from opening their cheatsheets. The OS notification / banner is the
//! right surface for "something is off" - the user can keep working while
//! they read the details.

use crate::error::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    /// True for a condition worth surfacing that must NOT fail the check:/r/n    /// bad library data is not a reason to refuse to start.
    pub warn: bool,
    pub detail: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub passed: usize,
    pub failed: usize,
    pub checks: Vec<Check>,
    pub user_version: i64,
    pub known_migrations: usize,
}

fn check_db_integrity(conn: &Connection) -> Check {
    let res: rusqlite::Result<String> = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0));
    match res {
        Ok(s) if s.eq_ignore_ascii_case("ok") => Check { name: "DB integrity", ok: true, warn: false, detail: "ok".into(), hint: None },
        Ok(other) => Check { name: "DB integrity", ok: false, warn: false, detail: other, hint: Some("Restore from a sync snapshot or rebuild the local DB.".into()) },
        Err(e) => Check { name: "DB integrity", ok: false, warn: false, detail: e.to_string(), hint: Some("DB could not be opened.".into()) },
    }
}

fn check_migration_version(conn: &Connection, known: usize) -> (Check, i64) {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);
    let ok = v as usize == known;
    let check = Check {
        name: "Schema version",
        ok,
        warn: false,
        detail: format!("user_version={} (expected {})", v, known),
        hint: if ok { None } else { Some("Run pending migrations before the next launch.".into()) },
    };
    (check, v)
}

fn check_disk_space(data_dir: &Path) -> Check {
    // The data_dir may not exist on first launch; create it then probe.
    let probe = data_dir.join(".cortex-disk-probe");
    let _ = std::fs::write(&probe, b"ok");
    let _ = std::fs::remove_file(&probe);
    // Available space via statvfs-equivalent on Windows: GetDiskFreeSpaceExW.
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let path: Vec<u16> = data_dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut free: u64 = 0;
        let ok = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        if ok == 0 {
            return Check { name: "Disk space", ok: false, warn: false, detail: "GetDiskFreeSpaceExW failed".into(), hint: None };
        }
        let mb = free / 1_048_576;
        let warn = mb < 500;
        Check {
            name: "Disk space",
            ok: !warn, warn: false,
            detail: format!("{} MB free", mb),
            hint: if warn { Some("Less than 500 MB free; embedding model + downloaded binaries need room.".into()) } else { None },
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Fallback: statvfs via rustix is overkill; report "unknown" rather than fail.
        Check { name: "Disk space", ok: true, warn: false, detail: "n/a".into(), hint: None }
    }
}

fn check_data_dir_writable(data_dir: &Path) -> Check {
    let probe = data_dir.join(".cortex-write-test");
    match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
        Ok(_) => Check { name: "Data dir writable", ok: true, warn: false, detail: data_dir.display().to_string(), hint: None },
        Err(e) => Check { name: "Data dir writable", ok: false, warn: false, detail: e.to_string(), hint: Some("Check the data dir permissions (e.g. on Linux: a stale ~/.local/share ownership).".into()) },
    }
}

fn check_onnx_model_if_used(conn: &Connection, data_dir: &Path) -> Check {
    let provider: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key='embed_provider'", [], |r| r.get(0))
        .ok();
    if provider.as_deref() != Some("onnx") {
        return Check { name: "Local ONNX model", ok: true, warn: false, detail: "not the active provider".into(), hint: None };
    }
    let model = data_dir.join("models").join("onnx").join("model_quantized.onnx");
    let tok = data_dir.join("models").join("onnx").join("tokenizer.json");
    if model.exists() && tok.exists() {
        Check { name: "Local ONNX model", ok: true, warn: false, detail: "present".into(), hint: None }
    } else {
        Check { name: "Local ONNX model", ok: false, warn: false, detail: "model_quantized.onnx or tokenizer.json missing".into(), hint: Some("Open Settings › Local ONNX embeddings and click Download.".into()) }
    }
}

/// 环境检测：能跑模型吗？任何一个云 key、注册表 provider key、Ollama 地址，
/// 或用户显式开了离线模式（offline stub / 本地 Ollama）都算就绪。
fn check_model_configured(conn: &Connection) -> Check {
    let keys = crate::commands::read_keys(conn);
    match keys {
        Ok(k) => {
            let cloud = k.gemini.is_some()
                || k.openrouter.is_some()
                || k.openai.is_some()
                || k.claude.is_some()
                || k.custom_api_key.is_some()
                || !k.extra.is_empty();
            if cloud || k.ollama_url.is_some() || k.offline {
                let how = if cloud {
                    "云端 key 已配置"
                } else if k.ollama_url.is_some() {
                    "本地 Ollama 已配置"
                } else {
                    "离线模式已开启"
                };
                Check { name: "Model available", ok: true, warn: false, detail: how.into(), hint: None }
            } else {
                Check {
                    name: "Model available",
                    ok: false, warn: false,
                    detail: "没有配置任何模型 API key".into(),
                    hint: Some("在 设置 › 模型 里添加 key（或开启离线模式）。没有模型时对话/生成/智能体都无法运行。".into()),
                }
            }
        }
        Err(e) => Check { name: "Model available", ok: false, warn: false, detail: e.to_string(), hint: None },
    }
}

/// 向量空间一致性（0081）。一次「换了嵌入模型但不重新索引」会把库切成多个
/// 空间，而检索只在查询自身的空间里找——其余来源静默消失（不是报错，是根本
/// 不出现在结果里）。未标注行（0081 之前的旧数据）对任何空间都可见，是升级
/// 兼容路径的一部分，不算分裂。
fn check_embedding_spaces(conn: &Connection) -> Check {
    let counts = match crate::repo::chunk_space_counts(conn) {
        Ok(c) => c,
        Err(e) => return Check { name: "Embedding spaces", ok: false, warn: false, detail: e.to_string(), hint: None },
    };
    let labeled: Vec<&(String, i64)> = counts.iter().filter(|(s, _)| s != "(未标注)").collect();
    match labeled.len() {
        0 => Check { name: "Embedding spaces", ok: true, warn: false, detail: "no chunks yet".into(), hint: None },
        1 => Check {
            name: "Embedding spaces",
            ok: true, warn: false,
            detail: format!("{} chunks in {}", labeled[0].1, labeled[0].0),
            hint: None,
        },
        _ => {
            let detail = labeled
                .iter()
                .map(|(s, n)| format!("{s}={n}"))
                .collect::<Vec<_>>()
                .join(", ");
            Check {
                name: "Embedding spaces",
                ok: false, warn: false,
                detail,
                hint: Some("库被切成多个向量空间（换了嵌入模型但没有重新索引）——检索只能看到其中一个空间的来源。请重新索引受影响的来源（或全部来源）。".into()),
            }
        }
    }
}

/// 数据健康（回答质量账本）。启动时只做轻量统计——绝不在这里逐条核验引用：那
/// 是一次全库读，会把启动拖慢到用户干脆关掉自检。因此这里只看五态、零命中与
/// 伪造引用三类信号；快照漂移留在 Lab 的引用台账里按需核验。
///
/// 阈值来自 `answer_health::quick_verdicts`，不复制一份数字——两边各写一份，
/// 迟早对同一批数据给出不同结论。**报 warn 不报 fail**：库里回答质量差不是拒绝
/// 启动的理由，但必须被说出来，否则问题只会在无人看见的角落累积。
fn check_answer_health(conn: &Connection) -> Check {
    match crate::answer_health::quick_verdicts(conn, 30) {
        Ok(signals) => {
            let warns: Vec<&str> = signals
                .iter()
                .filter(|s| s.level == "warn")
                .map(|s| s.text.as_str())
                .collect();
            if warns.is_empty() {
                Check {
                    name: "Answer health",
                    ok: true,
                    warn: false,
                    detail: "近 30 天没有异常信号".into(),
                    hint: None,
                }
            } else {
                Check {
                    name: "Answer health",
                    ok: true, // never blocks startup
                    warn: true,
                    detail: warns.join(" · "),
                    hint: Some("在 实验室 › 回答 里看逐条明细，并在引用台账里核验/重锚。".into()),
                }
            }
        }
        // A check that cannot answer is not evidence of health — say so, but
        // don't turn a stats failure into a startup failure.
        Err(e) => Check {
            name: "Answer health",
            ok: true,
            warn: true,
            detail: format!("无法统计回答健康度：{e}"),
            hint: None,
        },
    }
}

pub fn run(conn: &Connection, data_dir: &Path, known_migrations: usize) -> Result<Report> {
    let (version_check, v) = check_migration_version(conn, known_migrations);
    let checks = vec![
        check_db_integrity(conn),
        version_check,
        check_data_dir_writable(data_dir),
        check_disk_space(data_dir),
        check_onnx_model_if_used(conn, data_dir),
        check_model_configured(conn),
        check_embedding_spaces(conn),
        check_answer_health(conn),
    ];
    let passed = checks.iter().filter(|c| c.ok).count();
    let failed = checks.len() - passed;
    Ok(Report { passed, failed, checks, user_version: v, known_migrations })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo;

    #[test]
    fn single_space_with_legacy_rows_is_healthy() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "S", None, None, None).unwrap();
        let s1 = repo::insert_source(&c, &sid, None, "a.md", "md", None).unwrap();
        let s2 = repo::insert_source(&c, &sid, None, "legacy.md", "md", None).unwrap();
        repo::insert_chunk(&c, &s1, &sid, None, 0, "x", None, 2, &[0u8; 8], "onnx:384").unwrap();
        repo::insert_chunk(&c, &s2, &sid, None, 0, "y", None, 2, &[0u8; 8], "").unwrap();
        let check = check_embedding_spaces(&c);
        assert!(check.ok, "one labeled space plus unlabeled rows is the upgrade path, not a split: {check:?}");
    }

    #[test]
    fn two_labeled_spaces_fail_the_check() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "S", None, None, None).unwrap();
        let s1 = repo::insert_source(&c, &sid, None, "a.md", "md", None).unwrap();
        let s2 = repo::insert_source(&c, &sid, None, "b.md", "md", None).unwrap();
        repo::insert_chunk(&c, &s1, &sid, None, 0, "x", None, 2, &[0u8; 8], "onnx:384").unwrap();
        repo::insert_chunk(&c, &s2, &sid, None, 0, "y", None, 2, &[0u8; 8], "gemini:768").unwrap();
        let check = check_embedding_spaces(&c);
        assert!(!check.ok, "a model switch without re-ingest must be surfaced: {check:?}");
        assert!(check.detail.contains("onnx:384") && check.detail.contains("gemini:768"));
    }

    #[test]
    fn unhealthy_answers_warn_without_failing_the_check() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "S", None, None, None).unwrap();
        for _ in 0..6 {
            repo::add_chat_message_full(&c, &sid, "assistant", "答案", &[], None, None, Some("failed"))
                .unwrap();
        }
        let check = check_answer_health(&c);
        assert!(check.ok, "bad library data must never block startup: {check:?}");
        assert!(check.warn, "…and must not pass silently either");
        assert!(check.detail.contains("模型侧不稳定"), "{}", check.detail);
    }

    #[test]
    fn an_empty_library_passes_without_a_warning() {
        let st = crate::db::AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();
        let check = check_answer_health(&c);
        assert!(check.ok && !check.warn, "no answers is not a warning: {check:?}");
    }
}

// `encode_wide` lives in std::os::windows::ffi::OsStrExt; bring it into scope.
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
