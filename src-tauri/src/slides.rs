//! Slides two-round confirm workflow (roadmap 2.7, NotebookLM-Lite shape).
//!
//! `generate_material("slideshow")` is one-shot: a single LLM call produces the
//! whole deck and nothing can be corrected before it lands in the library.
//! This module splits that into two rounds with a confirm in between:
//!
//!   1. `slides_outline`   — round 1: ONE call returns an editable outline
//!                           ({title, focus} per slide); nothing is saved.
//!   2. (frontend)         — the user edits/adds/removes/reorders the plan.
//!   3. `render_slides`    — round 2: ONE focused call PER SLIDE (the plan plus
//!                           the full outline for flow), emitting
//!                           `slides:progress {idx,total,title}` per page, then
//!                           saves a normal `slideshow` material — same payload
//!                           shape {"slides":[{title,bullets,notes}]}, so PPTX
//!                           export and video-overview synthesis keep working.
//!   4. `regenerate_slide` — post-render: redo ONE slide (deck-aware, optional
//!                           user instruction) and patch the payload in place.
//!
//! Per-slide rendering costs n small calls instead of 1 big one, but each call
//! only has to be good for ~4 bullets — that is exactly the trade the confirm
//! round is buying. A slide whose JSON comes back unusable falls back to a
//! minimal plan-shaped slide rather than failing the whole deck.

use crate::commands::{apply_budget, custom_focus, guard_offline_llm, read_keys, style_instruction, NO_MODEL};
use crate::db::AppState;
use crate::error::{Error, Result};
use crate::models::MaterialRec;
use crate::repo;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

const PROGRESS_EVENT: &str = "slides:progress";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlidePlan {
    pub title: String,
    #[serde(default)]
    pub focus: String,
}

#[derive(Debug, Serialize)]
pub struct SlidesOutline {
    pub suggested_title: String,
    pub slides: Vec<SlidePlan>,
}

/// Shared setup, mirroring generate_material's scoping rules: the user's
/// explicit source selection is authoritative, else topic scope; context comes
/// from the summary tree (digests stretch an 18k-char budget across the whole
/// library) and degrades to plain fair-allocation text without one.
struct Deck {
    context: String,
    subject_name: String,
    topic_name: String,
    model: Box<dyn crate::llm::Llm>,
    style: String,
}

fn setup(
    app: &AppHandle,
    subject_id: &str,
    topic_id: Option<&str>,
    custom_prompt: Option<&str>,
    source_ids: Option<&[String]>,
) -> Result<Deck> {
    let state = app.state::<AppState>();
    let c = state.db.lock().unwrap();
    let has_sel = source_ids.is_some_and(|v| !v.is_empty());
    let sel_topics: Vec<String> = if has_sel {
        Vec::new()
    } else {
        topic_id.map(|t| t.to_string()).into_iter().collect()
    };
    let (context, _) = crate::summary_tree::context_block(
        &c,
        subject_id,
        &sel_topics,
        if has_sel { source_ids } else { None },
        custom_prompt,
        &[],
        18000,
    )?;
    let subj = repo::get_subject(&c, subject_id)?;
    let tname = topic_id
        .and_then(|tid| subj.topics.iter().find(|t| t.id == tid))
        .or_else(|| subj.topics.first())
        .map(|t| t.name.clone())
        .unwrap_or_default();
    let spec = repo::get_setting(&c, "model_cheatsheet")?
        .unwrap_or_else(|| "openrouter:deepseek/deepseek-v4-flash".into());
    guard_offline_llm(&c, &spec)?;
    let keys = read_keys(&c)?;
    let style = style_instruction(&c);
    let mut model =
        crate::llm::from_spec_or_any(&spec, &keys).ok_or_else(|| Error::Other(NO_MODEL.into()))?;
    apply_budget(&mut model, &c, "cheatsheet");
    if context.trim().is_empty() {
        return Err(Error::Other(
            "No source text to generate from — add and ingest a source first.".into(),
        ));
    }
    Ok(Deck { context, subject_name: subj.name, topic_name: tname, model, style })
}

/// Round 1 — plan the deck. Returns an editable outline; nothing is persisted.
#[tauri::command]
pub async fn slides_outline(
    app: AppHandle,
    subject_id: String,
    topic_id: Option<String>,
    custom_prompt: Option<String>,
    source_ids: Option<Vec<String>>,
    count: Option<u32>,
) -> Result<SlidesOutline> {
    tauri::async_runtime::spawn_blocking(move || -> Result<SlidesOutline> {
        let ctx = setup(
            &app,
            &subject_id,
            topic_id.as_deref(),
            custom_prompt.as_deref(),
            source_ids.as_deref(),
        )?;
        let count_line = match count.map(|n| n.clamp(4, 20)) {
            Some(n) => format!("EXACTLY {n} slides"),
            None => "8-12 slides".to_string(),
        };
        let system = format!(
            "You plan a slideshow OUTLINE from study material. Output ONLY JSON: \
             {{\"slides\":[{{\"title\":\"...\",\"focus\":\"...\"}}]}}. {count_line}. \
             \"title\" is a tight slide heading (<= 8 words). \"focus\" is ONE sentence naming the \
             specific facts, definitions, numbers or examples the slide must land — never a vague \
             label like \"Overview of X\". Order the deck from fundamental to advanced and end with \
             a slide that ties it together. No prose outside JSON.{style} Respond with ONLY raw \
             JSON — no markdown code fences, no prose before or after.{custom}",
            style = ctx.style,
            custom = custom_focus(custom_prompt.as_deref())
        );
        let user = format!(
            "Subject: {} › {}\n\nSOURCE MATERIAL:\n{}\n\nPlan the deck now.",
            ctx.subject_name, ctx.topic_name, ctx.context
        );
        let raw = ctx.model.complete(&system, &user)?;
        {
            let state = app.state::<AppState>();
            let c = state.db.lock().unwrap();
            crate::tokens::record(&c, "material", &ctx.model.name(), &format!("{system}\n{user}"), &raw, Some(&subject_id));
        }
        let v = crate::llm::extract_json(&raw)
            .map_err(|_| Error::Other("model returned unstructured output; try again".into()))?;
        let mut slides: Vec<SlidePlan> = serde_json::from_value(
            v.get("slides").cloned().unwrap_or_else(|| v.clone()),
        )
        .map_err(|_| Error::Other("outline JSON is missing a \"slides\" array".into()))?;
        slides.retain(|s| !s.title.trim().is_empty());
        if slides.is_empty() {
            return Err(Error::Other("model produced an empty outline; try again".into()));
        }
        Ok(SlidesOutline { suggested_title: format!("{} — slideshow", ctx.topic_name), slides })
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

/// Pull {title, bullets, notes} out of a slide response; None when the model
/// came back unusable (caller substitutes the plan-shaped fallback).
fn slide_from_value(v: &serde_json::Value) -> Option<(String, Vec<String>, String)> {
    let obj = v.as_object()?;
    let title = obj.get("title").and_then(|x| x.as_str())?.trim().to_string();
    if title.is_empty() {
        return None;
    }
    let bullets: Vec<String> = obj
        .get("bullets")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|b| b.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let notes = obj
        .get("notes")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    Some((title, bullets, notes))
}

/// Round 2 — render the confirmed outline, one focused call per slide, then
/// save the deck through the ordinary `slideshow` material path.
#[tauri::command]
pub async fn render_slides(
    app: AppHandle,
    subject_id: String,
    topic_id: Option<String>,
    title: Option<String>,
    custom_prompt: Option<String>,
    outline: Vec<SlidePlan>,
) -> Result<MaterialRec> {
    tauri::async_runtime::spawn_blocking(move || -> Result<MaterialRec> {
        let outline: Vec<SlidePlan> = outline
            .into_iter()
            .filter(|s| !s.title.trim().is_empty())
            .collect();
        if outline.is_empty() {
            return Err(Error::Other("outline is empty — plan a deck first".into()));
        }
        let n = outline.len();
        let ctx = setup(&app, &subject_id, topic_id.as_deref(), custom_prompt.as_deref(), None)?;

        let deck_map = outline
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {} — {}", i + 1, s.title, s.focus))
            .collect::<Vec<_>>()
            .join("\n");
        let system = format!(
            "You write ONE slide of a study slideshow, following its plan. Output ONLY JSON: \
             {{\"title\":\"...\",\"bullets\":[\"...\"],\"notes\":\"voiceover\"}}. Rules: 3-5 bullets, \
             each an informative phrase (<= 18 words, plain text, NO markdown) with concrete facts, \
             definitions, numbers, formulas, cause/effect — never vague labels. \"notes\" is a 60-120 \
             word voiceover script that TEACHES the bullets conversationally and hands off to the \
             next slide. Keep the plan's title unless a sharper one fits the material.{style} \
             Respond with ONLY raw JSON — no markdown code fences, no prose.{custom}",
            style = ctx.style,
            custom = custom_focus(custom_prompt.as_deref())
        );

        let mut slides: Vec<serde_json::Value> = Vec::with_capacity(n);
        let mut fallbacks = 0usize;
        for (i, plan) in outline.iter().enumerate() {
            let _ = app.emit(
                PROGRESS_EVENT,
                serde_json::json!({ "idx": i + 1, "total": n, "title": plan.title }),
            );
            let flow_in = if i > 0 {
                format!(
                    "The previous slide \"{}\" just covered: {}.\n",
                    outline[i - 1].title, outline[i - 1].focus
                )
            } else {
                "This is the OPENING slide — hook the audience.\n".to_string()
            };
            let flow_out = if i + 1 == n {
                "This is the CLOSING slide — land the takeaway.\n".to_string()
            } else {
                String::new()
            };
            let user = format!(
                "Subject: {} › {}\n\nSOURCE MATERIAL:\n{}\n\nFULL DECK PLAN (for flow only — write \
                 slide {} of {} ONLY):\n{deck_map}\n\n{flow_in}{flow_out}\nWrite slide {} now: \
                 \"{}\" — {}",
                ctx.subject_name,
                ctx.topic_name,
                ctx.context,
                i + 1,
                n,
                i + 1,
                plan.title,
                plan.focus
            );
            let rendered = ctx
                .model
                .complete(&system, &user)
                .ok()
                .and_then(|raw| {
                    {
                        let state = app.state::<AppState>();
                        let c = state.db.lock().unwrap();
                        crate::tokens::record(&c, "material", &ctx.model.name(), &format!("{system}\n{user}"), &raw, Some(&subject_id));
                    }
                    crate::llm::extract_json(&raw).ok()
                })
                .and_then(|v| slide_from_value(&v));
            let (title_s, bullets, notes) = rendered.unwrap_or_else(|| {
                fallbacks += 1;
                (plan.title.clone(), vec![plan.focus.clone()], String::new())
            });
            slides.push(serde_json::json!({
                "title": title_s,
                "bullets": bullets,
                "notes": notes,
            }));
        }

        let payload = serde_json::json!({ "slides": slides });
        let meta = if fallbacks > 0 {
            format!("{n} slides · {fallbacks} 页自动回退")
        } else {
            format!("{n} slides · 已确认大纲")
        };
        let title = match title.filter(|t| !t.trim().is_empty()) {
            Some(t) => t,
            None => format!("{} — slideshow", ctx.topic_name),
        };
        let (id, topic) = {
            let state = app.state::<AppState>();
            let c = state.db.lock().unwrap();
            let id = repo::save_material(
                &c,
                &subject_id,
                topic_id.as_deref(),
                "slideshow",
                &title,
                &meta,
                &payload,
            )?;
            (id, ctx.topic_name.clone())
        };
        Ok(MaterialRec { id, kind: "slideshow".into(), title, topic, meta, status: "ready".into(), payload })
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

/// Post-render touch-up: redo ONE slide of a saved deck. Deck-aware (the model
/// sees every slide title plus the neighbours' content so the rewrite flows),
/// optionally steered by a user instruction; the payload is patched in place.
#[tauri::command]
pub async fn regenerate_slide(
    app: AppHandle,
    material_id: String,
    index: usize,
    instruction: Option<String>,
) -> Result<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(move || -> Result<serde_json::Value> {
        let (subject_id, payload, deck_titles, own, prev, next, spec, keys, style) = {
            let state = app.state::<AppState>();
            let c = state.db.lock().unwrap();
            let mat = repo::get_material(&c, &material_id)?;
            if mat.kind != "slideshow" {
                return Err(Error::Other("material is not a slideshow".into()));
            }
            let slides = mat
                .payload
                .get("slides")
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default();
            if index >= slides.len() {
                return Err(Error::Other(format!(
                    "slide index {index} out of range ({} slides)",
                    slides.len()
                )));
            }
            let subject_id: String = c
                .query_row(
                    "SELECT subject_id FROM materials WHERE id = ?1",
                    rusqlite::params![material_id],
                    |r| r.get(0),
                )
                .map_err(|_| Error::NotFound(format!("material {material_id}")))?;
            let peek = |i: usize| {
                serde_json::to_string_pretty(&slides[i]).unwrap_or_default()
            };
            let spec = repo::get_setting(&c, "model_cheatsheet")?
                .unwrap_or_else(|| "openrouter:deepseek/deepseek-v4-flash".into());
            guard_offline_llm(&c, &spec)?;
            let keys = read_keys(&c)?;
            let style = style_instruction(&c);
            (
                subject_id,
                mat.payload.clone(),
                slides
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        format!(
                            "{}. {}",
                            i + 1,
                            s.get("title").and_then(|t| t.as_str()).unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                peek(index),
                if index > 0 { Some(peek(index - 1)) } else { None },
                if index + 1 < slides.len() { Some(peek(index + 1)) } else { None },
                spec,
                keys,
                style,
            )
        };
        // Lock released — the LLM call below must not hold the DB mutex.

        let ctx = setup(&app, &subject_id, None, None, None)?;
        // The explicit per-material model spec wins over the deck-wide default.
        let mut model = crate::llm::from_spec_or_any(&spec, &keys)
            .ok_or_else(|| Error::Other(NO_MODEL.into()))?;
        {
            let state = app.state::<AppState>();
            let c = state.db.lock().unwrap();
            apply_budget(&mut model, &c, "cheatsheet");
        }
        let steer = instruction
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| format!("\nThe user asks for this rewrite specifically: {s}"))
            .unwrap_or_default();
        let system = format!(
            "You rewrite ONE slide of an existing study slideshow. Output ONLY JSON: \
             {{\"title\":\"...\",\"bullets\":[\"...\"],\"notes\":\"voiceover\"}}. Keep the deck's \
             voice; sharpen the bullets with material facts (3-5 bullets, <= 18 words each, plain \
             text); \"notes\" is a 60-120 word voiceover that hands off to the next slide. Do not \
             repeat the neighbours' content.{style} Respond with ONLY raw JSON — no markdown code \
             fences, no prose.{steer}",
            style = style,
            steer = steer
        );
        let neighbours = match (&prev, &next) {
            (Some(p), Some(x)) => format!("\nPREVIOUS SLIDE (do not repeat):\n{p}\n\nNEXT SLIDE (hand off to it):\n{x}\n"),
            (Some(p), None) => format!("\nPREVIOUS SLIDE (do not repeat):\n{p}\n\nThis is the CLOSING slide — land the takeaway.\n"),
            (None, Some(x)) => format!("\nThis is the OPENING slide — hook the audience.\n\nNEXT SLIDE (hand off to it):\n{x}\n"),
            (None, None) => String::new(),
        };
        let user = format!(
            "FULL DECK (titles):\n{deck_titles}\n{neighbours}\nSLIDE TO REWRITE (current content, \
             as JSON):\n{own}\n\nSOURCE MATERIAL:\n{}\n\nRewrite slide {} now.",
            ctx.context,
            index + 1
        );
        let raw = model.complete(&system, &user)?;
        {
            let state = app.state::<AppState>();
            let c = state.db.lock().unwrap();
            crate::tokens::record(&c, "material", &model.name(), &format!("{system}\n{user}"), &raw, Some(&subject_id));
        }
        let v = crate::llm::extract_json(&raw)
            .map_err(|_| Error::Other("model returned unstructured output; try again".into()))?;
        let (title_s, bullets, notes) =
            slide_from_value(&v).ok_or_else(|| Error::Other("slide came back unusable; try again".into()))?;
        let mut payload = payload;
        if let Some(arr) = payload.get_mut("slides").and_then(|s| s.as_array_mut()) {
            arr[index] = serde_json::json!({ "title": title_s, "bullets": bullets, "notes": notes });
        }
        {
            let state = app.state::<AppState>();
            let c = state.db.lock().unwrap();
            repo::update_material_payload(&c, &material_id, &payload)?;
        }
        Ok(payload)
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}
