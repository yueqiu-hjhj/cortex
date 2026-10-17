//! Provider registry — the OpenAI-compatible long tail.
//!
//! Before this, every provider was a match arm: `gemini`, `openrouter`,
//! `openai`, `claude`, `custom`, `ollama`. That is fine for six and impossible
//! for thirty, and it is why the app could not talk to DeepSeek, Qwen, GLM or
//! Kimi even though every one of them speaks the OpenAI API verbatim.
//!
//! So the long tail is data, not code: one static table, and `from_spec` /
//! `from_settings` consult it. Adding a provider is now one line, and — more
//! importantly — adding one cannot break another.
//!
//! Only genuinely OpenAI-compatible endpoints belong here. Anything with a
//! bespoke wire format (Gemini, Claude) stays in its own module; putting it in
//! this table would only produce confusing 404s.

/// One OpenAI-compatible provider.
pub struct Provider {
    /// Spec prefix and settings suffix: `deepseek:deepseek-chat`, key setting
    /// `<id>_api_key`.
    pub id: &'static str,
    pub label: &'static str,
    /// OpenAI-compatible root, **including** the version segment when the
    /// provider has one. The client appends `/chat/completions`, `/embeddings`
    /// or `/models`.
    pub base_url: &'static str,
    /// Embedding model to offer, when the provider has a public embeddings API.
    /// `None` means it is chat-only — offering it for retrieval would just 404.
    pub embed_model: Option<&'static str>,
    /// Where to get a key, shown in Settings.
    pub key_note: &'static str,
    /// Curated chat models: `(id, label)`.
    pub chat_models: &'static [(&'static str, &'static str)],
}

/// The registry. Ordered by how a Chinese-first user would rank them.
pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "deepseek",
        label: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        embed_model: None, // no public embeddings API yet
        key_note: "platform.deepseek.com/api_keys",
        chat_models: &[
            ("deepseek-chat", "DeepSeek V3 — 主力，便宜"),
            ("deepseek-reasoner", "DeepSeek R1 — 推理"),
        ],
    },
    Provider {
        id: "dashscope",
        label: "通义千问 Qwen",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        embed_model: Some("text-embedding-v3"),
        key_note: "bailian.console.aliyun.com → API-KEY",
        chat_models: &[
            ("qwen-plus", "Qwen Plus — 主力"),
            ("qwen-turbo", "Qwen Turbo — 最快"),
            ("qwen-max", "Qwen Max — 最强"),
            ("qwen-long", "Qwen Long — 长上下文"),
        ],
    },
    Provider {
        id: "zhipu",
        label: "智谱 GLM",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        embed_model: Some("embedding-3"),
        key_note: "open.bigmodel.cn → API keys",
        chat_models: &[
            ("glm-4-plus", "GLM-4 Plus — 主力"),
            ("glm-4-air", "GLM-4 Air — 便宜"),
            ("glm-4-flash", "GLM-4 Flash — 免费"),
            ("glm-4-long", "GLM-4 Long — 长上下文"),
        ],
    },
    Provider {
        id: "moonshot",
        label: "Moonshot Kimi",
        base_url: "https://api.moonshot.cn/v1",
        embed_model: None,
        key_note: "platform.moonshot.cn/console",
        chat_models: &[
            ("moonshot-v1-8k", "Moonshot 8K"),
            ("moonshot-v1-32k", "Moonshot 32K"),
            ("moonshot-v1-128k", "Moonshot 128K — 长上下文"),
        ],
    },
    Provider {
        id: "siliconflow",
        label: "硅基流动 SiliconFlow",
        base_url: "https://api.siliconflow.cn/v1",
        embed_model: Some("BAAI/bge-m3"),
        key_note: "siliconflow.cn → API 密钥",
        chat_models: &[
            ("Qwen/Qwen2.5-7B-Instruct", "Qwen2.5 7B — 便宜"),
            ("deepseek-ai/DeepSeek-V3", "DeepSeek V3"),
            ("deepseek-ai/DeepSeek-R1", "DeepSeek R1 — 推理"),
            ("Pro/Qwen/Qwen2.5-7B-Instruct", "Qwen2.5 7B（Pro）"),
        ],
    },
    Provider {
        id: "volcengine",
        label: "火山引擎 豆包",
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        embed_model: None,
        key_note: "console.volcengine.com/ark → API Key",
        chat_models: &[
            ("doubao-pro-32k", "Doubao Pro 32K"),
            ("doubao-lite-32k", "Doubao Lite 32K — 便宜"),
        ],
    },
    Provider {
        id: "stepfun",
        label: "阶跃星辰 StepFun",
        base_url: "https://api.stepfun.com/v1",
        embed_model: None,
        key_note: "platform.stepfun.com → API Keys",
        chat_models: &[("step-2-16k", "Step 2 16K"), ("step-1-8k", "Step 1 8K")],
    },
    Provider {
        id: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        embed_model: Some("text-embedding-3-small"),
        key_note: "platform.openai.com/api-keys",
        chat_models: &[
            ("gpt-4o-mini", "GPT-4o mini — cheap"),
            ("gpt-5-mini", "GPT-5 mini — cheap + smart"),
            ("gpt-4o", "GPT-4o — balanced"),
            ("gpt-5", "GPT-5 — frontier"),
            ("o3-mini", "o3-mini — cheap reasoning"),
        ],
    },
    Provider {
        id: "mistral",
        label: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        embed_model: Some("mistral-embed"),
        key_note: "console.mistral.ai/api-keys",
        chat_models: &[
            ("mistral-small-latest", "Mistral Small — cheap"),
            ("mistral-large-latest", "Mistral Large — flagship"),
        ],
    },
    Provider {
        id: "xai",
        label: "xAI Grok",
        base_url: "https://api.x.ai/v1",
        embed_model: None,
        key_note: "console.x.ai",
        chat_models: &[("grok-2-latest", "Grok 2"), ("grok-3-latest", "Grok 3")],
    },
    Provider {
        id: "groq",
        label: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        embed_model: None,
        key_note: "console.groq.com/keys",
        chat_models: &[
            ("llama-3.3-70b-versatile", "Llama 3.3 70B"),
            ("deepseek-r1-distill-llama-70b", "DeepSeek R1 Distill 70B"),
        ],
    },
    Provider {
        id: "together",
        label: "Together AI",
        base_url: "https://api.together.xyz/v1",
        embed_model: Some("BAAI/bge-base-en-v1.5"),
        key_note: "api.together.xyz/settings/api-keys",
        chat_models: &[
            ("Qwen/Qwen2.5-72B-Instruct-Turbo", "Qwen2.5 72B Turbo"),
            ("deepseek-ai/DeepSeek-V3", "DeepSeek V3"),
        ],
    },
];

/// Look up a provider by spec id.
pub fn find(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// The settings key that holds a provider's API key.
pub fn key_setting(id: &str) -> String {
    format!("{id}_api_key")
}

/// Providers that can also produce embeddings — offered in the Embedding
/// dropdown. A chat-only provider in that list would fail at index time, which
/// is the kind of bug that only shows up hours later on a full-library embed.
pub fn embedding_providers() -> impl Iterator<Item = &'static Provider> {
    PROVIDERS.iter().filter(|p| p.embed_model.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_urls_look_right() {
        let mut seen = Vec::new();
        for p in PROVIDERS {
            assert!(!seen.contains(&p.id), "duplicate provider id {}", p.id);
            seen.push(p.id);
            assert!(p.base_url.starts_with("https://"), "{} is not https", p.id);
            assert!(p.base_url.ends_with("/v1") || p.base_url.contains("/v3") || p.base_url.contains("/v4"),
                "{} base_url is missing a version segment: {}", p.id, p.base_url);
            assert!(!p.chat_models.is_empty(), "{} has no chat models", p.id);
            assert!(!p.key_note.is_empty(), "{} has no key note", p.id);
        }
    }

    #[test]
    fn lookup_and_key_setting() {
        assert_eq!(find("deepseek").unwrap().label, "DeepSeek");
        assert!(find("nope").is_none());
        assert_eq!(key_setting("zhipu"), "zhipu_api_key");
    }

    #[test]
    fn embedding_providers_all_declare_a_model() {
        for p in embedding_providers() {
            assert!(p.embed_model.is_some(), "{} has no embed model", p.id);
        }
        // DeepSeek is chat-only, and must not be offered for retrieval.
        assert!(find("deepseek").unwrap().embed_model.is_none());
        assert!(!embedding_providers().any(|p| p.id == "deepseek"));
    }
}
