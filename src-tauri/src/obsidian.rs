use crate::{error::{Error, Result}, repo};
use rusqlite::Connection;
use std::{io::Write, path::Path};

pub fn pages(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut pages = Vec::new();
    let mut index = String::from("# Cortex\n\n");
    for subject in repo::list_subjects(conn)? {
        let folder = format!("subject-{}", subject.id);
        index.push_str(&format!("- [[{folder}/index|{}]]\n", subject.name.replace(['[',']','|','\n'], " ")));
        let mut body = format!("# {}\n\n## Sources\n\n", subject.name);
        for source in repo::list_sources(conn, &subject.id)? {
            let full = repo::get_source(conn, &source.id)?;
            let name = format!("source-{}", source.id);
            body.push_str(&format!("- [[{name}|{}]]\n", source.name.replace(['[',']','|','\n'], " ")));
            let title = serde_json::to_string(&source.name)?;
            let origin = serde_json::to_string(&full.origin)?;
            pages.push((format!("{folder}/{name}.md"), format!("---\ntitle: {title}\ncortex_id: {}\norigin: {origin}\n---\n\n# {}\n\n{}\n", source.id, source.name, full.content.unwrap_or_default())));
        }
        body.push_str("\n## Notes\n\n");
        for note in repo::list_notes(conn, Some(&subject.id))? {
            let name = format!("note-{}", note.id);
            body.push_str(&format!("- [[{name}|{}]]\n", note.title.replace(['[',']','|','\n'], " ")));
            pages.push((format!("{folder}/{name}.md"), format!("---\ntitle: {}\ncortex_id: {}\n---\n\n# {}\n\n{}\n", serde_json::to_string(&note.title)?, note.id, note.title, note.body)));
        }
        pages.push((format!("{folder}/index.md"), body));
    }
    pages.push(("index.md".into(), index));
    Ok(pages)
}

pub fn write_pages(parent: &Path, pages: &[(String, String)]) -> Result<String> {
    if !parent.is_dir() { return Err(Error::Other("select an existing vault directory".into())); }
    let root = parent.join(format!("Cortex-{}", crate::db::new_id()));
    std::fs::create_dir(&root)?;
    for (relative, content) in pages {
        // All paths are generated internally. Reject traversal even for corrupted IDs.
        if Path::new(relative).components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return Err(Error::Other("invalid export path".into()));
        }
        let path = root.join(relative);
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        std::fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(content.as_bytes())?;
    }
    Ok(root.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_has_text_links_and_origin_without_secrets() {
        let state = crate::db::AppState::in_memory().unwrap();
        let c = state.db.lock().unwrap();
        let sid = repo::insert_subject(&c, "Biology", None, None, None).unwrap();
        let src = repo::insert_source(&c, &sid, None, "A paper", "txt", Some("https://example.org/paper")).unwrap();
        repo::finalize_source(&c, &src, "ready", None, Some("中文 evidence"), None).unwrap();
        repo::insert_note(&c, Some(&sid), None, "My note", "A comment").unwrap();
        repo::set_setting(&c, "api_key", "SECRET").unwrap();
        let pages = pages(&c).unwrap();
        let all = pages.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n");
        assert!(all.contains("中文 evidence")); assert!(all.contains("https://example.org/paper"));
        assert!(all.contains("A comment")); assert!(all.contains("[[source-")); assert!(!all.contains("SECRET"));
    }
}
