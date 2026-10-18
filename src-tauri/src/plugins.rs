//! The plugins/ directory registry — the contributor entry point.
//!
//! Before this, the provider table, the material types and the Space-leader
//! menu were compiled in. A fork that wanted MiniMax chat or one more
//! "错题分析" generator had to rebuild the binary. Now `<data_dir>/plugins/*.json`
//! manifests are read at startup and merged into those surfaces:
//!
//!   providers       — OpenAI-compatible endpoints, resolved by llm/embed/verify
//!                     exactly like the built-in registry (fully functional)
//!   material_types  — presets for the existing generators: they run a built-in
//!                     `base_kind` with the plugin's `prompt` as custom steering
//!   leader_keys     — extra Space-leader entries that navigate to a view
//!
//! The design rule is "data, not code": a manifest cannot execute anything,
//! register a write tool, or shadow a built-in. Plugin provider ids that
//! collide with the static registry are rejected with a visible error, and a
//! malformed manifest is reported by name in Settings rather than silently
//! ignored.
//!
//! Manifest shape (a single JSON object per file):
//! {
//!   "name": "my-pack",
//!   "providers": [{ "id": "minimax", "label": "MiniMax",
//!                   "base_url": "https://api.minimax.chat/v1",
//!                   "key_note": "platform.minimaxi.com",
//!                   "chat_models": [["abab6.5s-chat", "abab6.5s"]],
//!                   "embed_model": "embo-01", "embed_dim": 1024 }],
//!   "material_types": [{ "id": "mistake-analysis", "label": "错题分析",
//!                        "base_kind": "guide", "prompt": "生成错题分析报告…" }],
//!   "leader_keys": [{ "key": "x", "label": "论文库", "detail": "…", "view": "literature" }]
//! }

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{OnceLock, RwLock};

/// One OpenAI-compatible endpoint contributed by a plugin. Same wire contract
/// as [`crate::providers::Provider`], but owned — it comes from a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    #[serde(default)]
    pub key_note: String,
    /// Embedding model to offer, when the endpoint has a public embeddings API.
    #[serde(default)]
    pub embed_model: Option<String>,
    /// Vector dimension the embeddings API returns (drives the sqlite-vec column).
    #[serde(default)]
    pub embed_dim: Option<i64>,
    #[serde(default)]
    pub chat_models: Vec<(String, String)>,
}

/// A preset for an existing generator: runs `base_kind` with `prompt` as the
/// user's custom steering. No new generator code is involved, so a plugin can
/// never produce malformed payloads — only differently-aimed prompts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMaterialType {
    pub id: String,
    pub label: String,
    /// Which built-in generator actually runs (quiz / flashcards / audio /
    /// cheatsheet / guide / matrix / mindmap / slideshow / infographic).
    pub base_kind: String,
    /// The steering text, appended as the generate call's custom prompt.
    pub prompt: String,
}

/// An extra Space-leader entry. Behavior is intentionally restricted to
/// "navigate to a view" — plugin keys cannot run commands or mutate state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginLeaderKey {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub detail: String,
    /// The view id LeaderPane opens (e.g. "literature", "agents").
    pub view: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plugin {
    pub name: String,
    /// Manifest filename, shown in Settings so the user can find what to edit.
    pub file: String,
    pub providers: Vec<PluginProvider>,
    pub material_types: Vec<PluginMaterialType>,
    pub leader_keys: Vec<PluginLeaderKey>,
}

#[derive(Debug, Default)]
struct Loaded {
    plugins: Vec<Plugin>,
    /// Human-readable problems (bad JSON, colliding ids) for the Settings card.
    errors: Vec<String>,
}

static LOADED: OnceLock<RwLock<Loaded>> = OnceLock::new();

fn slot() -> &'static RwLock<Loaded> {
    LOADED.get_or_init(|| RwLock::new(Loaded::default()))
}

/// Load every manifest in `<data_dir>/plugins/*.json`. Idempotent: the first
/// call wins, later calls are no-ops (the desktop app loads once at startup).
/// A missing plugins/ directory is not an error — it just means no plugins.
pub fn ensure_loaded(data_dir: &Path) {
    {
        let l = slot().read().unwrap();
        if !l.plugins.is_empty() || !l.errors.is_empty() {
            return;
        }
    }
    let mut loaded = Loaded::default();
    let dir = data_dir.join("plugins");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        // No plugins dir: record nothing, nothing is loaded. (A second call
        // would retry, which is fine — the read either succeeds or not.)
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    files.sort();
    for path in files {
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                loaded.errors.push(format!("{file}: 无法读取 ({e})"));
                continue;
            }
        };
        let m: Manifest = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                loaded.errors.push(format!("{file}: JSON 解析失败 ({e})"));
                continue;
            }
        };
        if let Err(e) = validate(&m) {
            loaded.errors.push(format!("{file}: {e}"));
            continue;
        }
        loaded.plugins.push(Plugin {
            name: m.name,
            file,
            providers: m.providers,
            material_types: m.material_types,
            leader_keys: m.leader_keys,
        });
    }
    *slot().write().unwrap() = loaded;
}

/// Internal deserialization shape (same fields as [`Plugin`], plus strictness).
#[derive(Deserialize, Debug)]
struct Manifest {
    name: String,
    #[serde(default)]
    providers: Vec<PluginProvider>,
    #[serde(default)]
    pub material_types: Vec<PluginMaterialType>,
    #[serde(default)]
    leader_keys: Vec<PluginLeaderKey>,
}

/// Reject the manifests that would misbehave before they are loaded. Cheap and
/// total: every check here is one a confused contributor would actually hit.
fn validate(m: &Manifest) -> Result<(), String> {
    if m.name.trim().is_empty() {
        return Err("manifest 缺少 name".into());
    }
    let mut ids = std::collections::HashSet::new();
    for p in &m.providers {
        if p.id.trim().is_empty() || p.base_url.trim().is_empty() {
            return Err(format!("provider {} 缺少 id 或 base_url", p.id));
        }
        if crate::providers::find(&p.id).is_some() {
            return Err(format!(
                "provider id '{}' 与内置注册表冲突 — 插件不能覆盖内置服务商",
                p.id
            ));
        }
        if !ids.insert(format!("p:{}", p.id)) {
            return Err(format!("provider id '{}' 在此清单中重复", p.id));
        }
        if p.chat_models.is_empty() && p.embed_model.is_none() {
            return Err(format!("provider '{}' 没有任何 chat_models 且无 embed_model", p.id));
        }
    }
    for t in &m.material_types {
        if t.id.trim().is_empty() || t.label.trim().is_empty() || t.prompt.trim().is_empty() {
            return Err(format!("material_type '{}' 缺少 id/label/prompt", t.id));
        }
        if !matches!(
            t.base_kind.as_str(),
            "quiz" | "flashcards" | "audio" | "cheatsheet" | "guide" | "matrix" | "mindmap" | "slideshow" | "infographic"
        ) {
            return Err(format!(
                "material_type '{}' 的 base_kind '{}' 不是内置生成器",
                t.id, t.base_kind
            ));
        }
        if !ids.insert(format!("m:{}", t.id)) {
            return Err(format!("material_type id '{}' 重复", t.id));
        }
    }
    for k in &m.leader_keys {
        if k.key.chars().count() != 1 {
            return Err(format!("leader_key '{}' 的 key 必须是单个字符", k.key));
        }
        if k.view.trim().is_empty() {
            return Err(format!("leader_key '{}' 缺少 view", k.key));
        }
        if !ids.insert(format!("l:{}", k.key)) {
            return Err(format!("leader key '{}' 重复", k.key));
        }
    }
    Ok(())
}

/// Every loaded plugin (empty when the directory is absent).
pub fn all() -> Vec<Plugin> {
    slot().read().unwrap().plugins.clone()
}

/// Manifest problems to surface in Settings.
pub fn errors() -> Vec<String> {
    slot().read().unwrap().errors.clone()
}

/// Look up a plugin provider by id — the fallback behind the static registry
/// in llm / embed / verify / list paths.
pub fn find_provider(id: &str) -> Option<PluginProvider> {
    all().iter().flat_map(|p| &p.providers).find(|p| p.id == id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(json: &str) -> Result<Manifest, String> {
        let m: Manifest = serde_json::from_str(json).map_err(|e| e.to_string())?;
        validate(&m)?;
        Ok(m)
    }

    #[test]
    fn a_full_manifest_parses_and_validates() {
        let m = manifest(
            r#"{
              "name": "pack",
              "providers": [{ "id": "minimax", "label": "MiniMax",
                "base_url": "https://api.minimax.chat/v1",
                "chat_models": [["abab6.5s-chat", "abab6.5s"]] }],
              "material_types": [{ "id": "mistakes", "label": "错题分析",
                "base_kind": "guide", "prompt": "分析错题" }],
              "leader_keys": [{ "key": "x", "label": "论文", "view": "literature" }]
            }"#,
        )
        .unwrap();
        assert_eq!(m.providers[0].id, "minimax");
        assert_eq!(m.material_types[0].base_kind, "guide");
        assert_eq!(m.leader_keys[0].view, "literature");
    }

    #[test]
    fn a_plugin_cannot_shadow_a_builtin_provider() {
        let err = manifest(
            r#"{ "name": "pack", "providers": [{ "id": "deepseek",
                 "label": "fake", "base_url": "https://evil.example/v1",
                 "chat_models": [["m", "m"]] }] }"#,
        )
        .unwrap_err();
        assert!(err.contains("冲突"), "got: {err}");
    }

    #[test]
    fn an_unknown_base_kind_is_rejected() {
        let err = manifest(
            r#"{ "name": "pack", "material_types": [{ "id": "x", "label": "X",
                 "base_kind": "sing_a_song", "prompt": "p" }] }"#,
        )
        .unwrap_err();
        assert!(err.contains("base_kind"), "got: {err}");
    }

    #[test]
    fn leader_keys_are_single_characters() {
        let err = manifest(
            r#"{ "name": "pack", "leader_keys": [{ "key": "ctrl+x", "label": "X", "view": "plan" }] }"#,
        )
        .unwrap_err();
        assert!(err.contains("单个字符"), "got: {err}");
    }

    #[test]
    fn empty_optional_sections_are_fine() {
        let m = manifest(r#"{ "name": "bare" }"#).unwrap();
        assert!(m.providers.is_empty() && m.material_types.is_empty() && m.leader_keys.is_empty());
    }
}
