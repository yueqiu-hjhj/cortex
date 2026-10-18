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
//! Testability: the whole request layer works on `&AppState` plus an
//! `Option<&AppHandle>`, so the tests below drive real HTTP semantics (routing,
//! status codes, JSON shape, DB writes) without booting a Tauri app. The
//! AppHandle is only needed for the two things a headless test cannot have:
//! progress events and the managed-state lookup.

use crate::db::AppState;
use crate::ingest;
use crate::repo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

/// Default loopback port. `CORTEX_CLIP_PORT` overrides it (tests, or a machine
/// where 7878 is already taken).
pub const DEFAULT_PORT: u16 = 7878;

/// Cap on a request body. A clip payload is a URL plus an optional selection;
/// an unbounded read would let a bogus client pin a worker thread.
const MAX_BODY_BYTES: u64 = 64 * 1024;

/// Longest page title we keep as a source name.
const MAX_TITLE_CHARS: usize = 280;

/// The tag every extension clip carries, so a filtered search for "clip"
/// surfaces exactly what arrived from the browser.
const CLIP_TAG: &str = "clip";

type HttpResponse = tiny_http::Response<std::io::Cursor<Vec<u8>>>;

/// Bound address of the running clip server, filled in by `start`.
static BOUND_ADDR: OnceLock<String> = OnceLock::new();

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

fn handle_clip(state: &AppState, app: Option<&AppHandle>, req: ClipRequest) -> ClipResponse {
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
    // start, which is exactly what the extension must not wait for. (No app
    // handle in tests → nothing to emit progress to, so skip it.)
    if has_body {
        if let Some(app) = app {
            let app_bg = app.clone();
            let id = source_id.clone();
            let subject = subject_id.clone();
            let topic = topic_id.clone();
            let text = body.clone();
            let spawned = std::thread::Builder::new()
                .name("cortex-clip-embed".into())
                .spawn(move || {
                    if let Err(e) = crate::commands::finish_text_ingest(
                        &app_bg,
                        &id,
                        &subject,
                        topic.as_deref(),
                        &text,
                    ) {
                        eprintln!("[clip] embedding source {id} failed: {e}");
                    }
                });
            if let Err(e) = spawned {
                eprintln!("[clip] could not spawn the embed worker: {e}");
            }
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

/// The whole request layer. `app` is optional purely so tests can drive this
/// without a Tauri runtime; only the background embed needs it.
fn route(
    state: &AppState,
    app: Option<&AppHandle>,
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
                let resp = handle_clip(state, app, req);
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

fn serve_request(app: AppHandle, mut request: tiny_http::Request) {
    let method = request.method().as_str().to_string();
    let raw_url = request.url().to_string();
    let path = raw_url.split('?').next().unwrap_or("/").to_string();
    let mut body = Vec::new();
    {
        let mut reader = request.as_reader().take(MAX_BODY_BYTES);
        let _ = reader.read_to_end(&mut body);
    }
    let response = {
        // Scoped so the managed-state guard is dropped before the response is
        // written back to the socket.
        let state = app.state::<AppState>();
        route(&state, Some(&app), &method, &path, &body)
    };
    if let Err(e) = request.respond(response) {
        eprintln!("[clip] could not answer a request: {e}");
    }
}

/// Bind 127.0.0.1 and serve for the life of the process. Called once from the
/// Tauri setup hook; the accept loop runs on its own thread so startup (and
/// first paint) never waits on the listener.
pub fn start(app: AppHandle) -> std::result::Result<u16, String> {
    let port = std::env::var("CORTEX_CLIP_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    let bound = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(port);
    let _ = BOUND_ADDR.set(format!("127.0.0.1:{bound}"));
    eprintln!("[clip] browser-clip server listening on 127.0.0.1:{bound}");

    std::thread::Builder::new()
        .name("cortex-clip-server".into())
        .spawn(move || {
            for request in server.incoming_requests() {
                let app = app.clone();
                // One thread per request: a clip does a blocking page fetch and
                // must not stall the accept loop or a concurrent /health ping.
                if let Err(e) = std::thread::Builder::new()
                    .name("cortex-clip-req".into())
                    .spawn(move || serve_request(app, request))
                {
                    eprintln!("[clip] could not spawn a request thread: {e}");
                }
            }
        })
        .map_err(|e| format!("spawn the clip server thread: {e}"))?;

    Ok(bound)
}

