//! Tauri glue for the browser-clip server.
//!
//! Everything that needs a running app — the managed `AppState` lookup, the
//! `AppHandle` progress events, and the accept loop itself — lives here. The
//! HTTP protocol and routing live in `clip_server.rs`, which deliberately has
//! no Tauri imports so its tests link without the GUI stack on Windows.

use crate::clip_server::{self, ClipHost, MAX_BODY_BYTES};
use crate::commands::finish_text_ingest;
use crate::db::AppState;
use std::io::Read;
use tauri::{AppHandle, Manager};

/// The real host behind a clip: hands the slow chunk + embed half to a
/// detached thread after the HTTP reply has already gone out.
struct TauriClipHost {
    app: AppHandle,
}

impl ClipHost for TauriClipHost {
    fn schedule_embed(
        &self,
        source_id: &str,
        subject_id: &str,
        topic_id: Option<&str>,
        text: &str,
    ) {
        let app = self.app.clone();
        let source_id = source_id.to_string();
        let subject_id = subject_id.to_string();
        let topic_id = topic_id.map(str::to_string);
        let text = text.to_string();
        let spawned = std::thread::Builder::new()
            .name("cortex-clip-embed".into())
            .spawn(move || {
                if let Err(e) =
                    finish_text_ingest(&app, &source_id, &subject_id, topic_id.as_deref(), &text)
                {
                    eprintln!("[clip] embedding source {source_id} failed: {e}");
                }
            });
        if let Err(e) = spawned {
            eprintln!("[clip] could not spawn the embed worker: {e}");
        }
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
    let host = TauriClipHost { app: app.clone() };
    let response = {
        // Scoped so the managed-state guard is dropped before the response is
        // written back to the socket.
        let state = app.state::<AppState>();
        clip_server::route(state.inner(), Some(&host), &method, &path, &body)
    };
    if let Err(e) = request.respond(response) {
        eprintln!("[clip] could not answer a request: {e}");
    }
}

/// Bind 127.0.0.1 and serve for the life of the process. Called once from the
/// Tauri setup hook; the accept loop runs on its own thread so startup (and
/// first paint) never waits on the listener.
pub fn start(app: AppHandle) -> std::result::Result<u16, String> {
    let port = clip_server::configured_port();
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    let bound = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(port);
    clip_server::set_bound_addr(format!("127.0.0.1:{bound}"));
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
