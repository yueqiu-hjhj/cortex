//! Local ONNX embedding provider. Zero-config means the desktop app can embed
//! chunks without any external service: we download a multilingual model
//! (intfloat/multilingual-e5-small, int8 quantized) on demand from
//! HuggingFace, then run it in-process via the ONNX Runtime.
//!
//! Three states the app cares about:
//!   - NotPresent     — no model on disk, embeddings fall back to the stub
//!   - Downloading(p) — in progress; emitted as a `onnx-progress` event
//!   - Ready          — session + tokenizer loaded, embed() works offline
//!
//! The actual ONNX inference runs lazily inside `OnnxEmbedder::embed`; the
//! session itself is built once and parked in a `OnceLock` so repeated
//! embeds share it (ORT sessions are heavy to construct).

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// HuggingFace repo + files we materialise on disk.
const MODEL_REPO: &str = "Xenova/multilingual-e5-small";
const MODEL_FILE: &str = "onnx/model_quantized.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
/// Approximate int8 ONNX size, used for the progress bar only.
const APPROX_MODEL_BYTES: u64 = 118_000_000;

/// Where everything lands: `<data_dir>/models/onnx/`.
pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("onnx")
}

fn model_path(data_dir: &Path) -> PathBuf {
    models_dir(data_dir).join("model_quantized.onnx")
}
fn tokenizer_path(data_dir: &Path) -> PathBuf {
    models_dir(data_dir).join("tokenizer.json")
}

/// Coarse status of the local ONNX model. The frontend uses this to decide
/// whether to show the "Download ONNX embeddings (~118 MB)" prompt.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Status {
    NotPresent,
    Downloading { downloaded: u64, total: u64 },
    Ready { bytes_on_disk: u64 },
    Failed { reason: String },
}

pub fn status(data_dir: &Path) -> Status {
    let m = model_path(data_dir);
    let t = tokenizer_path(data_dir);
    if let Some(s) = DOWNLOAD_STATE.get() {
        let s = s.lock().unwrap();
        if s.failed.is_some() {
            return Status::Failed { reason: s.failed.clone().unwrap_or_default() };
        }
        if s.downloaded > 0 && s.downloaded < s.total {
            return Status::Downloading { downloaded: s.downloaded, total: s.total };
        }
    }
    if m.exists() && t.exists() {
        let bytes = std::fs::metadata(&m).map(|m| m.len()).unwrap_or(0)
            + std::fs::metadata(&t).map(|m| m.len()).unwrap_or(0);
        Status::Ready { bytes_on_disk: bytes }
    } else {
        Status::NotPresent
    }
}

#[derive(Default)]
struct DownloadTracker {
    downloaded: u64,
    total: u64,
    failed: Option<String>,
}

static DOWNLOAD_STATE: OnceLock<Mutex<DownloadTracker>> = OnceLock::new();

fn tracker() -> &'static Mutex<DownloadTracker> {
    DOWNLOAD_STATE.get_or_init(|| Mutex::new(DownloadTracker::default()))
}

/// Download sources, in try order: huggingface directly, then the CN mirror —
/// huggingface.co is unreachable on many CN networks, where hf-mirror.com works.
const MODEL_HOSTS: &[&str] = &["https://huggingface.co", "https://hf-mirror.com"];

fn download_from_hosts<F: FnMut(u64, u64)>(
    client: &reqwest::blocking::Client,
    file: &str,
    mut on_progress: F,
) -> Result<Vec<u8>> {
    let mut last: Option<String> = None;
    for host in MODEL_HOSTS {
        let url = format!("{host}/{MODEL_REPO}/resolve/main/{file}");
        match download_with_progress(client, &url, &mut on_progress) {
            Ok(bytes) => return Ok(bytes),
            Err(e) => last = Some(format!("{host}: {e}")),
        }
    }
    Err(Error::Other(format!(
        "download failed from all mirrors ({}): {}",
        MODEL_HOSTS.join(", "),
        last.unwrap_or_default()
    )))
}

/// Kick off a download. Returns the file paths once finished. Reports via the
/// `tracker()` so the frontend can poll `onnx_status`, and also fires the
/// `on_progress` callback so callers (the Tauri command) can emit progress
/// events to the frontend.
pub fn ensure_model<F>(data_dir: &Path, mut on_progress: F) -> Result<(PathBuf, PathBuf)>
where
    F: FnMut(u64, u64),
{
    let dir = models_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| Error::Other(format!(
        "create {}: {e}", dir.display()
    )))?;
    let mp = model_path(data_dir);
    let tp = tokenizer_path(data_dir);

    if mp.exists() && tp.exists() {
        return Ok((mp, tp));
    }

    {
        let mut t = tracker().lock().unwrap();
        t.downloaded = 0;
        t.total = APPROX_MODEL_BYTES;
        t.failed = None;
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Other(format!("reqwest: {e}")))?;

    if !mp.exists() {
        match download_from_hosts(&client, MODEL_FILE, |d, total| {
            let mut t = tracker().lock().unwrap();
            t.downloaded = d;
            if total > 0 { t.total = total; }
            on_progress(d, t.total);
        }) {
            Ok(bytes) => std::fs::write(&mp, &bytes).map_err(|e| Error::Other(format!(
                "write {}: {e}", mp.display()
            )))?,
            Err(e) => {
                let reason = format!("model download: {e}");
                tracker().lock().unwrap().failed = Some(reason.clone());
                return Err(Error::Other(reason));
            }
        }
    }

    {
        match download_from_hosts(&client, TOKENIZER_FILE, |_, _| {}) {
            Ok(bytes) => std::fs::write(&tp, &bytes).map_err(|e| Error::Other(format!(
                "write {}: {e}", tp.display()
            )))?,
            Err(e) => {
                let reason = format!("tokenizer download: {e}");
                tracker().lock().unwrap().failed = Some(reason.clone());
                return Err(Error::Other(reason));
            }
        }
    }

    Ok((mp, tp))
}

fn download_with_progress<F: FnMut(u64, u64)>(
    client: &reqwest::blocking::Client,
    url: &str,
    mut on_progress: F,
) -> Result<Vec<u8>> {
    let mut resp = client.get(url).send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Other(format!("GET {url}: {e}")))?;
    let total = resp.content_length().unwrap_or(0);
    on_progress(0, total);
    let mut buf = Vec::with_capacity(total as usize);
    use std::io::Read;
    let mut chunk = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    loop {
        let n = resp.read(&mut chunk).map_err(|e| Error::Other(format!("read: {e}")))?;
        if n == 0 { break; }
        buf.extend_from_slice(&chunk[..n]);
        downloaded += n as u64;
        on_progress(downloaded, total);
    }
    Ok(buf)
}

/// Embedder that loads the local ONNX model on first embed and reuses it.
pub struct OnnxEmbedder {
    data_dir: PathBuf,
    session: OnceLock<std::result::Result<Arc<Mutex<ort::session::Session>>, String>>,
    tokenizer: OnceLock<std::result::Result<Arc<tokenizers::Tokenizer>, String>>,
}

impl OnnxEmbedder {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            session: OnceLock::new(),
            tokenizer: OnceLock::new(),
        }
    }

    fn session(&self) -> Result<&Arc<Mutex<ort::session::Session>>> {
        self.session.get_or_init(|| {
            let (mp, _tp) = ensure_model(&self.data_dir, |_, _| {}).map_err(|e| e.to_string())?;
            let env = ort::init().build().map_err(|e| e.to_string())?;
            let mut builder = ort::session::Session::builder(&env).map_err(|e| e.to_string())?;
            let s = builder.commit_from_file(&mp).map_err(|e| e.to_string())?;
            Ok(Arc::new(Mutex::new(s)))
        })
        .as_ref()
        .map_err(|e| Error::Other(format!("onnx session: {e}")))
    }

    fn tokenizer(&self) -> Result<&Arc<tokenizers::Tokenizer>> {
        self.tokenizer.get_or_init(|| {
            let (_mp, tp_path) = ensure_model(&self.data_dir, |_, _| {}).map_err(|e| e.to_string())?;
            let t = tokenizers::Tokenizer::from_file(&tp_path).map_err(|e| e.to_string())?;
            Ok(Arc::new(t))
        })
        .as_ref()
        .map_err(|e| Error::Other(format!("onnx tokenizer: {e}")))
    }
}

impl crate::embed::Embedder for OnnxEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let tokenizer = self.tokenizer()?.clone();
        let session_arc = self.session()?.clone();

        // e5 models expect a "passage: " prefix for indexing (per the model
        // card). We use passage for all calls so the same embedder covers
        // both retrieval-time (queries would be "query:") and indexing; the
        // semantic difference is tiny for our chunk sizes.
        let inputs: Vec<String> = texts.iter()
            .map(|t| format!("passage: {t}"))
            .collect();
        let input_refs: Vec<&str> = inputs.iter().map(|s| s.as_str()).collect();

        let encodings = tokenizer.encode_batch(input_refs, true)
            .map_err(|e| Error::Other(format!("tokenize: {e}")))?;

        let max_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
        let mut ids: Vec<i64> = Vec::with_capacity(encodings.len() * max_len);
        let mut mask: Vec<i64> = Vec::with_capacity(encodings.len() * max_len);
        let mut type_ids: Vec<i64> = Vec::with_capacity(encodings.len() * max_len);
        for enc in &encodings {
            let pad_len = max_len - enc.get_ids().len();
            ids.extend(enc.get_ids().iter().copied().map(i64::from));
            mask.extend(enc.get_attention_mask().iter().copied().map(i64::from));
            type_ids.extend(enc.get_type_ids().iter().copied().map(i64::from));
            for _ in 0..pad_len {
                ids.push(0);
                mask.push(0);
                type_ids.push(0);
            }
        }

        let shape = vec![encodings.len() as i64, max_len as i64];
        let ids_tensor = ort::value::Tensor::from_array((shape.clone(), ids.into_boxed_slice()))
            .map_err(|e| Error::Other(format!("ids tensor: {e}")))?;
        let mask_owned = mask.clone();
            let mask_tensor = ort::value::Tensor::from_array((shape.clone(), mask.into_boxed_slice()))
            .map_err(|e| Error::Other(format!("mask tensor: {e}")))?;
        let type_tensor = ort::value::Tensor::from_array((shape, type_ids.into_boxed_slice()))
            .map_err(|e| Error::Other(format!("type tensor: {e}")))?;

        let mut session = session_arc.lock().unwrap();
        let outputs = session.run(ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
            "token_type_ids" => type_tensor,
        ]).map_err(|e| Error::Other(format!("onnx run: {e}")))?;

        // Mean-pool with attention_mask + L2 normalise. Pulled straight from
        // the e5 model card. Output is the first tensor (last_hidden_state).
        let (out_shape, out_data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Other(format!("extract: {e}")))?;
        let dims: &[i64] = out_shape;
        let seq_len = *dims.get(1).ok_or_else(|| Error::Other("bad output shape".into()))? as usize;
        let hidden = *dims.get(2).ok_or_else(|| Error::Other("bad output shape".into()))? as usize;
        let batch = encodings.len();
        let mut out = Vec::with_capacity(batch);
        for b in 0..batch {
            let mut v = vec![0f32; hidden];
            for s in 0..seq_len {
                let m = mask_owned[b * seq_len + s];
                if m == 0 { continue; }
                for h in 0..hidden {
                    v[h] += out_data[b * seq_len * hidden + s * hidden + h];
                }
            }
            let mut denom: f32 = 0.0;
            for s in 0..seq_len { denom += mask_owned[b * seq_len + s] as f32; }
            if denom > 0.0 {
                for x in &mut v { *x /= denom; }
            }
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v { *x /= norm; }
            }
            out.push(v);
        }
        Ok(out)
    }

    fn dim(&self) -> usize {
        384
    }

    fn name(&self) -> &'static str {
        "onnx"
    }
}
