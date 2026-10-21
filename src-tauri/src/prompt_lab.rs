//! Prompt lab: run a prompt, compare two versions of it, and score them
//! against fixtures you can re-run.
//!
//! The point is not to admire output. It is to make a prompt change falsifiable:
//! a fixture states what a good answer must contain, the rubric is deterministic
//! (no LLM grading its own homework), and every comparison is stored so the next
//! edit starts from evidence instead of vibes.

use rusqlite::Connection;
use serde::Serialize;

use crate::db::{new_id, now_ms};
use crate::error::{Error, Result};
use crate::llm::Llm;

/// Outputs are persisted for inspection, not for archive: a runaway generation
/// should not grow the database by megabytes.
const STORE_CHARS: usize = 6000;

// ---------------------------------------------------------------- rules

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    Contains(String),
    Absent(String),
    Json,
    MinChars(usize),
    MaxChars(usize),
    Regex(String),
}

/// Parse one rule per line. Blank lines and `#` comments are ignored so a
/// fixture's rubric can be annotated.
pub fn parse_rules(text: &str) -> Result<Vec<Rule>> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let n = i + 1;
        let (head, tail) = match line.split_once(':') {
            Some((h, t)) => (h.trim().to_ascii_lowercase(), t.trim().to_string()),
            None => (line.to_ascii_lowercase(), String::new()),
        };
        let rule = match head.as_str() {
            "contains" | "包含" => {
                if tail.is_empty() {
                    return Err(Error::Other(format!("第 {n} 行：contains 需要一段文字")));
                }
                Rule::Contains(tail)
            }
            "absent" | "不含" => {
                if tail.is_empty() {
                    return Err(Error::Other(format!("第 {n} 行：absent 需要一段文字")));
                }
                Rule::Absent(tail)
            }
            "json" => Rule::Json,
            "minchars" => Rule::MinChars(parse_count(n, &tail, "minchars")?),
            "maxchars" => Rule::MaxChars(parse_count(n, &tail, "maxchars")?),
            "regex" => {
                if tail.is_empty() {
                    return Err(Error::Other(format!("第 {n} 行：regex 需要一个正则")));
                }
                // Compile now, not per run: a typo should be rejected while you
                // are writing the fixture, not discovered halfway through a run.
                regex::Regex::new(&tail)
                    .map_err(|e| Error::Other(format!("第 {n} 行：正则无效（{e}）")))?;
                Rule::Regex(tail)
            }
            other => {
                return Err(Error::Other(format!(
                    "第 {n} 行：不认识的规则「{other}」（可用 contains / absent / json / minchars / maxchars / regex）"
                )))
            }
        };
        out.push(rule);
    }
    Ok(out)
}

fn parse_count(line: usize, tail: &str, name: &str) -> Result<usize> {
    tail.parse::<usize>()
        .map_err(|_| Error::Other(format!("第 {line} 行：{name} 需要一个整数，拿到「{tail}」")))
}

/// Every failing rule, as a human-readable reason. Empty means the output
/// passes — rules are all-must-hold, which is what makes a score comparable.
pub fn check(output: &str, rules: &[Rule]) -> Vec<String> {
    let lower = output.to_lowercase();
    let mut fails = Vec::new();
    for r in rules {
        match r {
            Rule::Contains(s) => {
                if !lower.contains(&s.to_lowercase()) {
                    fails.push(format!("缺少「{s}」"));
                }
            }
            Rule::Absent(s) => {
                if lower.contains(&s.to_lowercase()) {
                    fails.push(format!("不该出现「{s}」"));
                }
            }
            Rule::Json => {
                if crate::llm::extract_json(output).is_err() {
                    fails.push("不是合法 JSON".to_string());
                }
            }
            Rule::MinChars(n) => {
                let got = output.chars().count();
                if got < *n {
                    fails.push(format!("太短（{got} < {n} 字）"));
                }
            }
            Rule::MaxChars(n) => {
                let got = output.chars().count();
                if got > *n {
                    fails.push(format!("太长（{got} > {n} 字）"));
                }
            }
            Rule::Regex(p) => match regex::Regex::new(p) {
                Ok(re) => {
                    if !re.is_match(output) {
                        fails.push(format!("不匹配 /{p}/"));
                    }
                }
                Err(e) => fails.push(format!("正则无效（{e}）")),
            },
        }
    }
    fails
}

// ---------------------------------------------------------------- fixtures

#[derive(Debug, Clone, Serialize)]
pub struct PromptEval {
    pub id: String,
    pub prompt_key: String,
    pub name: String,
    pub input: String,
    pub rules: String,
    pub created_at: i64,
}

fn row_eval(r: &rusqlite::Row<'_>) -> rusqlite::Result<PromptEval> {
    Ok(PromptEval {
        id: r.get(0)?,
        prompt_key: r.get(1)?,
        name: r.get(2)?,
        input: r.get(3)?,
        rules: r.get(4)?,
        created_at: r.get(5)?,
    })
}

pub fn list_evals(conn: &Connection, key: &str) -> Result<Vec<PromptEval>> {
    let mut st = conn.prepare(
        "SELECT id, prompt_key, name, input, rules, created_at FROM prompt_evals \
         WHERE prompt_key = ?1 ORDER BY created_at, name",
    )?;
    let rows = st.query_map([key], row_eval)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn add_eval(
    conn: &Connection,
    key: &str,
    name: &str,
    input: &str,
    rules: &str,
) -> Result<PromptEval> {
    if crate::prompts::spec(key).is_none() {
        return Err(Error::Other(format!("unknown prompt key: {key}")));
    }
    if name.trim().is_empty() {
        return Err(Error::Other("给这条测试起个名字".into()));
    }
    if input.trim().is_empty() {
        return Err(Error::Other("测试输入不能为空".into()));
    }
    let parsed = parse_rules(rules)?;
    if parsed.is_empty() {
        return Err(Error::Other(
            "至少要有一条判分规则，否则任何输出都算通过".into(),
        ));
    }
    let id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO prompt_evals (id, prompt_key, name, input, rules, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6)",
        rusqlite::params![id, key, name.trim(), input.trim(), rules.trim(), now],
    )?;
    Ok(PromptEval {
        id,
        prompt_key: key.to_string(),
        name: name.trim().to_string(),
        input: input.trim().to_string(),
        rules: rules.trim().to_string(),
        created_at: now,
    })
}

pub fn delete_eval(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM prompt_evals WHERE id = ?1", [id])?;
    Ok(())
}

// ---------------------------------------------------------------- runs

#[derive(Debug, Clone, Serialize)]
pub struct CaseResult {
    pub output: String,
    pub passed: bool,
    pub failures: Vec<String>,
    pub ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalCaseRow {
    pub eval_id: String,
    pub eval_name: String,
    pub a: CaseResult,
    pub b: CaseResult,
}

#[derive(Debug, Clone, Serialize)]
pub struct VariantScore {
    pub label: String,
    pub passed: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalOutcome {
    pub run_id: String,
    pub a: VariantScore,
    pub b: VariantScore,
    pub rows: Vec<EvalCaseRow>,
}

fn one_case(model: &dyn Llm, body: &str, input: &str, rules: &[Rule]) -> CaseResult {
    let started = std::time::Instant::now();
    match model.complete(body, input) {
        Ok(out) => {
            let failures = check(&out, rules);
            let ms = started.elapsed().as_millis() as i64;
            CaseResult {
                output: clip(&out),
                passed: failures.is_empty(),
                failures,
                ms,
            }
        }
        Err(e) => CaseResult {
            // A model error is a failed case, not a failed run: the other
            // fixtures still tell you something.
            output: String::new(),
            passed: false,
            failures: vec![format!("模型调用失败：{e}")],
            ms: started.elapsed().as_millis() as i64,
        },
    }
}

fn clip(s: &str) -> String {
    s.chars().take(STORE_CHARS).collect()
}

/// Run every fixture for one prompt against two bodies (A = the version under
/// test, B = the baseline) and persist the comparison.
#[allow(clippy::too_many_arguments)]
pub fn run_evals(
    conn: &Connection,
    model: &dyn Llm,
    key: &str,
    body_a: &str,
    body_b: &str,
    label_a: &str,
    label_b: &str,
    ids: &[String],
) -> Result<EvalOutcome> {
    let all = list_evals(conn, key)?;
    let cases: Vec<PromptEval> = if ids.is_empty() {
        all
    } else {
        all.into_iter().filter(|e| ids.iter().any(|i| i == &e.id)).collect()
    };
    if cases.is_empty() {
        return Err(Error::Other(
            "这个提示词还没有测试用例 —— 先在「评测」里加一条".into(),
        ));
    }

    let run_id = new_id();
    let now = now_ms();
    conn.execute(
        "INSERT INTO prompt_eval_runs (id, prompt_key, label_a, label_b, created_at) \
         VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![run_id, key, label_a, label_b, now],
    )?;

    let mut rows = Vec::with_capacity(cases.len());
    let (mut pa, mut pb) = (0usize, 0usize);
    for case in &cases {
        let rules = parse_rules(&case.rules).unwrap_or_default();
        let a = one_case(model, body_a, &case.input, &rules);
        let b = one_case(model, body_b, &case.input, &rules);
        if a.passed {
            pa += 1;
        }
        if b.passed {
            pb += 1;
        }
        for (variant, res) in [("a", &a), ("b", &b)] {
            conn.execute(
                "INSERT INTO prompt_eval_results \
                 (id, run_id, eval_id, eval_name, variant, output, passed, failures, ms, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![
                    new_id(),
                    run_id,
                    case.id,
                    case.name,
                    variant,
                    res.output,
                    res.passed as i64,
                    res.failures.join("；"),
                    res.ms,
                    now
                ],
            )?;
        }
        rows.push(EvalCaseRow {
            eval_id: case.id.clone(),
            eval_name: case.name.clone(),
            a,
            b,
        });
    }

    Ok(EvalOutcome {
        run_id,
        a: VariantScore { label: label_a.to_string(), passed: pa, total: cases.len() },
        b: VariantScore { label: label_b.to_string(), passed: pb, total: cases.len() },
        rows,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalRunSummary {
    pub run_id: String,
    pub label_a: String,
    pub label_b: String,
    /// [passed, total] per variant.
    pub a: [i64; 2],
    pub b: [i64; 2],
    pub created_at: i64,
}

/// Past comparisons for one prompt, newest first. Without this the run tables
/// would be write-only — and "it scored better last week" would be unknowable.
pub fn history(conn: &Connection, key: &str, limit: usize) -> Result<Vec<EvalRunSummary>> {
    let mut st = conn.prepare(
        "SELECT r.id, r.label_a, r.label_b, r.created_at, \
         SUM(CASE WHEN res.variant = 'a' AND res.passed = 1 THEN 1 ELSE 0 END), \
         SUM(CASE WHEN res.variant = 'a' THEN 1 ELSE 0 END), \
         SUM(CASE WHEN res.variant = 'b' AND res.passed = 1 THEN 1 ELSE 0 END), \
         SUM(CASE WHEN res.variant = 'b' THEN 1 ELSE 0 END) \
         FROM prompt_eval_runs r LEFT JOIN prompt_eval_results res ON res.run_id = r.id \
         WHERE r.prompt_key = ?1 GROUP BY r.id ORDER BY r.created_at DESC, r.id DESC LIMIT ?2",
    )?;
    let rows = st.query_map(rusqlite::params![key, limit as i64], |r| {
        Ok(EvalRunSummary {
            run_id: r.get(0)?,
            label_a: r.get(1)?,
            label_b: r.get(2)?,
            created_at: r.get(3)?,
            a: [r.get::<_, Option<i64>>(4)?.unwrap_or(0), r.get(5)?],
            b: [r.get::<_, Option<i64>>(6)?.unwrap_or(0), r.get(7)?],
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One ad-hoc completion — the A/B bench, with no fixture and no persistence
/// beyond token accounting.
#[derive(Debug, Clone, Serialize)]
pub struct PromptRun {
    pub text: String,
    pub ms: i64,
    pub chars: usize,
    pub model: String,
}

pub fn run_once(model: &dyn Llm, body: &str, input: &str) -> Result<PromptRun> {
    if body.trim().is_empty() {
        return Err(Error::Other("提示词是空的".into()));
    }
    if input.trim().is_empty() {
        return Err(Error::Other("没有输入，模型无从下手".into()));
    }
    let started = std::time::Instant::now();
    let text = model.complete(body, input)?;
    Ok(PromptRun {
        chars: text.chars().count(),
        text,
        ms: started.elapsed().as_millis() as i64,
        model: model.name(),
    })
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn state() -> crate::db::AppState {
        crate::db::AppState::in_memory().unwrap()
    }

    /// Replies in order; each call also reports which system prompt it saw, so
    /// a test can prove A and B were actually different bodies.
    struct Scripted {
        replies: Mutex<Vec<Result<String>>>,
        seen: Mutex<Vec<String>>,
        at: AtomicUsize,
    }

    impl Scripted {
        fn ok(replies: &[&str]) -> Self {
            Self {
                replies: Mutex::new(replies.iter().map(|r| Ok((*r).to_string())).collect()),
                seen: Mutex::new(Vec::new()),
                at: AtomicUsize::new(0),
            }
        }
    }

    impl Llm for Scripted {
        fn complete(&self, system: &str, _user: &str) -> Result<String> {
            self.seen.lock().unwrap().push(system.to_string());
            let i = self.at.fetch_add(1, Ordering::SeqCst);
            let r = self.replies.lock().unwrap();
            match r.get(i) {
                Some(Ok(s)) => Ok(s.clone()),
                Some(Err(e)) => Err(Error::Other(e.to_string())),
                None => Ok("（没有更多回复）".into()),
            }
        }
        fn name(&self) -> String {
            "scripted".into()
        }
    }

    #[test]
    fn rules_are_parsed_one_per_line_and_comments_are_ignored() {
        let rules = parse_rules("# 判分\ncontains: ANSWER\n\nabsent: 抱歉\njson\nminchars: 10\nmaxchars: 900\nregex: ^\\{").unwrap();
        assert_eq!(
            rules,
            vec![
                Rule::Contains("ANSWER".into()),
                Rule::Absent("抱歉".into()),
                Rule::Json,
                Rule::MinChars(10),
                Rule::MaxChars(900),
                Rule::Regex("^\\{".into()),
            ]
        );
    }

    #[test]
    fn a_typo_in_a_rule_is_rejected_with_its_line_number() {
        let e = parse_rules("contains: a\ncontainz: b").unwrap_err().to_string();
        assert!(e.contains("第 2 行"), "{e}");
        let e = parse_rules("minchars: many").unwrap_err().to_string();
        assert!(e.contains("第 1 行"), "{e}");
        let e = parse_rules("regex: [unclosed").unwrap_err().to_string();
        assert!(e.contains("正则无效"), "{e}");
    }

    #[test]
    fn check_reports_every_failure_not_just_the_first() {
        let rules = parse_rules("contains: 结论\nabsent: 抱歉\nminchars: 50").unwrap();
        let fails = check("抱歉，我不知道。", &rules);
        assert_eq!(fails.len(), 3, "{fails:?}");
        assert!(fails[0].contains("结论"));
    }

    #[test]
    fn json_rule_accepts_a_fenced_object() {
        let rules = parse_rules("json").unwrap();
        assert!(check("```json\n{\"a\":1}\n```", &rules).is_empty());
        assert_eq!(check("随便一句话", &rules), vec!["不是合法 JSON".to_string()]);
    }

    #[test]
    fn contains_is_case_insensitive_and_length_counts_characters() {
        let rules = parse_rules("contains: answer").unwrap();
        assert!(check("ANSWER: 42", &rules).is_empty());
        // Six Chinese characters, not eighteen bytes.
        let rules = parse_rules("minchars: 6").unwrap();
        assert!(check("六个汉字而已", &rules).is_empty());
    }

    #[test]
    fn a_fixture_needs_a_name_an_input_and_at_least_one_rule() {
        let st = state();
        let c = st.db.lock().unwrap();
        assert!(add_eval(&c, crate::prompts::LIT_PLAN, "", "q", "contains: x").is_err());
        assert!(add_eval(&c, crate::prompts::LIT_PLAN, "n", "  ", "contains: x").is_err());
        assert!(add_eval(&c, crate::prompts::LIT_PLAN, "n", "q", "\n# nothing\n").is_err());
        assert!(add_eval(&c, "no.such.key", "n", "q", "contains: x").is_err());
        let ok = add_eval(&c, crate::prompts::LIT_PLAN, "角度覆盖", "spaced repetition", "contains: [").unwrap();
        assert_eq!(ok.prompt_key, crate::prompts::LIT_PLAN);
        assert_eq!(list_evals(&c, crate::prompts::LIT_PLAN).unwrap().len(), 1);
        delete_eval(&c, &ok.id).unwrap();
        assert!(list_evals(&c, crate::prompts::LIT_PLAN).unwrap().is_empty());
    }

    #[test]
    fn a_run_compares_both_bodies_on_every_fixture_and_persists_the_result() {
        let st = state();
        let c = st.db.lock().unwrap();
        // Names sort ASCII-first so the fixture order (and therefore the reply
        // order below) is deterministic without relying on timestamps.
        add_eval(&c, crate::prompts::LIT_PLAN, "case1 有结论", "q1", "contains: 三").unwrap();
        add_eval(&c, crate::prompts::LIT_PLAN, "case2 是 JSON", "q2", "json").unwrap();

        // A answers both correctly; B misses the first and answers the second
        // in prose. Two fixtures × two variants = four calls, in order.
        let model = Scripted::ok(&[
            // Per fixture: A then B — A answers both correctly, B misses both.
            "三大方向：检索、重排、对话",
            "我觉得有几个方向",
            "[\"a\",\"b\"]",
            "大概是 a 和 b 吧",
        ]);
        let out = run_evals(
            &c,
            &model,
            crate::prompts::LIT_PLAN,
            "body A",
            "body B",
            "当前",
            "默认",
            &[],
        )
        .unwrap();

        assert_eq!(list_evals(&c, crate::prompts::LIT_PLAN).unwrap().len(), 2, "fixtures persisted");
        assert_eq!(out.rows.len(), 2, "one row per fixture");
        assert_eq!(out.a.passed, 2, "A should answer both fixtures");
        assert_eq!(out.b.passed, 0);
        assert!(out.rows[0].a.passed && !out.rows[0].b.passed);
        assert!(out.rows[0].b.failures[0].contains("三"));

        // The bodies really were different — this is the whole comparison.
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen[0], "body A");
        assert_eq!(seen[1], "body B");

        let stored: i64 = c
            .query_row("SELECT COUNT(*) FROM prompt_eval_results WHERE run_id = ?1", [&out.run_id], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 4, "every case×variant is recorded");
    }

    #[test]
    fn a_model_error_fails_that_case_only() {
        let st = state();
        let c = st.db.lock().unwrap();
        add_eval(&c, crate::prompts::SUMMARY_PART, "非空", "q", "minchars: 2").unwrap();
        let model = Scripted {
            replies: Mutex::new(vec![Err(Error::Other("boom".into())), Ok("还行".into())]),
            seen: Mutex::new(Vec::new()),
            at: AtomicUsize::new(0),
        };
        let out = run_evals(&c, &model, crate::prompts::SUMMARY_PART, "a", "b", "A", "B", &[]).unwrap();
        assert!(!out.rows[0].a.passed);
        assert!(out.rows[0].a.failures[0].contains("boom"));
        assert!(out.rows[0].b.passed);
        assert_eq!(out.b.passed, 1);
    }

    #[test]
    fn history_reports_the_last_comparison_with_its_counts() {
        let st = state();
        let c = st.db.lock().unwrap();
        add_eval(&c, crate::prompts::LIT_PLAN, "case1", "q", "contains: 三").unwrap();
        assert!(history(&c, crate::prompts::LIT_PLAN, 8).unwrap().is_empty());

        let model = Scripted::ok(&["三个方向", "几个方向"]);
        run_evals(&c, &model, crate::prompts::LIT_PLAN, "a", "b", "草稿", "默认", &[]).unwrap();

        let h = history(&c, crate::prompts::LIT_PLAN, 8).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].a, [1, 1]);
        assert_eq!(h[0].b, [0, 1]);
        assert_eq!(h[0].label_a, "草稿");
        // Another prompt's history stays separate.
        assert!(history(&c, crate::prompts::SUMMARY_PART, 8).unwrap().is_empty());
    }

    #[test]
    fn running_without_fixtures_says_so_instead_of_scoring_zero() {
        let st = state();
        let c = st.db.lock().unwrap();
        let model = Scripted::ok(&["x"]);
        let e = run_evals(&c, &model, crate::prompts::LIT_PLAN, "a", "b", "A", "B", &[]).unwrap_err().to_string();
        assert!(e.contains("测试用例"), "{e}");
    }

    #[test]
    fn run_once_refuses_an_empty_prompt_or_input() {
        let model = Scripted::ok(&["ok"]);
        assert!(run_once(&model, "  ", "q").is_err());
        assert!(run_once(&model, "body", " ").is_err());
        let out = run_once(&model, "body", "q").unwrap();
        assert_eq!(out.text, "ok");
        assert_eq!(out.chars, 2);
        assert_eq!(out.model, "scripted");
    }
}

// ---------------------------------------------------------------- commands

use tauri::{AppHandle, Manager, State};

use crate::db::AppState;

#[tauri::command]
pub fn prompt_list() -> Result<Vec<crate::prompts::PromptInfo>> {
    Ok(crate::prompts::list())
}

#[tauri::command]
pub fn prompt_save(state: State<AppState>, key: String, body: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    crate::prompts::save(&c, &key, &body)
}

#[tauri::command]
pub fn prompt_reset(state: State<AppState>, key: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    crate::prompts::reset(&c, &key)
}

/// One completion with a draft body — the A/B bench. The body is a parameter
/// (not read from the registry) so an unsaved edit can be tried before it is
/// saved, which is the whole point of a lab.
#[tauri::command]
pub async fn prompt_run(
    app: AppHandle,
    key: String,
    body: Option<String>,
    input: String,
) -> Result<PromptRun> {
    tauri::async_runtime::spawn_blocking(move || -> Result<PromptRun> {
        let state = app.state::<AppState>();
        let (model, db_path) = {
            let c = state.db.lock().unwrap();
            let keys = crate::commands::read_keys(&c)?;
            let mut m = crate::commands::load_model(&c, &keys, "chat")?;
            crate::commands::apply_budget(&mut m, &c, "chat");
            let path = state
                .data_dir
                .as_ref()
                .map(|d| d.join("cortex.db"))
                .ok_or_else(|| Error::Other("内存模式没有实验室数据库".into()))?;
            (m, path)
        };
        let body = match body.map(|b| b.trim().to_string()) {
            Some(b) if !b.is_empty() => b,
            _ => crate::prompts::text(&key),
        };
        let out = run_once(model.as_ref(), &body, &input)?;
        let conn = Connection::open(&db_path)?;
        crate::tokens::record(
            &conn,
            "lab",
            &model.name(),
            &format!("{body}\n{input}"),
            &out.text,
            None,
        );
        Ok(out)
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[tauri::command]
pub fn prompt_evals(state: State<AppState>, key: String) -> Result<Vec<PromptEval>> {
    let c = state.db.lock().unwrap();
    list_evals(&c, &key)
}

#[tauri::command]
pub fn prompt_eval_add(
    state: State<AppState>,
    key: String,
    name: String,
    input: String,
    rules: String,
) -> Result<PromptEval> {
    let c = state.db.lock().unwrap();
    add_eval(&c, &key, &name, &input, &rules)
}

#[tauri::command]
pub fn prompt_eval_delete(state: State<AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    delete_eval(&c, &id)
}

#[tauri::command]
pub fn prompt_eval_history(state: State<AppState>, key: String, limit: Option<usize>) -> Result<Vec<EvalRunSummary>> {
    let c = state.db.lock().unwrap();
    history(&c, &key, limit.unwrap_or(8).clamp(1, 50))
}

/// Score A against B across every fixture of one prompt.
#[tauri::command]
pub async fn prompt_eval_run(
    app: AppHandle,
    key: String,
    body_a: Option<String>,
    body_b: Option<String>,
) -> Result<EvalOutcome> {
    tauri::async_runtime::spawn_blocking(move || -> Result<EvalOutcome> {
        let state = app.state::<AppState>();
        let (model, db_path) = {
            let c = state.db.lock().unwrap();
            let keys = crate::commands::read_keys(&c)?;
            let mut m = crate::commands::load_model(&c, &keys, "chat")?;
            crate::commands::apply_budget(&mut m, &c, "chat");
            let path = state
                .data_dir
                .as_ref()
                .map(|d| d.join("cortex.db"))
                .ok_or_else(|| Error::Other("内存模式没有实验室数据库".into()))?;
            (m, path)
        };
        let spec = crate::prompts::spec(&key).ok_or_else(|| Error::Other(format!("unknown prompt key: {key}")))?;
        let label_a = if body_a.as_deref().map(|b| b.trim()).unwrap_or("").is_empty() {
            "当前".to_string()
        } else {
            "草稿".to_string()
        };
        let a = match body_a.map(|b| b.trim().to_string()) {
            Some(b) if !b.is_empty() => b,
            _ => crate::prompts::text(&key),
        };
        let b = match body_b.map(|b| b.trim().to_string()) {
            Some(b) if !b.is_empty() => b,
            _ => spec.default.to_string(),
        };
        let conn = Connection::open(&db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        run_evals(&conn, model.as_ref(), &key, &a, &b, &label_a, "默认", &[])
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}
