//! Browser-extension clip server.
//!
//! Listens on 127.0.0.1 while Cortex is running so the manifest-V3 browser
//! extension (see `browser-extension/`) can POST the page the user is reading
//! straight into a subject. Goal: turn "open Cortex, paste URL, wait for the
//! fetch" into one click of the toolbar icon or one right-click.
//!
//! Routes
//!   GET  /health     liveness probe — the extension pings this on every click
//!   GET  /version    version + bound port
//!   GET  /subjects   subject list for the popup's picker
//!   POST /clip       fetch + ingest a URL (optionally just the selection)
//!   OPTIONS *        CORS preflight
//!
//! Security: loopback-only bind, no auth token — the same trust boundary the
//! app itself has (a local user). CORS is open so the extension popup works
//! even if the browser tightens extension host permissions later.
//!
//! Threading: tiny_http hands us requests synchronously, so each request is
//! served on its own std::thread. The truly slow part of a clip — chunk +
//! embed, which may load the ONNX model — runs on a detached thread AFTER the
//! HTTP reply is sent, because the extension must never wait on the embedder.
//!
//! Testability: the whole request layer works on `&AppState` plus an optional
//! `&dyn ClipHost`, so the tests below drive real HTTP semantics (routing,
//! status codes, JSON shape, DB writes) without booting a Tauri app. The host
//! half — managed-state lookup and progress events — lives behind the
//! `ClipHost` trait and is implemented in `clip_host.rs`. Keeping this module
//! free of `tauri::AppHandle` is deliberate: a Windows test binary that links
//! Tauri's GUI stack fails to load with STATUS_ENTRYPOINT_NOT_FOUND.

use crate::db::AppState;
use crate::ingest;
use crate::repo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;

/// Default loopback port. `CORTEX_CLIP_PORT` overrides it (tests, or a machine
/// where 7878 is already taken).
pub const DEFAULT_PORT: u16 = 7878;

/// Cap on a request body. A clip payload is a URL plus an optional selection;
/// an unbounded read would let a bogus client pin a worker thread.
pub(crate) const MAX_BODY_BYTES: u64 = 64 * 1024;

/// Longest page title we keep as a source name.
const MAX_TITLE_CHARS: usize = 280;

/// The tag every extension clip carries, so a filtered search for "clip"
/// surfaces exactly what arrived from the browser.
const CLIP_TAG: &str = "clip";

pub(crate) type HttpResponse = tiny_http::Response<std::io::Cursor<Vec<u8>>>;

/// Bound address of the running clip server, filled in by `clip_host::start`.
static BOUND_ADDR: OnceLock<String> = OnceLock::new();

/// The host half of a clip. `clip_server` owns the HTTP protocol; `clip_host`
/// owns everything that needs a running Tauri app. Routing therefore stays
/// testable with a mock host and never names `tauri::AppHandle`.
pub(crate) trait ClipHost {
    /// Hand the slow chunk + embed half to the host. Must return immediately:
    /// the HTTP reply has already gone out by the time this runs.
    fn schedule_embed(
        &self,
        source_id: &str,
        subject_id: &str,
        topic_id: Option<&str>,
        text: &str,
    );
}

/// Port the listener should try, honoring `CORTEX_CLIP_PORT`.
pub(crate) fn configured_port() -> u16 {
    std::env::var("CORTEX_CLIP_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// Record the address `clip_host::start` actually bound, for `status()`.
pub(crate) fn set_bound_addr(addr: String) {
    let _ = BOUND_ADDR.set(addr);
}

/// `127.0.0.1:<port>` of the running clip server, or None when it never bound.
pub fn clip_server_addr() -> Option<String> {
    BOUND_ADDR.get().cloned()
}

/// What the `clip_server_status` Tauri command reports to Settings.
#[derive(Debug, Clone, Serialize)]
pub struct ClipServerStatus {
    pub running: bool,
    pub addr: Option<String>,
}

/// Liveness snapshot for the Settings row.
pub fn status() -> ClipServerStatus {
    ClipServerStatus {
        running: BOUND_ADDR.get().is_some(),
        addr: clip_server_addr(),
    }
}

fn cors_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
        ("Access-Control-Allow-Headers", "Content-Type"),
        ("Access-Control-Max-Age", "86400"),
    ]
}

fn json_response(code: u16, body: &Value) -> HttpResponse {
    let mut resp = tiny_http::Response::from_string(body.to_string()).with_status_code(code);
    for (k, v) in cors_headers() {
        resp.add_header(tiny_http::Header::from_bytes(k, v).unwrap());
    }
    resp.add_header(
        tiny_http::Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap(),
    );
    resp
}

/// 204 for CORS preflight. Chrome also wants
/// `Access-Control-Allow-Private-Network` when a public origin probes a
/// loopback address, so answer it and let the preflight pass.
fn preflight_response() -> HttpResponse {
    let mut resp = tiny_http::Response::from_data(Vec::<u8>::new()).with_status_code(204u16);
    for (k, v) in cors_headers() {
        resp.add_header(tiny_http::Header::from_bytes(k, v).unwrap());
    }
    resp.add_header(
        tiny_http::Header::from_bytes("Access-Control-Allow-Private-Network", "true").unwrap(),
    );
    resp
}

fn not_found() -> HttpResponse {
    json_response(404, &json!({ "ok": false, "error": "not found" }))
}

/// One row of the `/subjects` picker.
#[derive(Debug, Serialize)]
struct SubjectRow {
    id: String,
    name: String,
    glyph: String,
}

#[derive(Debug, Deserialize)]
struct ClipRequest {
    url: String,
    /// Title the extension already knows (the tab title), kept so the source
    /// name carries WHY it was clipped, not only where from.
    #[serde(default)]
    title: Option<String>,
    /// Text highlighted before clipping. With `selection_only` it replaces the
    /// page body; otherwise it is prepended as retrieval grounding.
    #[serde(default)]
    selection: Option<String>,
    #[serde(default)]
    selection_only: bool,
    #[serde(default)]
    subject_id: Option<String>,
    #[serde(default)]
    topic_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ClipResponse {
    ok: bool,
    source_id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    /// Saved, but not the way the user asked (link-only fallback, nothing
    /// readable on the page…). The popup shows this next to the success note.
    warning: Option<String>,
    error: Option<String>,
}

impl ClipResponse {
    fn fail(title: Option<String>, err: &str) -> Self {
        ClipResponse {
            ok: false,
            source_id: None,
            title,
            url: None,
            warning: None,
            error: Some(err.to_string()),
        }
    }
}

/// Fetch a URL with a blocking client and reduce it to (title, readable text,
/// final URL) through the same extractor the in-app fetch uses. No JS runs, so
/// this is safe for every URL class — it just comes back thin on JS-only pages.
fn fetch_readable(url: &str) -> std::result::Result<(String, String, String), String> {
    let client = crate::commands::http_client(20);
    let resp = client
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
        )
        .header("Accept", "text/html,application/xhtml+xml")
        .send()
        .map_err(|e| format!("fetch failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("the page answered HTTP {status}"));
    }
    let final_url = resp.url().to_string();
    let html = resp
        .text()
        .map_err(|e| format!("could not read the page body: {e}"))?;
    let (title, text, _links) = ingest::readable_page(&html, &final_url);
    Ok((title, text, final_url))
}

/// Subject to file this clip under: the popup's choice, else the first
/// subject. Never invents an id — a clip into a non-existent subject would
/// either trip the foreign key or land invisible to every view.
fn resolve_subject(state: &AppState, requested: Option<&str>) -> std::result::Result<String, String> {
    if let Some(s) = requested.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(s.to_string());
    }
    let c = state.db.lock().unwrap();
    match repo::list_subjects(&c) {
        Ok(v) => v
            .into_iter()
            .next()
            .map(|s| s.id)
            .ok_or_else(|| "no subjects yet — create one in Cortex, then clip again".to_string()),
        Err(e) => Err(format!("could not read the subject list: {e}")),
    }
}

fn handle_clip(
    state: &AppState,
    host: Option<&dyn ClipHost>,
    req: ClipRequest,
) -> ClipResponse {
    let url = req.url.trim().to_string();
    if url.is_empty() {
        return ClipResponse::fail(None, "url is required");
    }

    let selection = req
        .selection
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // The page fetch is the one genuinely slow synchronous step (~20s worst
    // case, and it never blocks the HTTP accept loop — see `serve_request`).
    let (mut title, mut body, final_url, warning) = match fetch_readable(&url) {
        Ok((t, text, fu)) => (t, text, fu, None),
        Err(e) => {
            // Unreadable page (JS-only, paywalled, PDF, offline). Keep what the
            // user *could* tell us — their selection, or at least the link — so
            // the clip lands instead of evaporating.
            let note = if selection.is_some() {
                format!("saved only the part you selected — the page itself could not be read ({e})")
            } else {
                format!("saved the link only — the page could not be read ({e})")
            };
            (String::new(), String::new(), url.clone(), Some(note))
        }
    };

    if let Some(t) = req.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        title = format!("{t} — {title}");
    }
    if title.trim().is_empty() {
        title = final_url.clone();
    }
    let title: String = title.chars().take(MAX_TITLE_CHARS).collect();

    if let Some(sel) = &selection {
        body = if req.selection_only || body.trim().is_empty() {
            sel.clone()
        } else {
            format!("{sel}\n\n---\n\n{body}")
        };
    }

    let subject_id = match resolve_subject(state, req.subject_id.as_deref()) {
        Ok(s) => s,
        Err(e) => return ClipResponse::fail(Some(title), &e),
    };
    let topic_id = req.topic_id.filter(|t| !t.trim().is_empty());
    let has_body = !body.trim().is_empty();
    let stored_body = if has_body { Some(body.clone()) } else { None };

    // The row + tag land synchronously so the reply carries a real id; the
    // content is stored here too, so a link-only clip is still readable.
    let inserted = {
        let c = state.db.lock().unwrap();
        match repo::insert_source(&c, &subject_id, topic_id.as_deref(), &title, "web", Some(&url)) {
            Ok(id) => {
                let _ = repo::attach_tags(&c, &id, &[CLIP_TAG.to_string()]);
                let status = if has_body { "ingesting" } else { "draft" };
                let meta = match &stored_body {
                    Some(b) => format!("{} chars · clipped", b.chars().count()),
                    None => warning
                        .clone()
                        .unwrap_or_else(|| "link only · not fetched".to_string()),
                };
                let _ = repo::finalize_source(
                    &c,
                    &id,
                    status,
                    Some(&meta),
                    stored_body.as_deref(),
                    warning.as_deref(),
                );
                Ok(id)
            }
            Err(e) => Err(e),
        }
    };
    let source_id = match inserted {
        Ok(id) => id,
        Err(e) => return ClipResponse::fail(Some(title), &format!("could not save the clip: {e}")),
    };

    // Chunk + embed out-of-band: the ONNX embedder can take minutes on a cold
    // start, which is exactly what the extension must not wait for. The host
    // decides how to schedule it; tests pass a mock that only records the call.
    if has_body {
        if let Some(host) = host {
            host.schedule_embed(&source_id, &subject_id, topic_id.as_deref(), &body);
        }
    }

    ClipResponse {
        ok: true,
        source_id: Some(source_id),
        title: Some(title),
        url: Some(final_url),
        warning,
        error: None,
    }
}

fn handle_subjects(state: &AppState) -> Value {
    let c = state.db.lock().unwrap();
    let subjects: Vec<SubjectRow> = repo::list_subjects(&c)
        .map(|v| {
            v.into_iter()
                .map(|s| SubjectRow { id: s.id, name: s.name, glyph: s.glyph })
                .collect()
        })
        .unwrap_or_default();
    let default_subject = subjects.first().map(|s| s.id.clone());
    json!({ "ok": true, "subjects": subjects, "default_subject": default_subject })
}

fn bound_port() -> u16 {
    BOUND_ADDR
        .get()
        .and_then(|s| s.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()))
        .unwrap_or(DEFAULT_PORT)
}

/// The whole request layer. `host` is optional purely so tests can drive this
/// without a Tauri runtime; only the background embed needs it.
pub(crate) fn route(
    state: &AppState,
    host: Option<&dyn ClipHost>,
    method: &str,
    path: &str,
    body: &[u8],
) -> HttpResponse {
    match (method, path) {
        ("OPTIONS", _) => preflight_response(),
        ("GET", "/health") => json_response(
            200,
            &json!({ "ok": true, "app": "cortex", "version": env!("CARGO_PKG_VERSION") }),
        ),
        ("GET", "/version") => json_response(
            200,
            &json!({ "version": env!("CARGO_PKG_VERSION"), "clip_port": bound_port() }),
        ),
        ("GET", "/subjects") => json_response(200, &handle_subjects(state)),
        ("POST", "/clip") => match serde_json::from_slice::<ClipRequest>(body) {
            Ok(req) => {
                let resp = handle_clip(state, host, req);
                let code = if resp.ok { 200 } else { 400 };
                json_response(code, &serde_json::to_value(&resp).unwrap_or_else(|_| json!({})))
            }
            Err(e) => json_response(
                400,
                &json!({ "ok": false, "error": format!("bad request body: {e}") }),
            ),
        },
        _ => not_found(),
    }
}

#[cfg(test)]
mod clip_server_tests {
    use super::*;
    use std::io::Read;
    use std::sync::Mutex;

    /// A throwaway in-memory library with one subject, like a first-run user.
    fn state_with_subject() -> (AppState, String) {
        let st = AppState::in_memory().expect("in-memory db");
        let id = {
            let c = st.db.lock().unwrap();
            repo::insert_subject(&c, "测试学科", None, Some("📘"), None).expect("insert subject")
        };
        (st, id)
    }

    fn read_json(resp: HttpResponse) -> (u16, Value) {
        let code = resp.status_code().0;
        let mut buf = Vec::new();
        resp.into_reader().read_to_end(&mut buf).unwrap();
        let body = serde_json::from_slice(&buf).unwrap_or(Value::Null);
        (code, body)
    }

    /// Records every `schedule_embed` call so a test can assert the protocol
    /// layer handed off exactly the work it should — and nothing else.
    #[derive(Debug, Clone, PartialEq)]
    struct EmbedCall {
        source_id: String,
        subject_id: String,
        topic_id: Option<String>,
        text: String,
    }

    #[derive(Default)]
    struct MockHost {
        calls: Mutex<Vec<EmbedCall>>,
    }

    impl MockHost {
        fn calls(&self) -> Vec<EmbedCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ClipHost for MockHost {
        fn schedule_embed(
            &self,
            source_id: &str,
            subject_id: &str,
            topic_id: Option<&str>,
            text: &str,
        ) {
            self.calls.lock().unwrap().push(EmbedCall {
                source_id: source_id.to_string(),
                subject_id: subject_id.to_string(),
                topic_id: topic_id.map(str::to_string),
                text: text.to_string(),
            });
        }
    }

    const SAMPLE_PAGE: &str = "<html><head><title>Long-term memory in LLMs</title></head>\
                    <body><article><h1>Retrieval</h1><p>Vector search beats keyword \
                    search on paraphrase queries.</p></article></body></html>";

    /// Serve `page` once from a throwaway loopback server, so the fetch +
    /// readability-extract path runs end to end without touching the internet.
    fn serve_one_page(page: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let server = tiny_http::Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/paper");
        let httpd = std::thread::spawn(move || {
            if let Ok(req) = server.recv() {
                let _ = req.respond(
                    tiny_http::Response::from_string(page).with_header(
                        tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap(),
                    ),
                );
            }
        });
        (url, httpd)
    }

    #[test]
    fn health_answers_ok_and_names_the_app() {
        let st = AppState::in_memory().unwrap();
        let (code, body) = read_json(route(&st, None, "GET", "/health", b""));
        assert_eq!(code, 200);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["app"], json!("cortex"));
    }

    #[test]
    fn version_reports_the_port_it_defaults_to() {
        let st = AppState::in_memory().unwrap();
        let (code, body) = read_json(route(&st, None, "GET", "/version", b""));
        assert_eq!(code, 200);
        assert_eq!(body["clip_port"], json!(bound_port()));
    }

    #[test]
    fn preflight_is_204_and_satisfies_chrome_pna() {
        let st = AppState::in_memory().unwrap();
        let resp = route(&st, None, "OPTIONS", "/clip", b"");
        assert_eq!(resp.status_code().0, 204);
        let has = |name: &'static str| resp.headers().iter().any(|h| h.field.equiv(name));
        assert!(has("Access-Control-Allow-Origin"));
        assert!(has("Access-Control-Allow-Methods"));
        assert!(has("Access-Control-Allow-Private-Network"));
    }

    #[test]
    fn json_replies_carry_cors_so_the_popup_can_read_them() {
        let st = AppState::in_memory().unwrap();
        let resp = route(&st, None, "GET", "/health", b"");
        let has = |name: &'static str| resp.headers().iter().any(|h| h.field.equiv(name));
        assert!(has("Content-Type"));
        assert!(has("Access-Control-Allow-Origin"));
    }

    #[test]
    fn unknown_paths_are_404_json() {
        let st = AppState::in_memory().unwrap();
        let (code, body) = read_json(route(&st, None, "GET", "/nope", b""));
        assert_eq!(code, 404);
        assert_eq!(body["ok"], json!(false));
    }

    #[test]
    fn subjects_endpoint_feeds_the_pickers_dropdown() {
        let (st, id) = state_with_subject();
        let (code, body) = read_json(route(&st, None, "GET", "/subjects", b""));
        assert_eq!(code, 200);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["subjects"][0]["id"], json!(id));
        assert_eq!(body["subjects"][0]["name"], json!("测试学科"));
        assert_eq!(body["default_subject"], json!(id));
    }

    #[test]
    fn a_query_string_does_not_break_routing() {
        let st = AppState::in_memory().unwrap();
        // serve_request strips the query before routing; make sure the strip is
        // what actually happens rather than relying on route() ignoring it.
        let raw = "/health?t=1";
        let path = raw.split('?').next().unwrap();
        let (code, _) = read_json(route(&st, None, "GET", path, b""));
        assert_eq!(code, 200);
    }

    #[test]
    fn clip_without_a_url_is_a_400() {
        let st = AppState::in_memory().unwrap();
        let (code, body) = read_json(route(&st, None, "POST", "/clip", br#"{"url":"   "}"#));
        assert_eq!(code, 400);
        assert_eq!(body["ok"], json!(false));
        assert!(body["error"].as_str().unwrap().contains("url"));
    }

    #[test]
    fn clip_with_a_malformed_body_is_a_400() {
        let st = AppState::in_memory().unwrap();
        let (code, body) = read_json(route(&st, None, "POST", "/clip", b"{not json"));
        assert_eq!(code, 400);
        assert!(body["error"].as_str().unwrap().contains("bad request body"));
    }

    #[test]
    fn an_empty_library_is_told_to_make_a_subject_first() {
        let st = AppState::in_memory().unwrap();
        // Port 1 on loopback refuses instantly, so the fetch fails offline and
        // this never touches the network.
        let (code, body) = read_json(
            route(&st, None, "POST", "/clip", br#"{"url":"http://127.0.0.1:1/x"}"#),
        );
        assert_eq!(code, 400);
        assert!(
            body["error"].as_str().unwrap().contains("no subjects"),
            "unexpected error: {body}"
        );
    }

    #[test]
    fn an_unreadable_page_still_saves_the_link_under_the_clip_tag() {
        let (st, id) = state_with_subject();
        let body = format!(r#"{{"url":"http://127.0.0.1:1/nope","subject_id":"{id}"}}"#);
        let (code, out) = read_json(route(&st, None, "POST", "/clip", body.as_bytes()));
        assert_eq!(code, 200, "{out}");
        assert_eq!(out["ok"], json!(true));
        assert!(
            out["warning"].as_str().unwrap_or("").contains("link only"),
            "expected a link-only warning, got {out}"
        );

        let source_id = out["source_id"].as_str().unwrap();
        let c = st.db.lock().unwrap();
        let src = repo::get_source(&c, source_id).unwrap();
        assert_eq!(src.kind, "web");
        assert_eq!(src.status, "draft");
        assert_eq!(src.origin.as_deref(), Some("http://127.0.0.1:1/nope"));
        assert!(src.tags.iter().any(|t| t == CLIP_TAG), "{:?}", src.tags);
        // The fallback also lands in the subject the extension asked for.
        assert_eq!(src.subject_id, id);
    }

    #[test]
    fn a_selection_survives_a_page_we_cannot_read() {
        let (st, id) = state_with_subject();
        let body = format!(
            r#"{{"url":"http://127.0.0.1:1/x","subject_id":"{id}","selection":"  勾选的重点  ","selection_only":true}}"#
        );
        let (code, out) = read_json(route(&st, None, "POST", "/clip", body.as_bytes()));
        assert_eq!(code, 200, "{out}");
        assert_eq!(out["ok"], json!(true));

        let source_id = out["source_id"].as_str().unwrap();
        let c = st.db.lock().unwrap();
        let src = repo::get_source(&c, source_id).unwrap();
        assert_eq!(src.content.as_deref(), Some("勾选的重点"));
    }

    #[test]
    fn the_tab_title_is_folded_into_the_source_name() {
        let (st, id) = state_with_subject();
        let body = format!(
            r#"{{"url":"http://127.0.0.1:1/y","subject_id":"{id}","title":"为什么我存这个"}}"#
        );
        let (_, out) = read_json(route(&st, None, "POST", "/clip", body.as_bytes()));
        let name = out["title"].as_str().unwrap();
        assert!(name.starts_with("为什么我存这个 — "), "got {name}");
    }

    #[test]
    fn an_overlong_title_is_capped() {
        let (st, id) = state_with_subject();
        let long = "长".repeat(400);
        let body = format!(
            r#"{{"url":"http://127.0.0.1:1/long","subject_id":"{id}","title":"{long}"}}"#
        );
        let (code, out) = read_json(route(&st, None, "POST", "/clip", body.as_bytes()));
        assert_eq!(code, 200, "{out}");
        let name = out["title"].as_str().unwrap();
        assert_eq!(
            name.chars().count(),
            MAX_TITLE_CHARS,
            "got {} chars",
            name.chars().count()
        );
    }

    #[test]
    fn a_readable_page_is_extracted_stored_and_named_after_it() {
        let (url, httpd) = serve_one_page(SAMPLE_PAGE);
        let (st, id) = state_with_subject();
        let body = format!(r#"{{"url":"{url}","subject_id":"{id}"}}"#);
        let (code, out) = read_json(route(&st, None, "POST", "/clip", body.as_bytes()));
        httpd.join().unwrap();

        assert_eq!(code, 200, "{out}");
        assert_eq!(out["ok"], json!(true));
        assert!(out["warning"].is_null(), "unexpected warning: {out}");
        assert!(
            out["title"].as_str().unwrap().contains("Long-term memory"),
            "{out}"
        );

        let source_id = out["source_id"].as_str().unwrap();
        let c = st.db.lock().unwrap();
        let src = repo::get_source(&c, source_id).unwrap();
        assert_eq!(src.kind, "web");
        assert_eq!(src.origin.as_deref(), Some(url.as_str()));
        assert!(src.content.as_deref().unwrap_or("").contains("Vector search"));
        assert!(src.tags.iter().any(|t| t == CLIP_TAG));
        // Content arrived, so the row is awaiting its background embed, not a
        // dead "draft".
        assert_eq!(src.status, "ingesting");
    }

    #[test]
    fn a_readable_clip_schedules_the_background_embed() {
        let (url, httpd) = serve_one_page(SAMPLE_PAGE);
        let (st, id) = state_with_subject();
        let host = MockHost::default();
        let body = format!(r#"{{"url":"{url}","subject_id":"{id}"}}"#);
        let (code, out) = read_json(route(
            &st,
            Some(&host as &dyn ClipHost),
            "POST",
            "/clip",
            body.as_bytes(),
        ));
        httpd.join().unwrap();

        assert_eq!(code, 200, "{out}");
        let calls = host.calls();
        assert_eq!(calls.len(), 1, "expected one hand-off, got {calls:?}");
        assert_eq!(calls[0].source_id, out["source_id"].as_str().unwrap());
        assert_eq!(calls[0].subject_id, id);
        assert_eq!(calls[0].topic_id, None);
        assert!(
            calls[0].text.contains("Vector search"),
            "unexpected embed text: {:?}",
            calls[0].text
        );
    }

    #[test]
    fn a_link_only_clip_does_not_schedule_an_embed() {
        let (st, id) = state_with_subject();
        let host = MockHost::default();
        let body = format!(r#"{{"url":"http://127.0.0.1:1/nope","subject_id":"{id}"}}"#);
        let (code, out) = read_json(route(
            &st,
            Some(&host as &dyn ClipHost),
            "POST",
            "/clip",
            body.as_bytes(),
        ));
        assert_eq!(code, 200, "{out}");
        assert!(
            host.calls().is_empty(),
            "a link-only clip has no text to embed"
        );
    }

    #[test]
    fn a_clip_can_target_a_topic() {
        let (st, id) = state_with_subject();
        let topic = {
            let c = st.db.lock().unwrap();
            repo::insert_topic(&c, &id, "第 3 讲", None, &[]).unwrap()
        };
        let body = format!(
            r#"{{"url":"http://127.0.0.1:1/z","subject_id":"{id}","topic_id":"{topic}"}}"#
        );
        let (_, out) = read_json(route(&st, None, "POST", "/clip", body.as_bytes()));
        let source_id = out["source_id"].as_str().unwrap();
        let c = st.db.lock().unwrap();
        let src = repo::get_source(&c, source_id).unwrap();
        assert_eq!(src.topic_id.as_deref(), Some(topic.as_str()));
    }

    #[test]
    fn the_first_subject_is_the_fallback_when_the_popup_sends_none() {
        let (st, id) = state_with_subject();
        let (_, out) = read_json(
            route(&st, None, "POST", "/clip", br#"{"url":"http://127.0.0.1:1/fallback"}"#),
        );
        let source_id = out["source_id"].as_str().unwrap();
        let c = st.db.lock().unwrap();
        let src = repo::get_source(&c, source_id).unwrap();
        assert_eq!(src.subject_id, id);
    }
}

