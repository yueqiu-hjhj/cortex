//! Cortex backend library. Wires the SQLite-backed AppState into Tauri and
//! registers the command surface consumed by the Svelte frontend.

mod alerts;
mod analytics;
mod anki;
mod asr;
mod backup;
mod calendar;
mod clip_host;
mod clip_server;
mod commands;
mod db;
mod embed;
mod embed_onnx;
mod error;
mod exam;
mod secrets;
mod selfcheck;
mod google;
mod homelab;
mod ingest;
mod livesync;
pub mod literature;
pub mod vocab;
pub mod obsidian;
pub mod agents;
mod agent_exec;
pub mod citecheck;
mod deep_research;
mod video_overview;
pub mod zotero;
pub mod experiments;
pub mod ics;
pub mod wiki;
pub mod cross_book;
pub mod workitems;
pub mod planner;
pub mod papers;
pub mod plugins;
pub mod providers;
pub mod connectors;
pub mod drafts;
pub mod extraction;
pub mod summary_tree;
pub mod rerank;
pub mod eval;
pub mod llm;
mod models;
mod moodle;
mod mpv;
mod notes;
pub mod office;
mod recorder;
pub mod repo;
mod review;
mod sidecars;
mod sync;
mod vector;

use db::AppState;
use tauri::Manager;

/// Reveal (and focus) the main window — used by the tray's Open and left-click.
/// Desktop-only: its callers (tray + single-instance) don't exist on mobile.
#[cfg(desktop)]
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();
    // Desktop-only plugins. Single-instance (a second launch reveals/focuses the
    // existing window instead of spawning another process) and the auto-updater
    // don't exist on mobile, where the OS / app store handle those. Gated so the
    // iOS/Android build links without them.
    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
                show_main_window(app);
            }))
            .plugin(tauri_plugin_updater::Builder::new().build());
    }
    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        // Moodle SSO callback: the launch flow redirects to cortexmoodle://token=…
        // This handler receives the RAW callback URI (so the base64 token isn't
        // corrupted by URL normalization) and hands it to the moodle module.
        .register_uri_scheme_protocol("cortexmoodle", |ctx, request| {
            let raw = request.uri().to_string();
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || moodle::handle_sso_uri(&app, &raw));
            tauri::http::Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .body(
                    b"<html><body style=\"font-family:system-ui;padding:2rem;background:#111;color:#eee\">Signed in to Moodle. You can close this window.</body></html>"
                        .to_vec(),
                )
                .unwrap()
        })
        .setup(|app| {
            // Per-app data dir (created if missing); DB lives at cortex.db.
            // CORTEX_DATA_DIR overrides it — used to run an isolated, throwaway
            // instance (e.g. a screenshot/demo profile) WITHOUT touching the
            // real library at the default location.
            let dir = match std::env::var("CORTEX_DATA_DIR") {
                Ok(d) if !d.trim().is_empty() => std::path::PathBuf::from(d.trim()),
                _ => app.path().app_data_dir().expect("resolve app data dir"),
            };
            std::fs::create_dir_all(&dir).expect("create app data dir");
            // Built-in connector manifest (Zotero/Notion/GitHub) — must land in
            // plugins/ BEFORE ensure_loaded reads the directory.
            crate::connectors::seed_builtin(&dir);
            // plugins/*.json manifests — provider/material/leader/connector contributions.
            // Read once here so every later lookup is a lock-free read.
            crate::plugins::ensure_loaded(&dir);
            let db_path = dir.join("cortex.db");
            // NOTE: the homelab pull/merge intentionally does NOT run here — it
            // would block startup on network I/O (up to the HTTP timeout if the
            // homelab is unreachable). The frontend calls `sync_pull` in the
            // background after the window is shown; see store loadSyncStatus.
            let state = AppState::new(&db_path).expect("init database");

            // Demo/screenshot profile: when CORTEX_DEMO is set, fill a *fresh*
            // database with rich showcase data. Guarded to only seed an empty
            // DB, so it can never overwrite a real library.
            if std::env::var("CORTEX_DEMO").is_ok() {
                let c = state.db.lock().unwrap();
                if let Err(e) = repo::seed_showcase(&c) {
                    eprintln!("demo seed failed: {e}");
                }
            }

            app.manage(state);

            // Browser-extension clip server: a loopback-only HTTP listener
            // (127.0.0.1:7878) so the manifest-V3 extension can POST the
            // page the user is reading. Binds on a detached thread, so
            // startup and first paint never wait on it. A bind failure
            // (port taken) is non-fatal: the app runs without clipping.
            if let Err(e) = clip_host::start(app.handle().clone()) {
                eprintln!("[clip] server did not start: {e}");
            }

            // LIVE sync: a SQLite update hook flags every local write; the
            // livesync push worker debounces them into tiny deltas for the
            // homelab syncd service, and its WebSocket listener applies peers'
            // deltas within a second. Idles harmlessly when unconfigured.
            {
                let state = app.state::<AppState>();
                let c = state.db.lock().unwrap();
                c.update_hook(Some(|_action, _db: &str, table: &str, _rowid: i64| {
                    livesync::mark_dirty(table);
                }));
            }
            livesync::start(&app.handle().clone());

            // Background sync + homelab connect. The frontend (store.svelte.ts) only
            // syncs while a window is open and dies when it closes; this dedicated OS
            // thread keeps pulling/pushing and warming the homelab connection for as
            // long as the PROCESS lives — so a closed-window device (close-to-tray on
            // desktop) stays in sync without anyone reopening it. A plain thread (not
            // the async runtime) keeps it independent of webview/UI state; each tick is
            // panic-caught so a transient poisoned-lock/network error can't kill the loop.
            {
                let handle = app.handle().clone();
                let sources_dir = dir.join("sources");
                std::thread::spawn(move || {
                    // Re-point each source's stored_path against the current sources dir —
                    // moved OFF the setup() critical path (it was a synchronous fs scan + N
                    // DB writes before first paint). iOS rotates the app-container UUID on
                    // every reinstall/update, invalidating absolute stored_paths (asset://
                    // then 403s) when no sync has run since; idempotent + never bumps
                    // updated_at, so deferring it a moment past first paint is safe.
                    if let Some(state) = handle.try_state::<AppState>() {
                        if let Ok(c) = state.db.lock() {
                            let _ = sync::repoint_source_files(&c, &sources_dir);
                        }
                    }
                    // Resume transcriptions interrupted by a quit/crash: any audio
                    // source still `ingesting` goes back on the background queue.
                    asr::resume_pending(&handle);
                    // Let the frontend's launch sync go first on a normal foreground start.
                    std::thread::sleep(std::time::Duration::from_secs(30));
                    loop {
                        let h = handle.clone();
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            sync::background_tick(&h);
                        }));
                        std::thread::sleep(std::time::Duration::from_secs(
                            sync::BACKGROUND_INTERVAL_SECS,
                        ));
                    }
                });
            }

            // If a previous session crashed, its mpv music sidecar is still
            // alive and streaming — quit it before this session starts its own.
            mpv::cleanup_stale(app.handle());

            // Mobile: ask for notification permission once at startup, off the
            // setup path (the iOS prompt blocks the calling thread). Without
            // this, lecture-ready alerts and the scheduled deadline reminders
            // never appear — nothing on the phone asks by itself.
            #[cfg(mobile)]
            {
                use tauri_plugin_notification::{NotificationExt, PermissionState};
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let n = handle.notification();
                    if !matches!(n.permission_state(), Ok(PermissionState::Granted)) {
                        let _ = n.request_permission();
                    }
                    // Permission may have just been granted: put the deadline
                    // alerts in place from the data we already have (a Moodle
                    // sync later refreshes them).
                    alerts::schedule_deadline_alerts(&handle);
                });
            }

            // Tray icon: lets the app keep working (ingest, generation, music)
            // after the window is closed. Left-click or "Open" reopens it.
            // Desktop-only — there is no system tray on iOS/Android.
            #[cfg(desktop)]
            {
                use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
                use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

                let open = MenuItem::with_id(app, "open", "Open Cortex", true, None::<&str>)?;
                let dashboard =
                    MenuItem::with_id(app, "dashboard", "Go to Dashboard", true, None::<&str>)?;
                let music =
                    MenuItem::with_id(app, "music", "Play / pause music", true, None::<&str>)?;
                let sep1 = PredefinedMenuItem::separator(app)?;
                let restart = MenuItem::with_id(app, "restart", "Restart Cortex", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "Quit Cortex", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&open, &dashboard, &music, &sep1, &restart, &quit])?;
                let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
                TrayIconBuilder::with_id("cortex-tray")
                    .icon(icon)
                    .tooltip("Cortex")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "open" => show_main_window(app),
                        "dashboard" => {
                            use tauri::Emitter;
                            show_main_window(app);
                            let _ = app.emit("tray-go-dashboard", ());
                        }
                        "music" => {
                            use tauri::Emitter;
                            let _ = app.emit("tray-music-toggle", ());
                        }
                        "restart" => app.restart(),
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_main_window(tray.app_handle());
                        }
                    })
                    .build(app)?;
            }

            // On Linux (WebKitGTK) getUserMedia is denied by default, which
            // surfaces as NotAllowedError in the recorder. Auto-grant audio
            // media permission requests for the app's own webview.
            #[cfg(target_os = "linux")]
            {
                use tauri::Manager;
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.with_webview(|webview| {
                        use webkit2gtk::glib::Cast;
                        use webkit2gtk::{
                            PermissionRequestExt, UserMediaPermissionRequest, WebViewExt,
                        };
                        let wv = webview.inner();
                        wv.connect_permission_request(|_wv, req| {
                            if req.downcast_ref::<UserMediaPermissionRequest>().is_some() {
                                req.allow();
                                true
                            } else {
                                false
                            }
                        });

                        // WebKitGTK's built-in find-in-page grabs Ctrl/Ctrl+F at the
                        // GTK level — BELOW the webview's JS — so a JS capture-phase
                        // handler can't stop it, and it steals Ctrl+C/Ctrl+V (the
                        // user can't copy/paste). Intercept Ctrl+F here: block the
                        // native handler and open OUR find bar via a synthetic event.
                        // All other keys (incl. copy/paste) pass straight through.
                        use gtk::prelude::WidgetExt;
                        wv.connect_key_press_event(|wv: &webkit2gtk::WebView, ev: &gdk::EventKey| {
                            let ctrl = ev.state().contains(gdk::ModifierType::CONTROL_MASK);
                            let key = ev.keyval().name().map(|s| s.to_string()).unwrap_or_default();
                            if ctrl && (key == "f" || key == "F") {
                                wv.run_javascript(
                                    "window.dispatchEvent(new KeyboardEvent('keydown',{key:'f',ctrlKey:true,bubbles:true}))",
                                    None::<&webkit2gtk::gio::Cancellable>,
                                    |_| {},
                                );
                                return gtk::glib::Propagation::Stop;
                            }
                            gtk::glib::Propagation::Proceed
                        });
                    });
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Close-to-tray (Settings → "close to tray", default ON): hide the
            // window instead of exiting so ingest/generation/music continue.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let close_to_tray = {
                    let state = window.state::<AppState>();
                    let c = state.db.lock().unwrap();
                    repo::get_setting(&c, "close_to_tray")
                        .ok()
                        .flatten()
                        .as_deref()
                        != Some("false")
                };
                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_subjects,
            commands::get_subject,
            commands::create_subject,
            commands::update_subject,
            commands::delete_subject,
            commands::archive_subject,
            commands::list_archived_subjects,
            commands::open_external,
            commands::create_topic,
            commands::update_topic,
            commands::delete_topic,
            commands::reorder_subjects,
            commands::reorder_topics,
            commands::list_sources,
            commands::list_failed_sources,
            commands::get_source,
            commands::update_source,
            commands::delete_source,
            commands::move_source,
            commands::list_chunks,
            commands::get_setting,
            commands::set_setting,
            commands::add_source,
            commands::stage_upload,
            commands::reingest_source,
            commands::extract_flashcards_from_image,
            commands::ocr_image_bytes,
            commands::save_flashcards_to_subject,
            commands::search_chunks,
            commands::global_search,
            commands::seed_demo,
            commands::env_probe,
            commands::clip_server_status,
            commands::chat_answer,
            commands::add_chat_message,
            commands::list_chat_messages,
            commands::clear_chat,
            commands::new_chat,
            commands::list_chat_threads,
            commands::open_chat_thread,
            commands::generate_cheatsheet,
            commands::generate_subject_cheatsheet,
            commands::get_cheatsheet,
            commands::get_subject_cheatsheet,
            commands::update_cheatsheet,
            commands::list_cheatsheet_versions,
            commands::get_cheatsheet_version,
            commands::restore_cheatsheet_version,
            commands::export_pdf,
            commands::export_database,
            commands::export_obsidian,
            commands::export_anki,
            commands::import_anki,
            backup::backup_status,
            backup::backup_now,
            commands::optimize_db,
            commands::generate_material,
            commands::synthesize_overview,
            commands::list_materials,
            commands::delete_material,
            commands::onnx_status,
            commands::onnx_download,
            commands::delete_material,
            commands::onnx_status,
            commands::onnx_download,
            commands::sync_knowledge_graph,
            commands::list_knowledge_nodes,
            commands::get_knowledge_graph,
            commands::apply_observation,
            commands::calibration_summary,
            commands::sprint_plan,
            commands::list_undo_entries,
            commands::restore_undo,
            commands::set_secret,
            commands::get_secret,
            commands::delete_secret,
            commands::list_secrets,
            commands::startup_self_check,
            commands::rename_material,
            commands::add_citation,
            commands::list_citations,
            commands::update_citation,
            commands::delete_citation,
            commands::get_all_settings,
            commands::set_settings,
            commands::ollama_models,
            commands::verify_provider,
            commands::list_providers,
            commands::plugins_list,
            // connector plugins (D5) — fetch + ingest primitives over manifests
            connectors::connector_list,
            connectors::connector_configure,
            connectors::connector_disconnect,
            connectors::connector_sync,
            commands::retrieval_eval_real,
            commands::agents_execute_run,
            // platform (D5/D6/D8) — agents, wiki, cross-book, search, external provenance
            commands::agents_list,
            commands::agents_upsert,
            commands::agents_delete,
            commands::agents_start_run,
            commands::agents_list_runs,
            commands::agents_list_messages,
            commands::agents_list_skills,
            commands::agents_upsert_skill,
            commands::agents_list_tools,
            commands::agents_set_tool_enabled,
            commands::wiki_list,
            commands::wiki_get,
            commands::wiki_get_by_slug,
            commands::wiki_upsert,
            commands::wiki_delete,
            commands::wiki_reverse_links,
            commands::wiki_lint,
            commands::cross_book_detect,
            commands::cross_book_for_subject,
            commands::cross_book_apply,
            commands::search_notebook_isolated,
            commands::external_provenance_record,
            commands::external_provenance_for,
            commands::ai_suggest_glossary,
            commands::deep_research,
            commands::synthesize_video_overview,
            commands::zotero_import,
            commands::experiment_create,
            commands::experiment_list,
            commands::experiment_update,
            commands::experiment_delete,
            commands::export_draft_tex,
            commands::export_calendar_ics,
            commands::save_recording,
            commands::save_recording_raw,
            commands::transcribe_partial,
            commands::check_whisper_model,
            recorder::save_recording_path,
            recorder::native_rec_discard,
            recorder::native_rec_start,
            recorder::native_rec_pause,
            recorder::native_rec_resume,
            recorder::native_rec_stop,
            recorder::native_rec_cancel,
            recorder::native_rec_level,
            commands::web_search,
            commands::literature_search,
            commands::literature_explain,
            commands::literature_repair,
            commands::literature_save_source,
            commands::vocab_list,
            commands::vocab_add,
            commands::vocab_update,
            commands::vocab_delete,
            commands::vocab_extract,
            commands::add_memory,
            commands::confirm_memory,
            commands::list_memory,
            commands::delete_memory,
            // custom music stations + YouTube-audio (mpv sidecar)
            commands::list_custom_stations,
            commands::add_custom_station,
            commands::delete_custom_station,
            commands::reorder_custom_stations,
            mpv::media_tools_status,
            mpv::youtube_play,
            mpv::youtube_pause,
            mpv::youtube_resume,
            mpv::youtube_stop,
            mpv::youtube_set_volume,
            mpv::youtube_prewarm,
            commands::db_stats,
            commands::delete_all_data,
            commands::ping_url,
            commands::omarchy_theme,
            commands::fetch_page,
            sync::sync_status,
            sync::sync_test,
            sync::sync_push,
            sync::sync_pull,
            moodle::moodle_connect,
            moodle::moodle_set_token,
            moodle::moodle_login_sso,
            moodle::moodle_status,
            moodle::moodle_disconnect,
            moodle::moodle_sync,
            moodle::moodle_data,
            moodle::moodle_link_subject,
            moodle::moodle_autolink,
            commands::set_subject_framework,
            commands::get_subject_framework,
            commands::get_subject_framework_text,
            commands::clear_subject_framework,
            commands::set_subject_aliases,
            commands::retag_calendar_events,
            commands::dependency_status,
            commands::install_dependencies,
            commands::install_kind,
            commands::list_folder_sources,
            // notes
            notes::create_note,
            notes::list_notes,
            notes::get_note,
            notes::update_note,
            notes::delete_note,
            notes::note_to_source,
            // calendar
            calendar::create_event,
            calendar::list_events,
            calendar::update_event,
            calendar::set_event_checklist,
            calendar::delete_event,
            calendar::set_event_done,
            calendar::set_event_status,
            calendar::check_reminders,
            alerts::notification_route,
            homelab::homelab_status,
            // review
            review::record_attempt,
            review::review_set,
            review::srs_grade,
            review::srs_due,
            review::srs_preview,
            review::srs_stats,
            review::review_today,
            // study analytics
            analytics::log_pomodoro_session,
            analytics::pomodoro_segments,
            analytics::topic_stats,
            analytics::analytics_summary,
            // exam mode
            exam::generate_exam,
            exam::start_exam,
            exam::submit_exam,
            exam::remark_exam,
            exam::list_exams,
            exam::get_exam,
            exam::delete_exam,
            // google calendar
            google::google_status,
            google::google_connect,
            google::google_disconnect,
            google::google_sync,
            google::google_list_calendars,
            google::plan_calendar_sync,
            // work items — the cross-subject spine (migration 0037)
            commands::list_work_items,
            commands::get_work_item,
            commands::create_work_item,
            commands::update_work_item,
            commands::set_work_item_status,
            commands::add_work_item_minutes,
            commands::delete_work_item,
            commands::link_work_item,
            commands::unlink_work_item,
            commands::list_work_item_links,
            commands::work_items_for_entity,
            commands::list_goals,
            commands::upsert_goal,
            commands::delete_goal,
            commands::today_board,
            commands::list_plan_blocks,
            commands::upsert_plan_block,
            commands::delete_plan_block,
            // planner (Phase 2)
            commands::plan_week,
            commands::plan_rollover,
            commands::complete_plan_block,
            commands::plan_board,
            commands::materialize_goal,
            // papers (Phase 3)
            commands::paper_import,
            commands::paper_list,
            commands::paper_get,
            commands::paper_update,
            commands::paper_delete,
            commands::paper_start_reading,
            commands::excerpt_add,
            commands::excerpt_list,
            commands::excerpt_delete,
            commands::paper_bibtex,
            commands::paper_import_bibtex,
            commands::generate_draft,
            commands::draft_list,
            commands::draft_delete,
            commands::paper_build_matrix,
            commands::plan_vs_actual,
            commands::retrieval_eval,
            commands::plan_calibration,
            // office: materials/drafts -> real Office files + docx/pdf edit-back
            commands::export_material_docx,
            commands::export_material_xlsx,
            commands::export_material_pptx,
            commands::export_draft_docx,
            commands::docx_read_paragraphs,
            commands::docx_apply_edits,
            commands::pdf_merge,
            commands::pdf_split,
            commands::pdf_watermark,
            commands::pdf_form_fields,
            commands::pdf_fill_form,
            commands::docx_review_edits,
            // extraction templates (D8) — Elicit-style field-wise paper extraction
            extraction::extract_template_list,
            extraction::extract_template_save,
            extraction::extract_template_delete,
            extraction::extract_run_create,
            extraction::extract_run_list,
            extraction::extract_run_delete,
            extraction::extract_run_execute,
            extraction::extract_run_retry,
            extraction::extract_result_get,
            extraction::extract_result_csv,
            extraction::extract_result_csv_file,
            extraction::extract_cell_set,
            // summary tree (D6) — hierarchical digests + budgeted context
            summary_tree::summary_tree_status,
            summary_tree::summary_tree_build,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Cortex")
        .run(|_app, event| {
            // Tear down the headless mpv music sidecar when the app exits.
            if let tauri::RunEvent::Exit = event {
                mpv::shutdown();
            }
        });
}

#[cfg(test)]
mod pipeline_tests {
    //! End-to-end offline proof of the ingestion + retrieval data flow, exercised
    //! without the GUI: parse → chunk → embed(stub) → store → cosine search.
    use crate::embed::Embedder;
    use crate::{db::AppState, embed, ingest, repo, vector::f32s_to_blob};

    #[test]
    fn ingest_then_search_finds_the_source() {
        let st = AppState::in_memory().unwrap();
        let c = st.db.lock().unwrap();

        let sid = repo::insert_subject(&c, "Algorithms", Some("CS-3490"), None, None).unwrap();
        let tid = repo::insert_topic(&c, &sid, "Dynamic programming", None, &[]).unwrap();
        let srcid = repo::insert_source(&c, &sid, Some(&tid), "dp.md", "md", None).unwrap();

        let text = "Dynamic programming solves problems by combining solutions to \
                    overlapping subproblems. Memoization caches results top-down; \
                    tabulation fills a table bottom-up. The coin change recurrence is \
                    dp[a] = 1 + min over coins c of dp[a - c].";
        let chunks = ingest::chunk_text(text, 120, 30);
        assert!(!chunks.is_empty(), "chunker produced chunks");

        let emb = embed::StubEmbedder;
        let vecs = ingest::embed_chunks(&emb, &chunks).unwrap();
        for (i, (chunk, v)) in chunks.iter().zip(vecs.iter()).enumerate() {
            repo::insert_chunk(
                &c, &srcid, &sid, Some(&tid), i as i64, chunk, None,
                v.len() as i64, &f32s_to_blob(v),
            )
            .unwrap();
        }
        repo::finalize_source(&c, &srcid, "ready", Some("test"), Some(text), None).unwrap();
        assert!(repo::count_chunks(&c, &srcid).unwrap() > 0, "chunks stored");

        // query retrieval: a related query should surface this source as top hit
        let qvec = &emb.embed(&["memoization overlapping subproblems".into()]).unwrap()[0];
        let hits = repo::search_chunks(&c, Some(&sid), qvec, 5).unwrap();
        assert!(!hits.is_empty(), "search returned hits");
        assert_eq!(hits[0].source_id, srcid, "top hit is the ingested source");
        assert!(hits[0].score > 0.0, "positive similarity");

        // seeded-demo style listing renders the tree end to end
        let subs = repo::list_subjects(&c).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].source_count, 1);
        assert_eq!(subs[0].topics[0].sources[0].name, "dp.md");
    }
}
