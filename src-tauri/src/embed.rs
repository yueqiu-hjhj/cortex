//! Embedding providers behind a single trait. Default is the deterministic
//! `stub` (no network, used until a real provider is configured). Locked design:
//! embed-once with a local model (Ollama `nomic-embed-text`, $0) is the cost
//! target; Gemini `text-embedding-004` is the BYOK cloud option. Provider is
//! chosen from the `settings` table key `embed_provider`.

use crate::error::{Error, Result};

pub const STUB_DIM: usize = 256;

pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dim(&self) -> usize;
    fn name(&self) -> &'static str;
}

/// Deterministic, dependency-free embedder. Hashes token n-grams into a fixed
/// vector so the same text always yields the same vector and similar texts
/// share dimensions. Good enough to exercise the whole pipeline offline.
pub struct StubEmbedder;

impl StubEmbedder {
    fn embed_one(text: &str) -> Vec<f32> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut v = vec![0.0f32; STUB_DIM];
        let lower = text.to_lowercase();
        for tok in lower.split(|c: char| !c.is_alphanumeric()) {
            if tok.is_empty() {
                continue;
            }
            let mut h = DefaultHasher::new();
            tok.hash(&mut h);
            let idx = (h.finish() as usize) % STUB_DIM;
            // sign from a second hash bit for some cancellation
            let sign = if (h.finish() >> 33) & 1 == 0 { 1.0 } else { -1.0 };
            v[idx] += sign;
        }
        // L2 normalize
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

impl Embedder for StubEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::embed_one(t)).collect())
    }
    fn dim(&self) -> usize {
        STUB_DIM
    }
    fn name(&self) -> &'static str {
        "stub"
    }
}

/// Gemini text-embedding-004 (BYOK). Requires `gemini_api_key` in settings.
pub struct GeminiEmbedder {
    pub api_key: String,
}

impl Embedder for GeminiEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // batchEmbedContents takes up to 100 contents per call — one round-trip
        // per 100 chunks instead of one per chunk.
        let client = reqwest::blocking::Client::new();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/text-embedding-004:batchEmbedContents?key={}",
            self.api_key
        );
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(100) {
            let requests: Vec<_> = batch
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "model": "models/text-embedding-004",
                        "content": { "parts": [{ "text": t }] }
                    })
                })
                .collect();
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "requests": requests }))
                .send()?;
            if !resp.status().is_success() {
                return Err(Error::Other(format!(
                    "gemini embed failed: {}",
                    resp.status()
                )));
            }
            let json: serde_json::Value = resp.json()?;
            let embeddings = json["embeddings"]
                .as_array()
                .ok_or_else(|| Error::Other("gemini: no embeddings".into()))?;
            if embeddings.len() != batch.len() {
                return Err(Error::Other(format!(
                    "gemini: expected {} embeddings, got {}",
                    batch.len(),
                    embeddings.len()
                )));
            }
            for e in embeddings {
                let vals = e["values"]
                    .as_array()
                    .ok_or_else(|| Error::Other("gemini: no embedding values".into()))?;
                out.push(
                    vals.iter()
                        .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                        .collect(),
                );
            }
        }
        Ok(out)
    }
    fn dim(&self) -> usize {
        768
    }
    fn name(&self) -> &'static str {
        "gemini"
    }
}

/// Ollama nomic-embed-text on localhost (offline, $0). Honors `ollama_url`.
pub struct OllamaEmbedder {
    pub base_url: String,
    pub model: String,
}

impl OllamaEmbedder {
    /// Legacy one-text-per-request endpoint, kept as a fallback for Ollama
    /// versions that predate the batched `/api/embed`.
    fn embed_singly(&self, client: &reqwest::blocking::Client, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/api/embeddings", self.base_url.trim_end_matches('/'));
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            let body = serde_json::json!({ "model": self.model, "prompt": t });
            let resp = client.post(&url).json(&body).send()?;
            if !resp.status().is_success() {
                return Err(Error::Other(format!(
                    "ollama embed failed: {}",
                    resp.status()
                )));
            }
            let json: serde_json::Value = resp.json()?;
            let vals = json["embedding"]
                .as_array()
                .ok_or_else(|| Error::Other("ollama: no embedding".into()))?;
            out.push(
                vals.iter()
                    .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                    .collect(),
            );
        }
        Ok(out)
    }
}

impl Embedder for OllamaEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let client = reqwest::blocking::Client::new();
        // Batched endpoint (Ollama ≥0.1.45): all texts in one request.
        let url = format!("{}/api/embed", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({ "model": self.model, "input": texts });
        let resp = client.post(&url).json(&body).send()?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return self.embed_singly(&client, texts);
        }
        if !resp.status().is_success() {
            return Err(Error::Other(format!(
                "ollama embed failed: {}",
                resp.status()
            )));
        }
        let json: serde_json::Value = resp.json()?;
        let embeddings = json["embeddings"]
            .as_array()
            .ok_or_else(|| Error::Other("ollama: no embeddings".into()))?;
        if embeddings.len() != texts.len() {
            return Err(Error::Other(format!(
                "ollama: expected {} embeddings, got {}",
                texts.len(),
                embeddings.len()
            )));
        }
        embeddings
            .iter()
            .map(|e| {
                e.as_array()
                    .map(|vals| {
                        vals.iter()
                            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                            .collect::<Vec<f32>>()
                    })
                    .ok_or_else(|| Error::Other("ollama: bad embedding shape".into()))
            })
            .collect()
    }
    fn dim(&self) -> usize {
        768
    }
    fn name(&self) -> &'static str {
        "ollama"
    }
}

/// One OpenAI-compatible embedder for every provider in the registry.
///
/// The app had *no* OpenAI-compatible embedding path at all before this — only
/// Gemini, Ollama and on-device ONNX — which meant the whole Chinese-provider
/// long tail was unusable for retrieval even after their chat models worked.
pub struct OpenAiCompatEmbedder {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub dim: usize,
}

impl Embedder for OpenAiCompatEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let mut out = Vec::with_capacity(texts.len());
        // 64 per request: providers differ in batch limits and payload size, and
        // 64 keeps a long chunk list from blowing past the common ones.
        for batch in texts.chunks(64) {
            let resp = client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&serde_json::json!({ "model": self.model, "input": batch }))
                .send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().unwrap_or_default();
                return Err(Error::Other(format!(
                    "{} embed failed: {status} {}",
                    self.model,
                    body.chars().take(200).collect::<String>()
                )));
            }
            let json: serde_json::Value = resp.json()?;
            let rows = json["data"]
                .as_array()
                .ok_or_else(|| Error::Other("embeddings: no data array".into()))?;
            if rows.len() != batch.len() {
                return Err(Error::Other(format!(
                    "embeddings: expected {} vectors, got {}",
                    batch.len(),
                    rows.len()
                )));
            }
            for row in rows {
                let vals = row["embedding"]
                    .as_array()
                    .ok_or_else(|| Error::Other("embeddings: row has no embedding".into()))?;
                let v: Vec<f32> = vals
                    .iter()
                    .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                if v.len() != self.dim {
                    return Err(Error::Other(format!(
                        "embeddings: expected dim {}, got {}",
                        self.dim,
                        v.len()
                    )));
                }
                // L2-normalise so cosine distance in SQLite means direction only.
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                out.push(if norm > 0.0 {
                    v.iter().map(|x| x / norm).collect()
                } else {
                    v
                });
            }
        }
        Ok(out)
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn name(&self) -> &'static str {
        "openai-compat"
    }
}

/// Build an embedder from settings. Falls back to the stub on anything missing,
/// so the app always works offline with zero configuration.
///
/// `keys` is the full key set (`llm::Keys`) rather than a single Gemini key:
/// the registry's providers each need their own, and threading them through
/// one struct keeps the signature from growing an argument per provider.
pub fn from_settings(
    provider: &str,
    keys: &crate::llm::Keys,
    ollama_url: Option<&str>,
    data_dir: Option<&std::path::Path>,
) -> Box<dyn Embedder> {
    // Registry providers resolve first — the bespoke arms below cover the ones
    // with a non-OpenAI wire format.
    if let Some(p) = crate::providers::find(provider) {
        if let (Some(k), Some(model)) = (keys.for_provider(p.id), p.embed_model) {
            return Box::new(OpenAiCompatEmbedder {
                base_url: p.base_url.to_string(),
                api_key: k.to_string(),
                model: model.to_string(),
                dim: 1024,
            });
        }
        return Box::new(StubEmbedder);
    }
    match provider {
        "gemini" => match keys.for_provider("gemini") {
            Some(k) => Box::new(GeminiEmbedder {
                api_key: k.to_string(),
            }),
            None => Box::new(StubEmbedder),
        },
        "ollama" => Box::new(OllamaEmbedder {
            base_url: ollama_url.unwrap_or("http://localhost:11434").to_string(),
            model: "nomic-embed-text".to_string(),
        }),
        "onnx" => match data_dir {
            Some(d) => Box::new(crate::embed_onnx::OnnxEmbedder::new(d.to_path_buf())),
            None => Box::new(StubEmbedder),
        },
        _ => Box::new(StubEmbedder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_is_deterministic_and_normalized() {
        let e = StubEmbedder;
        let a = e.embed(&["recursion base case".into()]).unwrap();
        let b = e.embed(&["recursion base case".into()]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].len(), STUB_DIM);
        let norm: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn similar_text_scores_higher_than_unrelated() {
        use crate::vector::cosine;
        let e = StubEmbedder;
        let q = &e.embed(&["dynamic programming memoization".into()]).unwrap()[0];
        let close = &e
            .embed(&["memoization in dynamic programming".into()])
            .unwrap()[0];
        let far = &e.embed(&["the cat sat on the mat".into()]).unwrap()[0];
        assert!(cosine(q, close) > cosine(q, far));
    }
}
