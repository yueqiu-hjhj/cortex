//! Notes command surface. Mirrored 1:1 in `src/lib/api.ts`. Sync commands lock
//! the DB inline like `commands.rs`; `note_to_source` does heavy (embed) work so
//! it runs in `spawn_blocking` exactly like `add_source` to avoid the
//! reqwest::blocking-in-async runtime panic.

use crate::db::AppState;
use crate::embed;
use crate::error::{Error, Result};
use crate::ingest;
use crate::models::*;
use crate::repo;
use crate::vector::f32s_to_blob;
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub fn create_note(
    state: State<AppState>,
    subject_id: Option<String>,
    topic_id: Option<String>,
    title: String,
    body: String,
) -> Result<Note> {
    let c = state.db.lock().unwrap();
    let id = repo::insert_note(
        &c,
        subject_id.as_deref(),
        topic_id.as_deref(),
        &title,
        &body,
    )?;
    // [[double links]] index — rebuilt on every save (0045).
    crate::note_links::rebuild(&c, &id, &body)?;
    let note = repo::get_note(&c, &id)?;
    mirror_to_vault(&state, &c, &note);
    Ok(note)
}

#[tauri::command]
pub fn list_notes(state: State<AppState>, subject_id: Option<String>) -> Result<Vec<Note>> {
    let c = state.db.lock().unwrap();
    repo::list_notes(&c, subject_id.as_deref())
}

#[tauri::command]
pub fn get_note(state: State<AppState>, id: String) -> Result<Note> {
    let c = state.db.lock().unwrap();
    repo::get_note(&c, &id)
}

#[tauri::command]
pub fn update_note(
    state: State<AppState>,
    id: String,
    title: String,
    body: String,
) -> Result<Note> {
    let c = state.db.lock().unwrap();
    repo::update_note(&c, &id, &title, &body)?;
    crate::note_links::rebuild(&c, &id, &body)?;
    let note = repo::get_note(&c, &id)?;
    mirror_to_vault(&state, &c, &note);
    Ok(note)
}

/// Push a saved note out to its Markdown file (vault layer, P1). Deliberately
/// best-effort: the DB row is already committed, so a transient filesystem
/// error (file locked by an editor, vault on a detached drive) is logged rather
/// than reported as a failed save. The next save or startup pass repairs it.
pub(crate) fn mirror_to_vault(state: &AppState, c: &rusqlite::Connection, note: &Note) {
    let Some(root) = crate::vault::root_for(c, state.data_dir.as_deref()) else {
        return;
    };
    if let Err(e) = crate::vault::write_note(c, &root, note) {
        eprintln!("[vault] could not write note {}: {e}", note.id);
    }
}

/// Notes that link to `title` via [[double links]] (backlinks panel).
#[tauri::command]
pub fn note_backlinks(state: State<AppState>, title: String) -> Result<Vec<crate::note_links::Backlink>> {
    let c = state.db.lock().unwrap();
    crate::note_links::backlinks(&c, &title)
}

/// Resolve a link target to an existing note (id, title) — click-through.
#[tauri::command]
pub fn note_resolve_link(state: State<AppState>, target: String) -> Result<Option<(String, String)>> {
    let c = state.db.lock().unwrap();
    crate::note_links::resolve(&c, &target)
}

#[tauri::command]
pub fn delete_note(state: State<AppState>, id: String) -> Result<()> {
    let c = state.db.lock().unwrap();
    use rusqlite::OptionalExtension;
    let rel: Option<String> = c
        .query_row(
            "SELECT vault_path FROM notes WHERE id=?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    repo::delete_note(&c, &id)?;
    // Only remove the file after the row is gone, so a failed delete never
    // leaves a note whose DB row points at a missing file.
    if let (Some(root), Some(rel)) = (
        crate::vault::root_for(&c, state.data_dir.as_deref()),
        rel.filter(|p| !p.trim().is_empty()),
    ) {
        crate::vault::remove_note(&root, &rel);
    }
    Ok(())
}

/// Where the vault lives, and how much of the library is already on disk.
/// Surfaced in Settings so the user can see (and copy) the path, and confirm
/// that notes from another editor were picked up.
#[derive(serde::Serialize)]
pub struct VaultStatus {
    pub root: String,
    pub files: usize,
    pub notes: usize,
    pub on_disk: usize,
    /// True when the path came from the `vault_path` setting rather than the
    /// app-private default.
    pub custom: bool,
}

#[tauri::command]
pub fn vault_status(state: State<AppState>) -> Result<VaultStatus> {
    let c = state.db.lock().unwrap();
    let root = crate::vault::root_for(&c, state.data_dir.as_deref()).unwrap_or_default();
    let custom = repo::get_setting(&c, crate::vault::SETTING_VAULT_PATH)?
        .map(|p| !p.trim().is_empty())
        .unwrap_or(false);
    let notes: i64 = c.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))?;
    let on_disk: i64 = c.query_row(
        "SELECT COUNT(*) FROM notes WHERE vault_path IS NOT NULL AND vault_path<>''",
        [],
        |r| r.get(0),
    )?;
    Ok(VaultStatus {
        files: crate::vault::count_files(&root),
        root: root.to_string_lossy().into_owned(),
        notes: notes as usize,
        on_disk: on_disk as usize,
        custom,
    })
}

/// Point the vault at a directory (an existing Obsidian vault, a new folder,
/// or empty to fall back to the app default). Immediately reindexes so the
/// library reflects whatever is already in there, then exports any note that
/// has no file yet.
#[tauri::command]
pub fn vault_set_path(state: State<AppState>, path: String) -> Result<VaultStatus> {
    {
        let c = state.db.lock().unwrap();
        if path.trim().is_empty() {
            c.execute(
                "DELETE FROM settings WHERE key=?1",
                rusqlite::params![crate::vault::SETTING_VAULT_PATH],
            )?;
        } else {
            repo::set_setting(&c, crate::vault::SETTING_VAULT_PATH, path.trim())?;
        }
    }
    vault_reindex(state)
}

/// Rescan the vault now (the manual equivalent of the startup pass).
#[tauri::command]
pub fn vault_reindex(state: State<AppState>) -> Result<VaultStatus> {
    let c = state.db.lock().unwrap();
    let root = crate::vault::root_for(&c, state.data_dir.as_deref()).unwrap_or_default();
    crate::vault::reindex(&c, &root)?;
    crate::vault::export_missing(&c, &root)?;
    drop(c);
    vault_status(state)
}

/// Convert a note into a first-class source: create a `kind="note"` source,
/// link it back to the note, then chunk + embed the note body using the same
/// pipeline `add_source` uses. Heavy work runs in `spawn_blocking`.
#[tauri::command]
pub async fn note_to_source(app: AppHandle, id: String) -> Result<IngestResult> {
    tauri::async_runtime::spawn_blocking(move || -> Result<IngestResult> {
        let state = app.state::<AppState>();

        // 1. read the note + create the source row (locked)
        let (text, source_id, subject_id, topic_id) = {
            let c = state.db.lock().unwrap();
            let note = repo::get_note(&c, &id)?;
            let subject_id = note.subject_id.clone().ok_or_else(|| {
                Error::Other(
                    "note has no subject — assign it to a subject before converting".into(),
                )
            })?;
            let name = if note.title.trim().is_empty() {
                "Untitled note".to_string()
            } else {
                note.title.clone()
            };
            let topic_id = note.topic_id.clone();
            let source_id =
                repo::insert_source(&c, &subject_id, topic_id.as_deref(), &name, "note", None)?;
            repo::set_note_source(&c, &id, &source_id)?;
            (note.body, source_id, subject_id, topic_id)
        };

        let chars = text.chars().count() as i64;
        let chunks = ingest::chunk_text(&text, 900, 150);

        // 2. embed (no lock) — same settings-driven embedder as add_source
        let (provider, keys, ollama_url, embed_model) = {
            let c = state.db.lock().unwrap();
            (
                repo::get_setting(&c, "embed_provider")?.unwrap_or_else(|| "stub".into()),
                crate::commands::read_keys(&c)?,
                repo::get_setting(&c, "ollama_url")?,
                crate::commands::embed_model_override(&c),
            )
        };
        let embedder =
            embed::from_settings(&provider, &keys, ollama_url.as_deref(), state.data_dir.as_deref(), embed_model.as_deref());
        let vectors = match ingest::embed_chunks(embedder.as_ref(), &chunks) {
            Ok(v) => v,
            Err(_) => {
                // fall back to the stub so conversion never hard-fails on a bad key
                let stub = embed::StubEmbedder;
                ingest::embed_chunks(&stub, &chunks)?
            }
        };

        // 3. store chunks + finalize (locked)
        let c = state.db.lock().unwrap();
        for (i, (chunk, vec)) in chunks.iter().zip(vectors.iter()).enumerate() {
            repo::insert_chunk(
                &c,
                &source_id,
                &subject_id,
                topic_id.as_deref(),
                i as i64,
                chunk,
                None,
                vec.len() as i64,
                &f32s_to_blob(vec),
            )?;
        }
        let chunk_count = repo::count_chunks(&c, &source_id)?;
        let status = if chunks.is_empty() { "draft" } else { "ready" };
        let meta = if chunks.is_empty() {
            "empty note".to_string()
        } else {
            format!("{chunk_count} chunks · {chars} chars · from note")
        };
        repo::finalize_source(&c, &source_id, status, Some(&meta), Some(&text), None)?;

        let source = repo::get_source(&c, &source_id)?;
        Ok(IngestResult {
            source,
            chunk_count,
            chars,
            warning: None,
        })
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}
