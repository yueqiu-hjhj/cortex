//! cortex-paper-meta — batch quality checks for the paper library.
//!
//! Two gaps the competitive dive flagged (see
//! `docs/competitive-deep-dive-2026-10-22.md`, §5B):
//!
//!   1. **No retraction check.** PaperQA2 verifies Crossref metadata and flags
//!      retractions; Cortex happily cited retracted work.
//!   2. **No open-access full text.** PaperQA2 uses Unpaywall; Cortex could find
//!      a paper but not fetch a legal PDF.
//!
//! Both are network-bound, batch facts about a DOI. Doing them at import time
//! would block ingestion and break offline use, so they live in a separate
//! table (`paper_quality`) filled by this tool:
//!
//!   cortex-paper-meta --check          Crossref + Unpaywall for every DOI
//!   cortex-paper-meta --fetch-oa       download the OA PDFs
//!   cortex-paper-meta --report         what we know so far
//!
//! Deliberate design choices:
//!   - **Table is created defensively.** Migration `0083_paper_quality.sql`
//!     owns the schema, but this tool also runs `CREATE TABLE IF NOT EXISTS`,
//!     so it works against a database the app hasn't migrated yet.
//!   - **Never fails the whole run.** A per-paper lookup error is recorded in
//!     `paper_quality.error` and the loop continues.
//!   - **Polite.** 250 ms between papers, a mailto user-agent, 20 s timeout.
//!
//! Retraction detection (defensive — Crossref exposes it three ways):
//!   - `message.updated-by[]` with `type == "retraction"`  (the usual shape)
//!   - `message.relation["is-retracted-by"]`               (newer records)
//!   - `message.type == "retraction"` → this work *is* a notice (flagged
//!     separately as `is_notice`, not as a retracted paper)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

const CROSSREF: &str = "https://api.crossref.org/works/";
const UNPAYWALL: &str = "https://api.unpaywall.org/v2/";
const FRESH_DAYS: i64 = 30;

// ---------------------------------------------------------------------------
// args
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Args {
    db: Option<String>,
    email: Option<String>,
    out: Option<String>,
    paper: Option<String>,
    limit: Option<usize>,
    force: bool,
    json: bool,
    check: bool,
    fetch_oa: bool,
    report: bool,
    help: bool,
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut a = Args::default();
        let mut it = argv;
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--db" => a.db = Some(take(&mut it, "--db")?),
                "--email" => a.email = Some(take(&mut it, "--email")?),
                "--out" => a.out = Some(take(&mut it, "--out")?),
                "--paper" => a.paper = Some(take(&mut it, "--paper")?),
                "--limit" => {
                    let v = take(&mut it, "--limit")?;
                    a.limit = Some(v.parse().map_err(|_| format!("--limit 需要数字，收到 {v}"))?);
                }
                "--check" => a.check = true,
                "--fetch-oa" => a.fetch_oa = true,
                "--report" => a.report = true,
                "--force" => a.force = true,
                "--json" => a.json = true,
                "--help" | "-h" => a.help = true,
                other => return Err(format!("未知参数: {other}（--help 查看用法）")),
            }
        }
        Ok(a)
    }
}

fn take(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} 缺少值"))
}

fn print_usage() {
    println!(
        "cortex-paper-meta — 论文库的撤稿校验与开放全文下载\n\n\
用法:\n  \
cortex-paper-meta --check            对每篇有 DOI 的论文查 Crossref + Unpaywall\n  \
cortex-paper-meta --fetch-oa         下载已发现的开放获取 PDF\n  \
cortex-paper-meta --report           打印当前已知结果\n\n\
选项:\n  \
--db <path>        指定 cortex.db（默认：应用数据目录）\n  \
--email <mail>     Unpaywall 要求的联系邮箱（默认 $UNPAYWALL_EMAIL，再退到 cortex@localhost）\n  \
--out <dir>        --fetch-oa 的输出目录（默认 ~/Downloads/cortex-oa）\n  \
--paper <id>       只处理这一篇论文\n  \
--limit <n>        --check 最多检查 n 篇\n  \
--force            忽略 30 天新鲜度缓存，强制重查\n  \
--json             机器可读输出\n  \
--help             显示本帮助\n\n\
提示:\n  \
- 先启动一次 Cortex 生成数据库；迁移时关闭应用更稳。\n  \
- 撤稿来自 Crossref 的 updated-by / is-retracted-by；开放全文来自 Unpaywall。\n  \
- 结果写入 paper_quality 表，不影响 papers 本身，离线也能继续用。"
    );
}

// ---------------------------------------------------------------------------
// db
// ---------------------------------------------------------------------------

fn resolve_db_path(args: &Args) -> PathBuf {
    if let Some(d) = &args.db {
        return PathBuf::from(d);
    }
    if let Ok(d) = std::env::var("CORTEX_DB") {
        if !d.trim().is_empty() {
            return PathBuf::from(d.trim());
        }
    }
    if let Ok(d) = std::env::var("CORTEX_DATA_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d.trim()).join("cortex.db");
        }
    }
    platform_default_db()
}

#[cfg(target_os = "windows")]
fn platform_default_db() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let base = std::env::var("APPDATA").unwrap_or_else(|_| format!("{home}\\AppData\\Roaming"));
    PathBuf::from(base).join("app.study.cortex").join("cortex.db")
}

#[cfg(target_os = "macos")]
fn platform_default_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("app.study.cortex")
        .join("cortex.db")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_default_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let base = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    PathBuf::from(base).join("app.study.cortex").join("cortex.db")
}

/// Idempotent: works whether the app has migrated yet or not, and upgrades a
/// table created by an older build of this tool (SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, so each column is checked first).
fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS paper_quality (
           paper_id        TEXT PRIMARY KEY,
           doi             TEXT,
           retracted       INTEGER NOT NULL DEFAULT 0,
           retraction_doi  TEXT,
           retraction_kind TEXT,
           is_notice       INTEGER NOT NULL DEFAULT 0,
           oa_status       TEXT,
           oa_pdf_url      TEXT,
           checked_at      INTEGER NOT NULL,
           error           TEXT,
           journal         TEXT,
           issn            TEXT,
           in_doaj         INTEGER,
           is_core         INTEGER,
           journal_type    TEXT,
           journal_quality TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_paper_quality_retracted
           ON paper_quality(retracted);",
    )
    .map_err(|e| format!("建表失败: {e}"))?;

    let have: std::collections::HashSet<String> = conn
        .prepare("PRAGMA table_info(paper_quality)")
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| e.to_string())?;
    for (name, ty) in [
        ("journal", "TEXT"),
        ("issn", "TEXT"),
        ("in_doaj", "INTEGER"),
        ("is_core", "INTEGER"),
        ("journal_type", "TEXT"),
        ("journal_quality", "TEXT"),
    ] {
        if !have.contains(name) {
            conn.execute_batch(&format!("ALTER TABLE paper_quality ADD COLUMN {name} {ty}"))
                .map_err(|e| format!("升级表结构失败（{name}）: {e}"))?;
        }
    }
    Ok(())
}

struct PaperRow {
    id: String,
    doi: String,
    label: String,
}

fn papers_with_doi(conn: &Connection, only: Option<&str>) -> Result<Vec<PaperRow>, String> {
    let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<PaperRow> {
        Ok(PaperRow {
            id: r.get(0)?,
            doi: r.get(1)?,
            label: r.get(2)?,
        })
    };
    let rows = match only {
        Some(id) => {
            let mut st = conn
                .prepare(
                    "SELECT id, COALESCE(doi, ''), COALESCE(NULLIF(TRIM(cite_key), ''), title)
                     FROM papers WHERE id = ?1",
                )
                .map_err(|e| e.to_string())?;
            let collected = st
                .query_map([id], map)
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            collected
        }
        None => {
            let mut st = conn
                .prepare(
                    "SELECT id, doi, COALESCE(NULLIF(TRIM(cite_key), ''), title)
                     FROM papers
                     WHERE doi IS NOT NULL AND TRIM(doi) <> ''
                     ORDER BY year DESC, id",
                )
                .map_err(|e| e.to_string())?;
            let collected = st
                .query_map([], map)
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            collected
        }
    };
    Ok(rows.into_iter().filter(|p| !normalize_doi(&p.doi).is_empty()).collect())
}

fn is_fresh(conn: &Connection, paper_id: &str) -> Result<bool, String> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT checked_at FROM paper_quality WHERE paper_id = ?1",
            [paper_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(match last {
        Some(ts) => now_ms() - ts < FRESH_DAYS * 24 * 3600 * 1000,
        None => false,
    })
}

#[derive(Default)]
struct Retraction {
    retracted: bool,
    retraction_doi: Option<String>,
    kind: Option<String>,
    is_notice: bool,
}

#[derive(Default)]
struct Oa {
    status: Option<String>,
    pdf_url: Option<String>,
}

fn upsert(
    conn: &Connection,
    paper_id: &str,
    doi: &str,
    ret: &Retraction,
    oa: &Oa,
    journal: &Journal,
    error: Option<&str>,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO paper_quality
           (paper_id, doi, retracted, retraction_doi, retraction_kind, is_notice,
            oa_status, oa_pdf_url, checked_at, error,
            journal, issn, in_doaj, is_core, journal_type, journal_quality)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(paper_id) DO UPDATE SET
           doi = excluded.doi,
           retracted = excluded.retracted,
           retraction_doi = excluded.retraction_doi,
           retraction_kind = excluded.retraction_kind,
           is_notice = excluded.is_notice,
           oa_status = excluded.oa_status,
           oa_pdf_url = excluded.oa_pdf_url,
           checked_at = excluded.checked_at,
           error = excluded.error,
           journal = excluded.journal,
           issn = excluded.issn,
           in_doaj = excluded.in_doaj,
           is_core = excluded.is_core,
           journal_type = excluded.journal_type,
           journal_quality = excluded.journal_quality",
        params![
            paper_id,
            doi,
            ret.retracted as i64,
            ret.retraction_doi,
            ret.kind,
            ret.is_notice as i64,
            oa.status,
            oa.pdf_url,
            now_ms(),
            error,
            journal.name,
            journal.issn,
            journal.in_doaj as i64,
            journal.is_core as i64,
            journal.jtype,
            journal.quality
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("写入失败: {e}"))
}

// ---------------------------------------------------------------------------
// network
// ---------------------------------------------------------------------------

fn build_client(email: &str) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(format!("cortex-paper-meta/1.0 (mailto:{email})"))
        .build()
        .map_err(|e| format!("HTTP 客户端初始化失败: {e}"))
}

fn crossref(client: &reqwest::blocking::Client, doi: &str) -> Result<Retraction, String> {
    let url = format!("{CROSSREF}{}", urlencode(doi));
    let resp = client.get(&url).send().map_err(|e| e.to_string())?;
    if resp.status().as_u16() == 404 {
        return Ok(Retraction::default());
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let v: Value = resp.json().map_err(|e| format!("解析失败: {e}"))?;
    Ok(parse_crossref(&v))
}

fn unpaywall(client: &reqwest::blocking::Client, doi: &str, email: &str) -> Result<Oa, String> {
    let url = format!("{UNPAYWALL}{}?email={}", urlencode(doi), urlencode(email));
    let resp = client.get(&url).send().map_err(|e| e.to_string())?;
    if resp.status().as_u16() == 404 {
        return Ok(Oa::default());
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let v: Value = resp.json().map_err(|e| format!("解析失败: {e}"))?;
    Ok(parse_unpaywall(&v))
}

fn parse_crossref(v: &Value) -> Retraction {
    let msg = v.get("message").unwrap_or(v);
    let mut out = Retraction::default();

    if msg.get("type").and_then(Value::as_str) == Some("retraction") {
        out.is_notice = true;
    }

    // 1. updated-by: this work was updated by a later notice.
    if let Some(updates) = msg.get("updated-by").and_then(Value::as_array) {
        for upd in updates {
            let kind = upd.get("type").and_then(Value::as_str).unwrap_or("");
            if kind.eq_ignore_ascii_case("retraction") {
                out.retracted = true;
                out.retraction_doi = upd
                    .get("DOI")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                out.kind = Some("retraction".to_string());
                break;
            }
            if out.kind.is_none() && !kind.is_empty() {
                out.kind = Some(kind.to_string());
            }
        }
    }

    // 2. relation["is-retracted-by"] — newer Crossref records.
    if !out.retracted {
        if let Some(rel) = msg.get("relation").and_then(Value::as_object) {
            if let Some(list) = rel.get("is-retracted-by").and_then(Value::as_array) {
                if let Some(first) = list.first() {
                    out.retracted = true;
                    out.retraction_doi = first.get("id").and_then(Value::as_str).map(str::to_string);
                    out.kind = Some("retraction".to_string());
                }
            }
        }
    }

    out
}

fn parse_unpaywall(v: &Value) -> Oa {
    let status = v
        .get("oa_status")
        .and_then(Value::as_str)
        .unwrap_or(if v.get("is_oa").and_then(Value::as_bool) == Some(true) {
            "open"
        } else {
            "closed"
        })
        .to_string();
    let best = v.get("best_oa_location");
    let pdf = best
        .and_then(|l| l.get("url_for_pdf"))
        .and_then(Value::as_str)
        .or_else(|| best.and_then(|l| l.get("url")).and_then(Value::as_str))
        .map(str::to_string);
    Oa {
        status: Some(status),
        pdf_url: pdf,
    }
}

/// Journal-level signals from OpenAlex. Deliberately advisory: DOAJ is a
/// positive whitelist, `core` is OpenAlex's curated collection, and `journal`
/// only means "a journal we could not confirm in either" — a new or niche
/// title looks identical to a dubious one, so it is never called predatory.
///
/// OpenAlex's nested source object is dehydrated: it has `is_in_doaj`,
/// `is_core` and `type`, but no `works_count` / `cited_by_count`.
struct Journal {
    name: Option<String>,
    issn: Option<String>,
    in_doaj: bool,
    is_core: bool,
    jtype: Option<String>,
    quality: String,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            name: None,
            issn: None,
            in_doaj: false,
            is_core: false,
            jtype: None,
            quality: "unknown".to_string(),
        }
    }
}

fn openalex(client: &reqwest::blocking::Client, doi: &str, email: &str) -> Result<Journal, String> {
    let url = format!(
        "https://api.openalex.org/works/doi:{doi}?mailto={}",
        urlencode(email)
    );
    let resp = client.get(&url).send().map_err(|e| e.to_string())?;
    if resp.status().as_u16() == 404 {
        return Ok(Journal::default());
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let v: Value = resp.json().map_err(|e| format!("解析失败: {e}"))?;
    Ok(parse_openalex(&v))
}

fn parse_openalex(v: &Value) -> Journal {
    let src = v.get("primary_location").and_then(|l| l.get("source"));
    let Some(src) = src.filter(|s| !s.is_null()) else {
        return Journal::default();
    };
    let name = src
        .get("display_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let issn = src
        .get("issn_l")
        .and_then(Value::as_str)
        .map(str::to_string);
    let in_doaj = src
        .get("is_in_doaj")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_core = src
        .get("is_core")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let jtype = src.get("type").and_then(Value::as_str).map(str::to_string);
    let quality = if in_doaj {
        "doaj"
    } else if is_core {
        "core"
    } else if jtype.as_deref() == Some("journal") {
        "journal"
    } else if name.is_some() {
        "other"
    } else {
        "unknown"
    };
    Journal {
        name,
        issn,
        in_doaj,
        is_core,
        jtype,
        quality: quality.to_string(),
    }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

fn check(conn: &Connection, args: &Args) -> Result<(), String> {
    let email = args
        .email
        .clone()
        .or_else(|| std::env::var("UNPAYWALL_EMAIL").ok())
        .unwrap_or_else(|| "cortex@localhost".to_string());
    let client = build_client(&email)?;
    let rows = papers_with_doi(conn, args.paper.as_deref())?;

    let mut checked = 0usize;
    let mut skipped = 0usize;
    let mut retracted: Vec<(String, String, Option<String>)> = Vec::new();
    let mut oa_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut journal_quality: BTreeMap<String, usize> = BTreeMap::new();
    let mut errors: Vec<(String, String)> = Vec::new();

    for row in &rows {
        if args.limit.is_some_and(|limit| checked >= limit) {
            break;
        }
        if !args.force && is_fresh(conn, &row.id)? {
            skipped += 1;
            continue;
        }
        let doi = normalize_doi(&row.doi);
        let mut err: Option<String> = None;

        let ret = match crossref(&client, &doi) {
            Ok(r) => r,
            Err(e) => {
                err = Some(format!("crossref: {e}"));
                Retraction::default()
            }
        };
        let oa = match unpaywall(&client, &doi, &email) {
            Ok(o) => o,
            Err(e) => {
                err = Some(match err {
                    Some(prev) => format!("{prev}; unpaywall: {e}"),
                    None => format!("unpaywall: {e}"),
                });
                Oa::default()
            }
        };

        let journal = match openalex(&client, &doi, &email) {
            Ok(j) => j,
            Err(e) => {
                err = Some(match err {
                    Some(prev) => format!("{prev}; openalex: {e}"),
                    None => format!("openalex: {e}"),
                });
                Journal::default()
            }
        };

        upsert(conn, &row.id, &doi, &ret, &oa, &journal, err.as_deref())?;
        checked += 1;
        if ret.retracted {
            retracted.push((row.label.clone(), doi.clone(), ret.retraction_doi.clone()));
        }
        if let Some(s) = &oa.status {
            *oa_counts.entry(s.clone()).or_insert(0) += 1;
        }
        *journal_quality.entry(journal.quality.clone()).or_insert(0) += 1;
        if let Some(e) = err {
            errors.push((row.label.clone(), e));
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    if args.json {
        let payload = serde_json::json!({
            "checked": checked,
            "skipped": skipped,
            "retracted": retracted.iter().map(|(label, doi, notice)| serde_json::json!({
                "paper": label, "doi": doi, "retraction_doi": notice
            })).collect::<Vec<_>>(),
            "oa": oa_counts,
            "journal_quality": journal_quality,
            "errors": errors.iter().map(|(label, e)| serde_json::json!({
                "paper": label, "error": e
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return Ok(());
    }

    println!(
        "检查完成：{checked} 篇（跳过 {skipped} 篇 30 天内已查，{} 个错误）",
        errors.len()
    );
    if !retracted.is_empty() {
        println!("\n⚠ 撤稿 {} 篇", retracted.len());
        for (label, doi, notice) in &retracted {
            println!("  - {label}  ({doi})");
            if let Some(n) = notice {
                println!("      撤稿声明: {n}");
            }
        }
    }
    let oa_total: usize = oa_counts
        .iter()
        .filter(|(k, _)| k.as_str() != "closed")
        .map(|(_, n)| n)
        .sum();
    if oa_total > 0 {
        let parts: Vec<String> = oa_counts
            .iter()
            .filter(|(k, _)| k.as_str() != "closed")
            .map(|(k, n)| format!("{k} {n}"))
            .collect();
        println!("\n开放获取 {oa_total} 篇（{}）", parts.join(" · "));
        println!("  用 --fetch-oa 下载 PDF");
    }
    let q_parts: Vec<String> = journal_quality
        .iter()
        .filter(|(k, _)| k.as_str() != "unknown")
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    if !q_parts.is_empty() {
        println!("\n期刊信号：{}", q_parts.join(" · "));
        println!("  doaj=DOAJ 收录 · core=OpenAlex 核心库 · journal=期刊但未进核心/DOAJ（非质量结论）");
    }
    if !errors.is_empty() {
        println!("\n查询失败 {} 篇（下次 --check 会重试）", errors.len());
        for (label, e) in errors.iter().take(10) {
            println!("  - {label}: {e}");
        }
    }
    Ok(())
}

fn fetch_oa(conn: &Connection, args: &Args) -> Result<(), String> {
    let out_dir = args
        .out
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(default_out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建输出目录失败: {e}"))?;

    let mut st = conn
        .prepare(
            "SELECT q.paper_id, q.oa_pdf_url,
                    COALESCE(NULLIF(TRIM(p.cite_key), ''), p.title)
             FROM paper_quality q
             JOIN papers p ON p.id = q.paper_id
             WHERE q.oa_pdf_url IS NOT NULL AND TRIM(q.oa_pdf_url) <> ''
               AND q.retracted = 0
             ORDER BY p.year DESC, p.id",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        println!("没有可下载的开放全文。先跑一次 --check。");
        return Ok(());
    }

    let client = build_client("cortex@localhost")?;
    let mut saved: Vec<(String, u64)> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();

    for (paper_id, url, label) in rows {
        let name = format!("{}.pdf", safe_name(&label));
        let dest = out_dir.join(&name);
        if dest.is_file() {
            println!("= 已存在 {name}");
            continue;
        }
        match download_pdf(&client, &url, &dest) {
            Ok(bytes) => {
                println!("✔ {name}（{} KB）", bytes / 1024);
                saved.push((name, bytes));
            }
            Err(e) => {
                let note = format!("download: {e}");
                failed.push((label.clone(), e));
                // A bad URL should not block the rest; drop the stale link.
                conn.execute(
                    "UPDATE paper_quality SET oa_pdf_url = NULL, error = ?2 WHERE paper_id = ?1",
                    params![paper_id, note],
                )
                .ok();
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    if args.json {
        let payload = serde_json::json!({
            "out": out_dir.display().to_string(),
            "saved": saved.iter().map(|(n, b)| serde_json::json!({"file": n, "bytes": b})).collect::<Vec<_>>(),
            "failed": failed.iter().map(|(l, e)| serde_json::json!({"paper": l, "error": e})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return Ok(());
    }
    println!(
        "\n下载完成：{} 个成功，{} 个失败 → {}",
        saved.len(),
        failed.len(),
        out_dir.display()
    );
    if !failed.is_empty() {
        for (label, e) in failed.iter().take(10) {
            println!("  - {label}: {e}");
        }
    }
    println!("把 PDF 拖进 Cortex（或对应学科的来源列表）即可入库。");
    Ok(())
}

struct QualityRow {
    id: String,
    label: String,
    doi: Option<String>,
    retracted: bool,
    notice: Option<String>,
    is_notice: bool,
    oa: Option<String>,
    checked: Option<i64>,
    error: Option<String>,
    journal: Option<String>,
    quality: Option<String>,
}

fn report(conn: &Connection, args: &Args) -> Result<(), String> {
    let mut st = conn
        .prepare(
            "SELECT p.id, COALESCE(NULLIF(TRIM(p.cite_key), ''), p.title), COALESCE(q.doi, p.doi),
                    q.retracted, q.retraction_doi, q.is_notice, q.oa_status, q.oa_pdf_url,
                    q.checked_at, q.error, q.journal, q.journal_quality
             FROM papers p LEFT JOIN paper_quality q ON q.paper_id = p.id
             ORDER BY q.retracted DESC, q.checked_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<QualityRow> = st
        .query_map([], |r| {
            Ok(QualityRow {
                id: r.get(0)?,
                label: r.get(1)?,
                doi: r.get(2)?,
                retracted: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                notice: r.get(4)?,
                is_notice: r.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
                oa: r.get(6)?,
                checked: r.get(8)?,
                error: r.get(9)?,
                journal: r.get(10)?,
                quality: r.get(11)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| e.to_string())?;

    if args.json {
        let items: Vec<Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": &r.id, "paper": &r.label, "doi": &r.doi,
                    "retracted": r.retracted,
                    "retraction_doi": &r.notice,
                    "is_notice": r.is_notice,
                    "oa_status": &r.oa,
                    "journal": &r.journal,
                    "journal_quality": &r.quality,
                    "checked_at": &r.checked, "error": &r.error
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
        return Ok(());
    }

    if rows.is_empty() {
        println!("论文库还是空的。");
        return Ok(());
    }
    let n_ret = rows.iter().filter(|r| r.retracted).count();
    let n_oa = rows
        .iter()
        .filter(|r| matches!(r.oa.as_deref(), Some(s) if s != "closed"))
        .count();
    let n_checked = rows.iter().filter(|r| r.checked.is_some()).count();
    println!(
        "论文 {} 篇 · 已检查 {n_checked} · 撤稿 {n_ret} · 开放全文 {n_oa}\n",
        rows.len()
    );
    for r in &rows {
        let mut flags: Vec<&str> = Vec::new();
        if r.retracted {
            flags.push("⚠撤稿");
        }
        if r.is_notice {
            flags.push("撤稿声明");
        }
        if matches!(r.oa.as_deref(), Some(s) if s != "closed") {
            flags.push("OA");
        }
        if r.quality.as_deref() == Some("doaj") {
            flags.push("DOAJ");
        }
        if r.quality.as_deref() == Some("journal") {
            flags.push("期刊未核心收录");
        }
        let suffix = match (flags.is_empty(), r.error.as_deref()) {
            (true, Some(e)) => format!("  [error: {e}]"),
            (true, None) => String::new(),
            (false, Some(e)) => format!("  [{}]  [error: {e}]", flags.join(" ")),
            (false, None) => format!("  [{}]", flags.join(" ")),
        };
        let doi_s = r.doi.clone().unwrap_or_default();
        let notice_s = r
            .notice
            .as_ref()
            .map(|n| format!(" ← {n}"))
            .unwrap_or_default();
        println!("- {}  ({doi_s}){notice_s}{suffix}", r.label);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn default_out_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join("Downloads").join("cortex-oa")
}

fn download_pdf(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
) -> Result<u64, String> {
    let resp = client.get(url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| e.to_string())?;
    if !bytes.starts_with(b"%PDF") {
        return Err("不是 PDF（可能是落地页）".to_string());
    }
    std::fs::write(dest, &bytes).map_err(|e| e.to_string())?;
    Ok(bytes.len() as u64)
}

fn safe_name(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if out.is_empty() {
        out.push_str("paper");
    }
    out
}

fn normalize_doi(raw: &str) -> String {
    let mut s = raw.trim();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ] {
        if let Some(head) = s.get(..prefix.len()) {
            if head.eq_ignore_ascii_case(prefix) {
                s = &s[prefix.len()..];
                break;
            }
        }
    }
    s.trim().trim_end_matches('.').trim().to_string()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn run(args: &Args) -> Result<(), String> {
    let db_path = resolve_db_path(args);
    if !db_path.is_file() {
        return Err(format!(
            "找不到数据库: {}\n  先启动一次 Cortex，或用 --db 指定 cortex.db。",
            db_path.display()
        ));
    }
    let conn = Connection::open(&db_path).map_err(|e| format!("打开数据库失败: {e}"))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("设置 busy_timeout 失败: {e}"))?;
    ensure_schema(&conn)?;

    if args.check {
        check(&conn, args)?;
    }
    if args.fetch_oa {
        fetch_oa(&conn, args)?;
    }
    if args.report {
        report(&conn, args)?;
    }
    Ok(())
}

fn main() {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("✖ {e}");
            std::process::exit(2);
        }
    };
    if args.help || (!args.check && !args.fetch_oa && !args.report) {
        print_usage();
        return;
    }
    if let Err(e) = run(&args) {
        eprintln!("✖ {e}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// tests — pure parsing only (no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn updated_by_retraction_marks_paper() {
        let v = json!({
            "message": {
                "type": "journal-article",
                "updated-by": [
                    { "type": "retraction", "DOI": "10.1000/notice" }
                ]
            }
        });
        let r = parse_crossref(&v);
        assert!(r.retracted);
        assert_eq!(r.retraction_doi.as_deref(), Some("10.1000/notice"));
        assert_eq!(r.kind.as_deref(), Some("retraction"));
        assert!(!r.is_notice);
    }

    #[test]
    fn relation_is_retracted_by_marks_paper() {
        let v = json!({
            "message": {
                "type": "journal-article",
                "relation": { "is-retracted-by": [ { "id": "10.1000/r" } ] }
            }
        });
        let r = parse_crossref(&v);
        assert!(r.retracted);
        assert_eq!(r.retraction_doi.as_deref(), Some("10.1000/r"));
    }

    #[test]
    fn a_notice_is_flagged_but_not_retracted() {
        let v = json!({ "message": { "type": "retraction" } });
        let r = parse_crossref(&v);
        assert!(r.is_notice);
        assert!(!r.retracted);
    }

    #[test]
    fn correction_only_is_not_a_retraction() {
        let v = json!({
            "message": {
                "updated-by": [ { "type": "correction", "DOI": "10.1000/c" } ]
            }
        });
        let r = parse_crossref(&v);
        assert!(!r.retracted);
        assert_eq!(r.kind.as_deref(), Some("correction"));
    }

    #[test]
    fn unpaywall_prefers_pdf_url() {
        let v = json!({
            "is_oa": true,
            "oa_status": "green",
            "best_oa_location": {
                "url": "https://repo.example/paper",
                "url_for_pdf": "https://repo.example/paper.pdf"
            }
        });
        let o = parse_unpaywall(&v);
        assert_eq!(o.status.as_deref(), Some("green"));
        assert_eq!(o.pdf_url.as_deref(), Some("https://repo.example/paper.pdf"));
    }

    #[test]
    fn unpaywall_closed_has_no_pdf() {
        let v = json!({ "is_oa": false, "oa_status": "closed" });
        let o = parse_unpaywall(&v);
        assert_eq!(o.status.as_deref(), Some("closed"));
        assert!(o.pdf_url.is_none());
    }

    #[test]
    fn doi_prefixes_and_case_are_stripped() {
        assert_eq!(normalize_doi("https://doi.org/10.1/ABC"), "10.1/ABC");
        assert_eq!(normalize_doi("  DOI:10.1/x. "), "10.1/x");
        assert_eq!(normalize_doi("10.1/y"), "10.1/y");
    }

    #[test]
    fn urlencode_escapes_slashes_but_keeps_unreserved() {
        assert_eq!(urlencode("10.1/ab-c_d.e~f"), "10.1%2Fab-c_d.e~f");
    }

    #[test]
    fn safe_name_never_escapes_the_output_dir() {
        assert_eq!(safe_name("../../etc/passwd"), "______etc_passwd");
    }

    #[test]
    fn doaj_source_is_marked_doaj() {
        let v = json!({ "primary_location": { "source": {
            "display_name": "PLOS ONE", "issn_l": "1932-6203",
            "is_in_doaj": true, "is_core": true, "type": "journal"
        }}});
        let j = parse_openalex(&v);
        assert_eq!(j.quality, "doaj");
        assert_eq!(j.name.as_deref(), Some("PLOS ONE"));
        assert_eq!(j.issn.as_deref(), Some("1932-6203"));
        assert!(j.in_doaj);
    }

    #[test]
    fn core_journal_is_core_not_doaj() {
        // The real shape for 10.1038/nature14539.
        let v = json!({ "primary_location": { "source": {
            "display_name": "Nature", "issn_l": "0028-0836",
            "is_in_doaj": false, "is_core": true, "type": "journal",
            "host_organization_name": "Nature Portfolio"
        }}});
        let j = parse_openalex(&v);
        assert_eq!(j.quality, "core");
        assert!(j.is_core);
    }

    #[test]
    fn non_core_journal_is_flagged_but_never_called_predatory() {
        let v = json!({ "primary_location": { "source": {
            "display_name": "Some New Journal", "is_in_doaj": false,
            "is_core": false, "type": "journal"
        }}});
        assert_eq!(parse_openalex(&v).quality, "journal");
    }

    #[test]
    fn conference_is_other_not_a_journal_warning() {
        let v = json!({ "primary_location": { "source": {
            "display_name": "NeurIPS", "is_in_doaj": false,
            "is_core": false, "type": "conference"
        }}});
        assert_eq!(parse_openalex(&v).quality, "other");
    }

    #[test]
    fn missing_source_is_unknown() {
        assert_eq!(
            parse_openalex(&json!({ "primary_location": { "source": null } })).quality,
            "unknown"
        );
        assert_eq!(parse_openalex(&json!({})).quality, "unknown");
    }
}
