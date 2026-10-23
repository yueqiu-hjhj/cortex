//! LLM providers behind one trait, mirroring embed.rs. Default is an offline
//! `stub` that returns a clearly-marked placeholder so the app works with zero
//! config; Gemini `generateContent` (BYOK) is the real path. Per-task model is
//! read from settings as `model_<task>` = "provider:model" (e.g. "gemini:gemini-2.5-flash").

use crate::error::{Error, Result};
use crate::models::CardDraft;

/// What a provider actually sent back, before Cortex cleans it up.
///
/// `text` is what the pipeline consumes (reasoning blocks stripped); `content`
/// is the untouched string, because a prompt lab that only ever shows the
/// cleaned output cannot explain why an answer came out empty or truncated.
#[derive(Debug, Clone, Default)]
pub struct RawReply {
    /// Content exactly as the provider returned it (may include `<think>`).
    pub content: String,
    /// The cleaned text the rest of Cortex uses.
    pub text: String,
    /// Why generation stopped, when the provider says (`stop` / `length` / …).
    pub finish_reason: Option<String>,
    /// Provider-reported usage, verbatim — token counts differ per provider and
    /// inventing a normalized shape would hide exactly that.
    pub usage: Option<serde_json::Value>,
}

/// Terminal state of one generation, borrowed from KnowNote's answer
/// provenance: the UI must be able to say WHY an answer looks the way it
/// does. A half-answer cut off by the token cap is still worth reading (and
/// continuable), an answer the safety filter stopped needs a different
/// warning, and "the model returned nothing at all" is its own thing —
/// showing all three as an identical bubble hides exactly the difference
/// the learner needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerState {
    /// Finished normally with usable text.
    Completed,
    /// Hit the output cap mid-answer — partial text kept, continuation invited.
    Truncated,
    /// Stopped by a safety/policy filter (partial text may exist).
    Blocked,
    /// The user pressed stop (set by the caller, never by the classifier).
    Aborted,
    /// Nothing usable came back (error or empty output).
    Failed,
}

impl AnswerState {
    /// Canonical wire string — what lands in the DB and in logs.
    pub fn as_str(self) -> &'static str {
        match self {
            AnswerState::Completed => "completed",
            AnswerState::Truncated => "truncated",
            AnswerState::Blocked => "blocked",
            AnswerState::Aborted => "aborted",
            AnswerState::Failed => "failed",
        }
    }
}

/// finishReason vocabularies differ per provider, so the classifier is a
/// table, not a guess:
/// - OpenAI-compatible: `length` / `content_filter`
/// - Gemini: `MAX_TOKENS` / `SAFETY` / `RECITATION` / `PROHIBITED_CONTENT` / `BLOCKLIST`
/// - Anthropic: `max_tokens` / `refusal`
/// Matching is case-insensitive. An unknown reason with usable text counts
/// as completed — a provider we don't recognize must not downgrade a real
/// answer; a provider we don't recognize with NO text is a failure, which
/// the empty-text branch handles.
const TRUNCATION_REASONS: &[&str] = &["length", "max_tokens"];
const BLOCK_REASONS: &[&str] = &[
    "content_filter",
    "safety",
    "recitation",
    "prohibited_content",
    "blocklist",
    "refusal",
];

/// Map one finished generation onto [`AnswerState`]. `reason` is the
/// provider's finishReason (`None` = provider doesn't report one, e.g. the
/// offline stub); `text` is the cleaned output the user would see.
pub fn classify_answer(reason: Option<&str>, text: &str) -> AnswerState {
    let reason = reason.map(|r| r.trim().to_ascii_lowercase());
    let known = reason.as_deref().filter(|r| !r.is_empty());
    if text.trim().is_empty() {
        // Empty output: a named filter explains it (blocked), anything else —
        // including an exhausted token budget spent on hidden reasoning — is
        // a failure. Either way there is no answer to show.
        return match known {
            Some(r) if BLOCK_REASONS.contains(&r) => AnswerState::Blocked,
            _ => AnswerState::Failed,
        };
    }
    match known {
        Some(r) if TRUNCATION_REASONS.contains(&r) => AnswerState::Truncated,
        Some(r) if BLOCK_REASONS.contains(&r) => AnswerState::Blocked,
        _ => AnswerState::Completed,
    }
}

pub trait Llm: Send + Sync {
    fn complete(&self, system: &str, user: &str) -> Result<String>;
    fn name(&self) -> String;
    /// One completion with the raw payload attached. Default: no extra metadata
    /// — only providers that expose it (OpenAI-compatible endpoints) override.
    fn complete_raw(&self, system: &str, user: &str) -> Result<RawReply> {
        let text = self.complete(system, user)?;
        Ok(RawReply { content: text.clone(), text, finish_reason: None, usage: None })
    }
    /// Stream one completion: hand each NEW piece of visible text to `on_delta`
    /// as it arrives, and return the same [`RawReply`] `complete_raw` would.
    /// `on_delta` returns `false` when the caller wants generation to STOP (the
    /// chat panel's Stop button) — providers must honour that between chunks and
    /// return what they have so far.
    ///
    /// Default: no incremental transport, so the finished answer is handed over
    /// as one delta. Every provider therefore stays correct; only the ones that
    /// override this are actually incremental.
    fn complete_stream(
        &self,
        system: &str,
        user: &str,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<RawReply> {
        let reply = self.complete_raw(system, user)?;
        if !reply.text.is_empty() {
            let _ = on_delta(&reply.text);
        }
        Ok(reply)
    }
    /// Cap output tokens for this provider's request. Wiring the per-task token
    /// budget here is what stops OpenRouter from defaulting to a huge max_tokens
    /// (its model default) and 402-ing when the key's credit limit can't cover it.
    /// Default: no-op (providers that don't store it ignore the cap).
    fn set_max_tokens(&mut self, _max: u32) {}
    /// OCR/transcribe images (each `(mime, base64)`) to Markdown text. Default:
    /// unsupported — only vision-capable providers override this.
    fn ocr(&self, _images: &[(String, String)]) -> Result<String> {
        Err(Error::Unsupported(format!(
            "{} can't read images — pick a vision model (e.g. an OpenRouter/Gemini multimodal model) in Settings",
            self.name()
        )))
    }
    /// Vision Q&A over images with a CUSTOM prompt — unlike `ocr`, whose
    /// prompt is baked in. Used by 2.6 figure explanation. Default: unsupported.
    fn vision(&self, _images: &[(String, String)], _system: &str, _user: &str) -> Result<String> {
        Err(Error::Unsupported(format!(
            "{} can't read images — pick a vision model (e.g. an OpenRouter/Gemini multimodal model) in Settings",
            self.name()
        )))
    }
    /// Extract editable question/answer flashcards from one or more images.
    fn extract_cards(&self, _images: &[(String, String)]) -> Result<Vec<CardDraft>> {
        Err(Error::Unsupported(format!(
            "{} cannot extract flashcards — pick a vision model in Settings",
            self.name()
        )))
    }
    /// Generate an image from a text prompt; returns a `data:image/...;base64,…`
    /// URL. Only image-capable models (e.g. Gemini "nano-banana") override this.
    fn gen_image(&self, _prompt: &str) -> Result<String> {
        Err(Error::Unsupported(format!(
            "{} can't generate images — use an image model like openrouter:google/gemini-2.5-flash-image",
            self.name()
        )))
    }
}

/// Standard base64 (with padding). Inline to avoid a dependency just for OCR
/// data URLs / inline image payloads.
pub fn b64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

const OCR_CARDS_PROMPT: &str = r#"从图片中提取适合间隔重复学习的问答卡。只输出 JSON 对象，格式为 {"cards":[{"front":"问题","back":"答案"}]}。要求：提取 3 到 15 张高质量卡片；front 是明确、可独立回答的问题或术语；back 是忠实、简洁的答案；保留图片原语言；合并重复内容；跳过页眉页脚、装饰文字和 OCR 噪声；不要添加图片中没有的知识。"#;
const OCR_PROMPT: &str = "Transcribe ALL text in these page image(s) EXACTLY, in reading order — \
including HANDWRITTEN text (photographed notes, whiteboards): transcribe messy writing as \
faithfully as you can and mark genuinely unreadable words as [illegible]. \
Preserve headings, lists, tables (as Markdown tables), and math. Do not summarise, \
translate, or add commentary — output ONLY the transcribed text as Markdown.";

/// Offline placeholder — no network, no key. Produces something usable so the
/// UI flow works, while telling the user to connect a model for real output.
#[allow(dead_code)]
pub struct StubLlm;

impl Llm for StubLlm {
    fn complete(&self, _system: &str, user: &str) -> Result<String> {
        let preview: String = user.chars().take(280).collect();
        Ok(format!(
            "[Offline draft — connect a model in Settings → API keys for real generation.]\n\n\
             Based on the provided sources:\n{preview}…"
        ))
    }
    fn name(&self) -> String {
        "stub".into()
    }
}

/// Google Gemini `generateContent` (BYOK).
pub struct GeminiLlm {
    pub api_key: String,
    pub model: String,
    pub max_tokens: Option<u32>,
}

impl GeminiLlm {
    /// The request body both `generateContent` and `streamGenerateContent` take
    /// — one definition, so streaming cannot drift from non-streaming.
    fn request_body(&self, system: &str, user: &str) -> serde_json::Value {
        // Gemini 2.5 models reason with hidden "thinking" tokens that draw from
        // the SAME maxOutputTokens budget. A small budget is routinely exhausted
        // by thinking before any answer text is emitted, leaving an empty
        // response with a MAX_TOKENS finishReason — the real cause of
        // "generation doesn't work at all" for cheatsheet/material.
        // Two defenses: a roomy budget, and — for flash models, which allow it —
        // disable thinking entirely so every token goes to the actual output.
        // (gemini-2.5-pro rejects thinkingBudget:0, so only set it for flash.)
        // Roomy output budget: a full 7-section cheatsheet (or a merged subject
        // digest) easily exceeds a small cap, and the model would otherwise stop
        // mid-answer — e.g. a table cut off halfway. 2.5 models support 65536.
        let mut generation_config = serde_json::json!({
            "temperature": 0.3,
            "maxOutputTokens": self.max_tokens.unwrap_or(65536)
        });
        if self.model.contains("flash") {
            generation_config["thinkingConfig"] = serde_json::json!({ "thinkingBudget": 0 });
        }
        serde_json::json!({
            "system_instruction": { "parts": [{ "text": system }] },
            "contents": [{ "role": "user", "parts": [{ "text": user }] }],
            "generationConfig": generation_config
        })
    }

    /// Streaming generateContent: same body, `:streamGenerateContent?alt=sse`.
    fn stream_generate(
        &self,
        system: &str,
        user: &str,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<RawReply> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
            self.model, self.api_key
        );
        let resp = llm_client()
            .post(&url)
            .timeout(std::time::Duration::from_secs(900))
            .json(&self.request_body(system, user))
            .send()?;
        let mut acc = String::new();
        let mut shown = String::new();
        let mut finish: Option<String> = None;
        let mut usage: Option<serde_json::Value> = None;
        read_sse("gemini", resp, |json| {
            let piece = gemini_piece(json);
            if let Some(u) = piece.usage {
                usage = Some(u);
            }
            if let Some(f) = piece.finish {
                finish = Some(f);
            }
            if let Some(c) = piece.content {
                acc.push_str(&c);
                return Ok(emit_visible(&acc, &mut shown, on_delta));
            }
            Ok(true)
        })?;
        Ok(RawReply { content: acc.clone(), text: acc, finish_reason: finish, usage })
    }
    /// One generateContent round-trip: (concatenated text, finishReason).
    /// Split out of `complete` so `complete_raw` can hand an EMPTY reply
    /// back for state classification instead of erroring — a MAX_TOKENS
    /// stop that emitted nothing is a `Failed` state the UI can badge,
    /// not an opaque exception.
    fn generate(&self, system: &str, user: &str) -> Result<(String, Option<String>, Option<serde_json::Value>)> {
        let client = llm_client();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );
        // Gemini 2.5 models reason with hidden "thinking" tokens that draw from
        // the SAME maxOutputTokens budget. A small budget is routinely exhausted
        // by thinking before any answer text is emitted, leaving an empty
        // response with a MAX_TOKENS finishReason — the real cause of
        // "generation doesn't work at all" for cheatsheet/material.
        // Two defenses: a roomy budget, and — for flash models, which allow it —
        // disable thinking entirely so every token goes to the actual output.
        // (gemini-2.5-pro rejects thinkingBudget:0, so only set it for flash.)
        // Roomy output budget: a full 7-section cheatsheet (or a merged subject
        // digest) easily exceeds a small cap, and the model would otherwise stop
        // mid-answer — e.g. a table cut off halfway. 2.5 models support 65536.
        let json = send_json("gemini", client.post(&url).json(&self.request_body(system, user)))?;
        // Concatenate every text part (some responses split across parts).
        let text: String = json["candidates"][0]["content"]["parts"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<String>()
            })
            .unwrap_or_default();
        let finish = json["candidates"][0]["finishReason"]
            .as_str()
            .map(str::to_string);
        let usage = if json["usageMetadata"].is_null() {
            None
        } else {
            Some(json["usageMetadata"].clone())
        };
        Ok((text, finish, usage))
    }
}

impl Llm for GeminiLlm {
    fn set_max_tokens(&mut self, max: u32) {
        self.max_tokens = Some(max);
    }
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        let (text, finish, _) = self.generate(system, user)?;
        if text.is_empty() {
            // Surface the finishReason (e.g. MAX_TOKENS / SAFETY) so the failure
            // is diagnosable instead of an opaque "empty response".
            let reason = finish.as_deref().unwrap_or("unknown");
            return Err(Error::Other(format!(
                "gemini: empty response (finishReason={reason}). \
                 If MAX_TOKENS, the model spent its budget on reasoning — try a flash model."
            )));
        }
        Ok(text)
    }
    fn complete_raw(&self, system: &str, user: &str) -> Result<RawReply> {
        let (text, finish, usage) = self.generate(system, user)?;
        Ok(RawReply { content: text.clone(), text, finish_reason: finish, usage })
    }
    fn complete_stream(
        &self,
        system: &str,
        user: &str,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<RawReply> {
        self.stream_generate(system, user, on_delta)
    }
    fn name(&self) -> String {
        format!("gemini:{}", self.model)
    }
    fn ocr(&self, images: &[(String, String)]) -> Result<String> {
        let client = llm_client();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );
        let mut parts = vec![serde_json::json!({ "text": OCR_PROMPT })];
        for (mime, b64) in images {
            parts.push(serde_json::json!({ "inline_data": { "mime_type": mime, "data": b64 } }));
        }
        let body = serde_json::json!({
            "contents": [{ "role": "user", "parts": parts }],
            "generationConfig": { "temperature": 0.0, "maxOutputTokens": 8192 }
        });
        let json = send_json("gemini", client.post(&url).json(&body))?;
        Ok(json["candidates"][0]["content"]["parts"]
            .as_array()
            .map(|ps| ps.iter().filter_map(|p| p["text"].as_str()).collect::<String>())
            .unwrap_or_default())
    }
    /// Vision Q&A with a custom prompt (2.6 figure explanation).
    fn vision(&self, images: &[(String, String)], system: &str, user: &str) -> Result<String> {
        let client = llm_client();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );
        let mut parts = vec![serde_json::json!({ "text": format!("{system}\n\n{user}") })];
        for (mime, b64) in images {
            parts.push(serde_json::json!({ "inline_data": { "mime_type": mime, "data": b64 } }));
        }
        let body = serde_json::json!({
            "contents": [{ "role": "user", "parts": parts }],
            "generationConfig": { "temperature": 0.2, "maxOutputTokens": 8192 }
        });
        let json = send_json("gemini", client.post(&url).json(&body))?;
        Ok(json["candidates"][0]["content"]["parts"]
            .as_array()
            .map(|ps| ps.iter().filter_map(|p| p["text"].as_str()).collect::<String>())
            .unwrap_or_default())
    }
    fn extract_cards(&self, images: &[(String, String)]) -> Result<Vec<CardDraft>> {
        let client = llm_client();
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );
        let mut parts = vec![serde_json::json!({ "text": OCR_CARDS_PROMPT })];
        for (mime, b64) in images {
            parts.push(serde_json::json!({ "inline_data": { "mime_type": mime, "data": b64 } }));
        }
        let mut body = serde_json::json!({
            "contents": [{ "role": "user", "parts": parts }],
            "generationConfig": {
                "temperature": 0.1, "maxOutputTokens": 4096,
                "responseMimeType": "application/json",
                "responseSchema": {
                    "type": "OBJECT",
                    "properties": {
                        "cards": {
                            "type": "ARRAY", "minItems": 1, "maxItems": 15,
                            "items": {
                                "type": "OBJECT",
                                "properties": { "front": { "type": "STRING" }, "back": { "type": "STRING" } },
                                "required": ["front", "back"]
                            }
                        }
                    },
                    "required": ["cards"]
                }
            }
        });
        if self.model.contains("2.5-flash") {
            body["generationConfig"]["thinkingConfig"] = serde_json::json!({ "thinkingBudget": 0 });
        }
        let json = send_json("gemini", client.post(&url).json(&body))?;
        let text: String = json["candidates"][0]["content"]["parts"]
            .as_array().map(|ps| ps.iter().filter_map(|p| p["text"].as_str()).collect()).unwrap_or_default();
        parse_card_drafts(&text)
    }
}

fn parse_card_drafts(text: &str) -> Result<Vec<CardDraft>> {
    let value = extract_json(text)?;
    let raw = value.get("cards").and_then(|v| v.as_array()).cloned()
        .or_else(|| value.as_array().cloned())
        .ok_or_else(|| Error::Other("vision model returned no cards array".into()))?;
    let mut out = Vec::new();
    for item in raw {
        let front = item.get("front").and_then(|v| v.as_str()).unwrap_or("").trim();
        let back = item.get("back").and_then(|v| v.as_str()).unwrap_or("").trim();
        if front.is_empty() || back.is_empty() { continue; }
        if out.iter().any(|c: &CardDraft| c.front.eq_ignore_ascii_case(front)) { continue; }
        out.push(CardDraft { front: front.to_string(), back: back.to_string() });
        if out.len() >= 15 { break; }
    }
    if out.is_empty() { return Err(Error::Other("vision model returned no usable flashcards".into())); }
    Ok(out)
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Pull the affordable token count out of an OpenRouter 402 message, e.g.
/// "...you requested up to 64000 tokens, but can only afford 55765." Returns the
/// last number after "afford" so we can retry within the key's credit limit.
fn parse_affordable(msg: &str) -> Option<u32> {
    let idx = msg.find("afford")?;
    let tail = &msg[idx..];
    let digits: String = tail.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u32>().ok().filter(|n| *n > 0)
}

/// One provider chunk of a streamed answer, normalized. Providers disagree on
/// where the text lives (`delta.content` vs `candidates[].content.parts[].text`)
/// and on how the finishReason travels, so each adapter's parser fills this in
/// and the accumulation logic stays shared.
#[derive(Debug, Default, PartialEq)]
struct StreamPiece {
    /// Visible answer text carried by this chunk (empty on role-only or
    /// finish-only chunks).
    content: Option<String>,
    /// Hidden reasoning (`reasoning` / `reasoning_content`). Never shown while
    /// streaming — the model thinking aloud is not the answer.
    reasoning: Option<String>,
    /// finishReason, when this chunk carries it (normally the last one).
    finish: Option<String>,
    /// usage payload, when the provider reports it mid- or end-of-stream.
    usage: Option<serde_json::Value>,
    /// A refusal that arrived inside the stream, after a 200. Only Anthropic
    /// reports these (an overloaded model can fail mid-answer), but it belongs
    /// here: the parser normalizes the chunk, the reader decides what to do.
    error: Option<String>,
}

/// Payload of one SSE `data:` line, or None for comments, blanks and the
/// `[DONE]` sentinel. SSE permits multi-line `data:` fields; neither OpenAI nor
/// Gemini uses them, and concatenating them would merge two JSON documents.
fn sse_payload(line: &str) -> Option<&str> {
    let body = line.trim_end_matches(['\r', '\n']);
    let body = body.strip_prefix("data:")?.trim();
    if body.is_empty() || body == "[DONE]" {
        None
    } else {
        Some(body)
    }
}

/// Walk an SSE body chunk by chunk. Stops when `on_chunk` returns false (the
/// user pressed Stop) or the body ends. A non-2xx reply becomes an error with
/// the same shape `send_json` uses, so a 402 still reads as a 402.
fn read_sse(
    provider: &str,
    resp: reqwest::blocking::Response,
    mut on_chunk: impl FnMut(&serde_json::Value) -> Result<bool>,
) -> Result<()> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(Error::Other(format!("{provider} {status}: {}", truncate(body.trim(), 300))));
    }
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(resp);
    let mut line = String::new();
    loop {
        line.clear();
        // read_line splits on \n only, and a multi-byte UTF-8 character never
        // contains 0x0A — so a token split across TCP packets still arrives whole.
        let n = reader
            .read_line(&mut line)
            .map_err(|e| Error::Other(format!("{provider}: stream broke mid-answer: {e}")))?;
        if n == 0 {
            return Ok(());
        }
        let Some(payload) = sse_payload(&line) else { continue };
        let json: serde_json::Value = serde_json::from_str(payload).map_err(|e| {
            Error::Other(format!(
                "{provider}: unreadable stream event ({e}): {}",
                truncate(payload, 200)
            ))
        })?;
        if !on_chunk(&json)? {
            return Ok(());
        }
    }
}

/// Withhold a trailing fragment that may be the beginning of a `<think>` tag
/// whose remaining characters are still in flight — otherwise "<thi" + "nk>"
/// flashes raw reasoning markup in the middle of the answer.
fn trim_partial_think_tag(s: &str) -> &str {
    const TAG: &str = "<think>";
    for cut in 1..TAG.len().min(s.len() + 1) {
        if s.ends_with(&TAG[..cut]) {
            return &s[..s.len() - cut];
        }
    }
    s
}

/// Emit only the text the caller has not seen yet.
///
/// `<think>` stripping must run on the WHOLE accumulated text (a tag can open in
/// one chunk and close in another), so the visible text grows monotonically and
/// `shown` is a prefix of it in every normal chunk. If that stops being true
/// (the provider rewound, or a reasoning block closed over already-shown text)
/// nothing is emitted: the caller keeps what it has, and the finished reply
/// replaces the bubble. Returns false = stop generating.
fn emit_visible(acc: &str, shown: &mut String, on_delta: &mut dyn FnMut(&str) -> bool) -> bool {
    let visible = strip_think(acc);
    let safe = trim_partial_think_tag(&visible);
    let Some(new) = safe.strip_prefix(shown.as_str()) else { return true };
    if new.is_empty() {
        return true;
    }
    if !on_delta(new) {
        return false;
    }
    shown.push_str(new);
    true
}

/// Normalize one OpenAI-compatible SSE chunk.
fn openai_piece(v: &serde_json::Value) -> StreamPiece {
    let choice = &v["choices"][0];
    let delta = &choice["delta"];
    let text_of = |key: &str| delta[key].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    StreamPiece {
        content: text_of("content"),
        // OpenRouter/DeepSeek spell it `reasoning`; some gateways `reasoning_content`.
        reasoning: text_of("reasoning").or_else(|| text_of("reasoning_content")),
        finish: choice["finish_reason"].as_str().map(str::to_string),
        usage: (!v["usage"].is_null()).then(|| v["usage"].clone()),
        error: None,
    }
}

/// Normalize one Gemini `streamGenerateContent` chunk: text lives in
/// `candidates[].content.parts[].text` and can be split across several parts.
/// `thought: true` parts are 2.5's thinking, not answer text, so they are skipped.
fn gemini_piece(v: &serde_json::Value) -> StreamPiece {
    let cand = &v["candidates"][0];
    let text: String = cand["content"]["parts"]
        .as_array()
        .map(|ps| {
            ps.iter()
                .filter(|p| p["thought"].as_bool() != Some(true))
                .filter_map(|p| p["text"].as_str())
                .collect()
        })
        .unwrap_or_default();
    StreamPiece {
        content: (!text.is_empty()).then_some(text),
        reasoning: None,
        finish: cand["finishReason"].as_str().map(str::to_string),
        usage: (!v["usageMetadata"].is_null()).then(|| v["usageMetadata"].clone()),
        error: None,
    }
}

/// Normalize one Anthropic `/v1/messages` stream event. Anthropic frames whole
/// events rather than a delta per chunk: `content_block_delta` carries text,
/// `message_delta` the stop reason, `message_start`/`message_delta` the token
/// counts (input on the first, output on the second), and `error` a refusal
/// that arrived after the 200.
fn claude_piece(v: &serde_json::Value) -> StreamPiece {
    match v["type"].as_str().unwrap_or_default() {
        "content_block_delta" => StreamPiece {
            content: v["delta"]["text"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
            ..StreamPiece::default()
        },
        "message_delta" => StreamPiece {
            finish: v["delta"]["stop_reason"].as_str().map(str::to_string),
            usage: (!v["usage"].is_null()).then(|| v["usage"].clone()),
            ..StreamPiece::default()
        },
        "message_start" => StreamPiece {
            usage: (!v["message"]["usage"].is_null()).then(|| v["message"]["usage"].clone()),
            ..StreamPiece::default()
        },
        "error" => StreamPiece {
            error: Some(
                v["error"]["message"]
                    .as_str()
                    .unwrap_or("the stream reported an error")
                    .to_string(),
            ),
            ..StreamPiece::default()
        },
        _ => StreamPiece::default(),
    }
}

fn build_client(use_proxy: bool) -> reqwest::blocking::Client {
    // Force identity (uncompressed) transfer encoding. OpenRouter streams a gzip
    // body padded with whitespace keep-alives during slow generation, which
    // reqwest's automatic gzip decoder fails to decode — surfacing as the opaque
    // "error decoding response body" that broke cheatsheet/material generation.
    // Setting Accept-Encoding ourselves ALSO disables reqwest's auto-decompress,
    // so the body is read verbatim regardless of what the server sends.
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        reqwest::header::HeaderValue::from_static("identity"),
    );
    let builder = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .default_headers(headers);
    let builder = if use_proxy { builder } else { builder.no_proxy() };
    builder.build().unwrap_or_default()
}

/// Shared blocking HTTP client for LLM calls: a generous request timeout (model
/// generation can be slow) so a stalled connection fails cleanly instead of
/// hanging forever.
fn llm_client() -> reqwest::blocking::Client {
    build_client(true)
}

/// True for a base URL that never leaves the machine (Ollama, LM Studio, a local
/// gateway). Read from the URL because that is the only place the destination is
/// known before the request is built.
fn is_loopback(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let rest = rest.split('/').next().unwrap_or(rest);
    rest == "localhost"
        || rest.starts_with("localhost:")
        || rest.starts_with("127.")
        || rest.starts_with("[::1]")
        || rest.starts_with("0.0.0.0")
}

/// The client to use for a configurable `base_url`. A loopback destination
/// bypasses `HTTP_PROXY`: reqwest obeys the env proxy by default, so a machine
/// with one set answers 127.0.0.1 with a refusal — which is exactly "Ollama
/// stopped working", reported as if the model were at fault.
fn llm_client_for(base_url: &str) -> reqwest::blocking::Client {
    build_client(!is_loopback(base_url))
}

/// Send a request, then read its body as text and parse it as JSON — with
/// retries on TRANSIENT transport failures. Long LLM generations behind proxies
/// (notably OpenRouter, which streams whitespace keep-alives during slow
/// generation) occasionally drop the connection mid-body; reqwest surfaces that
/// as the opaque "error decoding response body". A couple of retries recover it.
/// Non-2xx responses and JSON-parse failures are returned verbatim (with the
/// body for diagnosis) and are NOT retried. Reading as text (not `resp.json()`)
/// keeps errors diagnosable instead of opaque.
fn send_json(provider: &str, req: reqwest::blocking::RequestBuilder) -> Result<serde_json::Value> {
    let transient = |e: &reqwest::Error| {
        e.is_timeout() || e.is_connect() || e.is_request() || e.is_body() || e.is_decode()
    };
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let this = req
            .try_clone()
            .ok_or_else(|| Error::Other(format!("{provider}: request could not be retried")))?;
        let resp = match this.send() {
            Ok(r) => r,
            Err(e) if attempt < 3 && transient(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(800 * attempt as u64));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let status = resp.status();
        let raw = match resp.text() {
            Ok(t) => t,
            Err(e) if attempt < 3 && transient(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(800 * attempt as u64));
                continue;
            }
            Err(e) => {
                return Err(Error::Other(format!(
                    "{provider}: could not read response body: {e}"
                )))
            }
        };
        if !status.is_success() {
            return Err(Error::Other(format!(
                "{provider} {status}: {}",
                truncate(raw.trim(), 300)
            )));
        }
        match serde_json::from_str(&raw) {
            Ok(v) => return Ok(v),
            // A successful (2xx) response that doesn't parse is almost always a
            // TRUNCATED body (connection cut mid-stream during a long/large
            // generation — e.g. OCR with big images). Retry rather than fail.
            Err(_) if attempt < 3 => {
                std::thread::sleep(std::time::Duration::from_millis(800 * attempt as u64));
                continue;
            }
            Err(e) => {
                return Err(Error::Other(format!(
                    "{provider}: response was not valid JSON ({e}): {}",
                    truncate(raw.trim(), 300)
                )))
            }
        }
    }
}

/// OpenAI-compatible chat provider — used for OpenRouter and OpenAI (and any
/// custom OpenAI-compatible gateway). One code path, different base URL/key.
pub struct OpenAiCompatLlm {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub label: &'static str,
    pub max_tokens: Option<u32>,
}

/// Reasoning models (MiniMax M-series, DeepSeek-R1 style) inline their
/// chain-of-thought as `<think>…</think>` inside message.content. Strip
/// completed blocks; an unterminated block (max_tokens cut mid-think) drops
/// everything from the opening tag so the UI never shows raw reasoning.
fn strip_think(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => rest = &rest[i + j + "</think>".len()..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out.trim_start().to_string()
}

impl OpenAiCompatLlm {
    /// One raw chat/completions attempt — the JSON, not just the text.
    fn chat_json(&self, system: &str, user: &str, max_tokens: Option<u32>) -> Result<serde_json::Value> {
        let key = self.api_key.trim();
        let client = llm_client_for(&self.base_url);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user }
            ],
            "temperature": 0.3
        });
        if let Some(n) = max_tokens {
            body["max_tokens"] = serde_json::json!(n);
        }
        send_json(
            self.label,
            client
                .post(&url)
                .header("Authorization", format!("Bearer {key}"))
                // OpenRouter likes these; harmless elsewhere.
                .header("HTTP-Referer", "https://cortex.study")
                .header("X-Title", "Cortex")
                .json(&body),
        )
    }

    /// One chat/completions attempt with an explicit max_tokens (or none).
    fn complete_once(&self, system: &str, user: &str, max_tokens: Option<u32>) -> Result<String> {
        let json = self.chat_json(system, user, max_tokens)?;
        json["choices"][0]["message"]["content"]
            .as_str()
            .map(strip_think)
            .ok_or_else(|| Error::Other(format!("{}: empty response", self.label)))
    }
    /// One streaming chat/completions attempt: the same request as `chat_json`
    /// with `stream: true`. The answer arrives as SSE chunks, each carrying a
    /// piece of `delta.content`; the return value is the concatenation, exactly
    /// like a non-streamed call, so the caller's post-processing is unchanged.
    fn stream_once(
        &self,
        system: &str,
        user: &str,
        max_tokens: Option<u32>,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<RawReply> {
        let key = self.api_key.trim();
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user }
            ],
            "temperature": 0.3,
            "stream": true
        });
        if let Some(n) = max_tokens {
            body["max_tokens"] = serde_json::json!(n);
        }
        let resp = llm_client_for(&self.base_url)
            .post(&url)
            .header("Authorization", format!("Bearer {key}"))
            .header("HTTP-Referer", "https://cortex.study")
            .header("X-Title", "Cortex")
            // A stream is read for as long as the model keeps talking; the
            // client's 300s budget covers one blocking response, not a long one
            // arriving in pieces.
            .timeout(std::time::Duration::from_secs(900))
            .json(&body)
            .send()
            .map_err(|e| Error::Other(format!("{}: {e}", self.label)))?;

        let mut acc = String::new();
        let mut reasoning = String::new();
        let mut shown = String::new();
        let mut finish: Option<String> = None;
        let mut usage: Option<serde_json::Value> = None;
        read_sse(self.label, resp, |json| {
            let piece = openai_piece(json);
            if let Some(u) = piece.usage {
                usage = Some(u);
            }
            if let Some(f) = piece.finish {
                finish = Some(f);
            }
            if let Some(r) = piece.reasoning {
                reasoning.push_str(&r);
            }
            if let Some(c) = piece.content {
                acc.push_str(&c);
                return Ok(emit_visible(&acc, &mut shown, on_delta));
            }
            Ok(true)
        })?;

        // A reasoning model can spend the whole stream in `reasoning` and emit no
        // content at all; `complete_raw` falls back to that text, so the streamed
        // path has to do the same or it would return an empty answer.
        let raw = if acc.trim().is_empty() { reasoning } else { acc };
        Ok(RawReply {
            text: strip_think(&raw),
            finish_reason: finish,
            usage,
            content: raw,
        })
    }
}

impl Llm for OpenAiCompatLlm {
    fn set_max_tokens(&mut self, max: u32) {
        self.max_tokens = Some(max);
    }
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        let key = self.api_key.trim();
        if key.is_empty() {
            return Err(Error::Other(format!(
                "{}: API key is empty — paste it in Settings → API keys and click Save keys.",
                self.label
            )));
        }
        match self.complete_once(system, user, self.max_tokens) {
            Ok(t) => Ok(t),
            // OpenRouter returns 402 with "...can only afford N" when the key's
            // credit limit can't cover the requested max_tokens. Retry once with
            // the affordable amount so generation succeeds instead of hard-failing.
            Err(Error::Other(msg)) if msg.contains("402") => {
                if let Some(afford) = parse_affordable(&msg) {
                    // Leave a little headroom under the affordable cap.
                    let retry = afford.saturating_sub(512).max(256);
                    return self.complete_once(system, user, Some(retry));
                }
                Err(Error::Other(msg))
            }
            Err(e) => Err(e),
        }
    }
    fn name(&self) -> String {
        format!("{}:{}", self.label, self.model)
    }
    fn complete_stream(
        &self,
        system: &str,
        user: &str,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<RawReply> {
        if self.api_key.trim().is_empty() {
            return Err(Error::Other(format!(
                "{}: API key is empty — paste it in Settings → API keys and click Save keys.",
                self.label
            )));
        }
        // Cell, not a local bool: the retry guard below reads it while the
        // closure that writes it is still borrowed by `stream_once`.
        let emitted = std::cell::Cell::new(false);
        let mut sink = |d: &str| {
            emitted.set(true);
            on_delta(d)
        };
        match self.stream_once(system, user, self.max_tokens, &mut sink) {
            Ok(r) => Ok(r),
            // Same 402 story as `complete` — but only when nothing reached the
            // screen yet: retrying after partial text would repeat the answer.
            Err(Error::Other(msg)) if !emitted.get() && msg.contains("402") => {
                match parse_affordable(&msg) {
                    Some(afford) => {
                        let retry = afford.saturating_sub(512).max(256);
                        self.stream_once(system, user, Some(retry), &mut sink)
                    }
                    None => Err(Error::Other(msg)),
                }
            }
            Err(e) => Err(e),
        }
    }
    fn complete_raw(&self, system: &str, user: &str) -> Result<RawReply> {
        if self.api_key.trim().is_empty() {
            return Err(Error::Other(format!(
                "{}: API key is empty — paste it in Settings → API keys and click Save keys.",
                self.label
            )));
        }
        let json = self.chat_json(system, user, self.max_tokens)?;
        let choice = &json["choices"][0];
        let content = choice["message"]["content"].as_str().unwrap_or("").to_string();
        // Reasoning models put their chain-of-thought inside `reasoning`, not
        // `content`; surface it rather than reporting an empty raw string.
        let raw = if content.trim().is_empty() {
            choice["message"]["reasoning"].as_str().unwrap_or("").to_string()
        } else {
            content
        };
        Ok(RawReply {
            text: strip_think(&raw),
            finish_reason: choice["finish_reason"].as_str().map(str::to_string),
            usage: if json["usage"].is_null() { None } else { Some(json["usage"].clone()) },
            content: raw,
        })
    }
    fn ocr(&self, images: &[(String, String)]) -> Result<String> {
        let key = self.api_key.trim();
        if key.is_empty() {
            return Err(Error::Other(format!("{}: API key is empty", self.label)));
        }
        let client = llm_client_for(&self.base_url);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut content = vec![serde_json::json!({ "type": "text", "text": OCR_PROMPT })];
        for (mime, b64) in images {
            content.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{b64}") }
            }));
        }
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": content }],
            "temperature": 0.0
        });
        let json = send_json(
            self.label,
            client
                .post(&url)
                .header("Authorization", format!("Bearer {key}"))
                .header("HTTP-Referer", "https://cortex.study")
                .header("X-Title", "Cortex")
                .json(&body),
        )?;
        Ok(strip_think(
            json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default(),
        ))
    }
    /// Vision Q&A with a custom prompt (2.6 figure explanation).
    fn vision(&self, images: &[(String, String)], system: &str, user: &str) -> Result<String> {
        let key = self.api_key.trim();
        if key.is_empty() {
            return Err(Error::Other(format!("{}: API key is empty", self.label)));
        }
        let client = llm_client_for(&self.base_url);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut content = vec![serde_json::json!({
            "type": "text",
            "text": format!("{system}\n\n{user}")
        })];
        for (mime, b64) in images {
            content.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{b64}") }
            }));
        }
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": content }],
            "temperature": 0.2
        });
        let json = send_json(
            self.label,
            client
                .post(&url)
                .header("Authorization", format!("Bearer {key}"))
                .header("HTTP-Referer", "https://cortex.study")
                .header("X-Title", "Cortex")
                .json(&body),
        )?;
        Ok(strip_think(
            json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default(),
        ))
    }
    fn extract_cards(&self, images: &[(String, String)]) -> Result<Vec<CardDraft>> {
        let key = self.api_key.trim();
        if key.is_empty() { return Err(Error::Other(format!("{}: API key is empty", self.label))); }
        let client = llm_client_for(&self.base_url);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut content = vec![serde_json::json!({ "type": "text", "text": OCR_CARDS_PROMPT })];
        for (mime, b64) in images {
            content.push(serde_json::json!({ "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{b64}") } }));
        }
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": content }],
            "temperature": 0.1, "max_tokens": 4096,
            "response_format": { "type": "json_object" }
        });
        let json = send_json(self.label, client.post(&url)
            .header("Authorization", format!("Bearer {key}"))
            .header("HTTP-Referer", "https://cortex.study")
            .header("X-Title", "Cortex")
            .json(&body))?;
        let text = strip_think(json["choices"][0]["message"]["content"].as_str().unwrap_or_default());
        parse_card_drafts(&text)
    }
    fn gen_image(&self, prompt: &str) -> Result<String> {
        let key = self.api_key.trim();
        if key.is_empty() {
            return Err(Error::Other(format!("{}: API key is empty", self.label)));
        }
        let client = llm_client_for(&self.base_url);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
            "modalities": ["image", "text"],
        });
        let json = send_json(
            self.label,
            client
                .post(&url)
                .header("Authorization", format!("Bearer {key}"))
                .header("HTTP-Referer", "https://cortex.study")
                .header("X-Title", "Cortex")
                .json(&body),
        )?;
        json["choices"][0]["message"]["images"][0]["image_url"]["url"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Other(format!("{}: model returned no image", self.label)))
    }
}

/// Anthropic Claude Messages API (BYOK).
pub struct ClaudeLlm {
    pub api_key: String,
    pub model: String,
    pub max_tokens: Option<u32>,
}

impl ClaudeLlm {
    /// One messages round-trip: (first text block, stop_reason, usage).
    /// Shared by `complete` (hard error on empty, as before) and
    /// `complete_raw` (empty handed back for state classification).
    fn messages(&self, system: &str, user: &str, max_tokens: u32) -> Result<(String, Option<String>, Option<serde_json::Value>)> {
        let client = llm_client();
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "temperature": 0.3,
            "system": system,
            "messages": [{ "role": "user", "content": user }]
        });
        let json = send_json(
            "claude",
            client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body),
        )?;
        let text = json["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let stop = json["stop_reason"].as_str().map(str::to_string);
        let usage = if json["usage"].is_null() { None } else { Some(json["usage"].clone()) };
        Ok((text, stop, usage))
    }

    /// One streaming messages round-trip: the same request with `stream: true`,
    /// read as SSE. Returns exactly what `messages` would have, so the pipeline
    /// cannot tell the two apart apart from having already shown the text.
    fn stream_messages(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<RawReply> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "temperature": 0.3,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
            "stream": true
        });
        let resp = llm_client()
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            // The stream is read for as long as the model keeps talking; the
            // shared client's 300s budget covers one blocking response, not a
            // long one arriving in pieces.
            .timeout(std::time::Duration::from_secs(900))
            .json(&body)
            .send()
            .map_err(|e| Error::Other(format!("claude: {e}")))?;

        let mut acc = String::new();
        let mut finish: Option<String> = None;
        let mut usage: Option<serde_json::Value> = None;
        let mut error: Option<String> = None;
        read_sse("claude", resp, |ev| {
            let piece = claude_piece(ev);
            if let Some(e) = piece.error {
                error = Some(e);
                return Ok(false); // nothing more is coming
            }
            if let Some(f) = piece.finish {
                finish = Some(f);
            }
            if let Some(u) = piece.usage {
                // input_tokens arrives on message_start, output_tokens on
                // message_delta: merge instead of letting the second replace
                // the first, or the accounting only ever records half.
                match usage.as_mut() {
                    Some(serde_json::Value::Object(m)) if u.is_object() => {
                        if let Some(new) = u.as_object() {
                            for (k, v) in new {
                                m.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    _ => usage = Some(u),
                }
            }
            if let Some(t) = piece.content {
                acc.push_str(&t);
                return Ok(on_delta(&t));
            }
            Ok(true)
        })?;
        if let Some(e) = error {
            return Err(Error::Other(format!("claude: {e}")));
        }
        if acc.is_empty() {
            return Err(Error::Other("claude: empty response".into()));
        }
        Ok(RawReply { content: acc.clone(), text: acc, finish_reason: finish, usage })
    }
}

impl Llm for ClaudeLlm {
    fn set_max_tokens(&mut self, max: u32) {
        self.max_tokens = Some(max);
    }
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        // Roomy enough for a full cheatsheet without cutting off mid-section.
        // Modern Claude models support far more (Sonnet 4.x: 64K, Opus: 128K
        // output tokens); 32K covers any cheatsheet while staying within limits.
        let (text, _, _) = self.messages(system, user, self.max_tokens.unwrap_or(32000))?;
        if text.is_empty() {
            return Err(Error::Other("claude: empty response".into()));
        }
        Ok(text)
    }
    fn complete_raw(&self, system: &str, user: &str) -> Result<RawReply> {
        let (text, stop, usage) = self.messages(system, user, self.max_tokens.unwrap_or(32000))?;
        Ok(RawReply { content: text.clone(), text, finish_reason: stop, usage })
    }
    fn complete_stream(
        &self,
        system: &str,
        user: &str,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<RawReply> {
        self.stream_messages(system, user, self.max_tokens.unwrap_or(32000), on_delta)
    }
    fn extract_cards(&self, images: &[(String, String)]) -> Result<Vec<CardDraft>> {
        let mut content = Vec::new();
        for (mime, b64) in images {
            content.push(serde_json::json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": b64 } }));
        }
        content.push(serde_json::json!({ "type": "text", "text": OCR_CARDS_PROMPT }));
        let body = serde_json::json!({
            "model": self.model, "max_tokens": self.max_tokens.unwrap_or(4096), "temperature": 0.1,
            "system": "只输出合法 JSON，不要 Markdown 代码围栏或解释文字。",
            "messages": [{ "role": "user", "content": content }]
        });
        let json = send_json("claude", llm_client().post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body))?;
        let text = json["content"][0]["text"].as_str().unwrap_or_default();
        parse_card_drafts(text)
    }
    fn name(&self) -> String {
        format!("claude:{}", self.model)
    }
}

/// API keys available from settings.
#[derive(Default)]
pub struct Keys {
    pub gemini: Option<String>,
    pub openrouter: Option<String>,
    pub openai: Option<String>,
    pub claude: Option<String>,
    pub custom_api_key: Option<String>,
    pub custom_endpoint: Option<String>,
    /// Ollama base URL (e.g. http://localhost:11434) — local, keyless.
    pub ollama_url: Option<String>,
    /// True when the app is in offline mode: runtime fallback chains then
    /// never gain cloud providers (the offline promise beats availability).
    pub offline: bool,
    /// Keys for every provider in `crate::providers`, keyed by provider id.
    /// The bespoke arms above stay authoritative (a legacy field wins), so
    /// moving a provider into the registry never invalidates a saved key.
    pub extra: std::collections::HashMap<String, String>,
}

impl Keys {
    /// The API key for a provider id, whether it lives in a legacy field or in
    /// the registry table.
    pub fn for_provider(&self, id: &str) -> Option<&str> {
        let legacy = match id {
            "gemini" => self.gemini.as_deref(),
            "openrouter" => self.openrouter.as_deref(),
            "openai" => self.openai.as_deref(),
            "claude" | "anthropic" => self.claude.as_deref(),
            _ => None,
        }
        .or_else(|| self.extra.get(id).map(String::as_str));
        legacy.map(str::trim).filter(|s| !s.is_empty())
    }
}

fn nonempty(o: &Option<String>) -> Option<&str> {
    o.as_deref().filter(|s| !s.is_empty())
}

/// Build an LLM from a "provider:model" spec + the available keys. Returns
/// `None` when no usable provider/key is configured — callers turn that into a
/// clear "add an API key" error instead of silently producing fake output.
pub fn from_spec(spec: &str, keys: &Keys) -> Option<Box<dyn Llm>> {
    // Trim — a stored spec with stray whitespace/newline would otherwise produce
    // an invalid model id (and thus a provider error).
    let spec = spec.trim();
    let (provider, model) = spec.split_once(':').unwrap_or(("gemini", spec));
    let model = model.trim().to_string();
    match provider {
        "gemini" => nonempty(&keys.gemini).map(|k| {
            Box::new(GeminiLlm { api_key: k.to_string(), model, max_tokens: None }) as Box<dyn Llm>
        }),
        "openrouter" => nonempty(&keys.openrouter).map(|k| {
            Box::new(OpenAiCompatLlm {
                base_url: "https://openrouter.ai/api/v1".into(),
                api_key: k.to_string(),
                model,
                label: "openrouter",
                max_tokens: None,
            }) as Box<dyn Llm>
        }),
        "claude" | "anthropic" => nonempty(&keys.claude).map(|k| {
            Box::new(ClaudeLlm { api_key: k.to_string(), model, max_tokens: None }) as Box<dyn Llm>
        }),
        "custom" => nonempty(&keys.custom_endpoint).map(|base| {
            Box::new(OpenAiCompatLlm {
                base_url: base.to_string(),
                api_key: nonempty(&keys.custom_api_key).unwrap_or("").to_string(),
                model,
                label: "custom",
                max_tokens: None,
            }) as Box<dyn Llm>
        }),
        // Ollama exposes an OpenAI-compatible API at <base>/v1; it's keyless, so
        // pass a dummy bearer token (Ollama ignores it).
        "ollama" => {
            let base = nonempty(&keys.ollama_url).unwrap_or("http://localhost:11434");
            Some(Box::new(OpenAiCompatLlm {
                base_url: format!("{}/v1", base.trim_end_matches('/')),
                api_key: "ollama".to_string(),
                model,
                label: "ollama",
                max_tokens: None,
            }) as Box<dyn Llm>)
        }
        // Everything else comes from the registry (built-in first, then the
        // plugins/ manifests), so a new provider is data rather than a new arm
        // here.
        other => {
            let resolved: Option<(String, String, &'static str)> = crate::providers::find(other)
                .and_then(|p| keys.for_provider(p.id).map(|k| (p.base_url.to_string(), k.to_string(), p.id)));
            let resolved = resolved.or_else(|| {
                crate::plugins::find_provider(other).and_then(|p| {
                    keys.for_provider(other).map(|k| {
                        let label: &'static str = Box::leak(p.id.clone().into_boxed_str());
                        (p.base_url, k.to_string(), label)
                    })
                })
            });
            resolved.map(|(base_url, k, label)| {
                Box::new(OpenAiCompatLlm {
                    base_url,
                    api_key: k,
                    model,
                    label,
                    max_tokens: None,
                }) as Box<dyn Llm>
            })
        }
    }
}

/// Providers kept on standby for runtime fallback, in try order. Ollama is
/// absent on purpose: "local" needs a concrete model name, so it only joins
/// via an explicit user choice (the model picker), not a guess.
const AUTO_FALLBACKS: &[&str] = &[
    "openrouter:deepseek/deepseek-v4-flash",
    "gemini:gemini-2.5-flash",
    "openai:gpt-4o-mini",
    "claude:claude-3-5-sonnet-20241022",
];

/// Runtime multi-provider fallback (2.9). `from_spec_or_any` handles a MISSING
/// key at construction time; this handles a DOWN provider at call time — 429s,
/// 5xx, network drops — by walking the chain until one model answers, instead
/// of surfacing the failure and interrupting the study flow. `name()` reports
/// whichever model last served, so token records stay attributable.
pub struct FallbackLlm {
    chain: Vec<Box<dyn Llm>>,
    served: std::sync::atomic::AtomicUsize,
}

impl FallbackLlm {
    pub fn new(chain: Vec<Box<dyn Llm>>) -> Self {
        Self { chain, served: std::sync::atomic::AtomicUsize::new(0) }
    }

    /// How many models are on standby (exposed for tests).
    pub fn chain_len(&self) -> usize {
        self.chain.len()
    }

    fn delegate<T>(&self, op: &str, f: impl Fn(&dyn Llm) -> Result<T>) -> Result<T> {
        let mut last: Option<Error> = None;
        for (i, m) in self.chain.iter().enumerate() {
            match f(m.as_ref()) {
                Ok(v) => {
                    self.served
                        .store(i, std::sync::atomic::Ordering::Relaxed);
                    return Ok(v);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| Error::Other(format!("{op}: empty model chain"))))
    }
}

impl Llm for FallbackLlm {
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        self.delegate("complete", |m| m.complete(system, user))
    }
    fn name(&self) -> String {
        let i = self
            .served
            .load(std::sync::atomic::Ordering::Relaxed);
        self.chain[i.min(self.chain.len() - 1)].name()
    }
    /// Stream through the chain. A provider that dies BEFORE showing any text is
    /// swapped for the next one — the same contract as `delegate`, and the reason
    /// a 429 stays invisible. Once text has left for the UI a retry would repeat
    /// it, so from that point the error is the answer.
    fn complete_stream(
        &self,
        system: &str,
        user: &str,
        on_delta: &mut dyn FnMut(&str) -> bool,
    ) -> Result<RawReply> {
        let mut last: Option<Error> = None;
        for (i, m) in self.chain.iter().enumerate() {
            // Cell, not a local bool: the guard reads it while the closure that
            // writes it is still borrowed by the provider call.
            let emitted = std::cell::Cell::new(false);
            let mut sink = |d: &str| {
                emitted.set(true);
                on_delta(d)
            };
            match m.complete_stream(system, user, &mut sink) {
                Ok(reply) => {
                    self.served
                        .store(i, std::sync::atomic::Ordering::Relaxed);
                    return Ok(reply);
                }
                Err(e) if emitted.get() => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| Error::Other("stream: empty model chain".into())))
    }
    fn set_max_tokens(&mut self, max: u32) {
        for m in &mut self.chain {
            m.set_max_tokens(max);
        }
    }
    fn ocr(&self, images: &[(String, String)]) -> Result<String> {
        self.delegate("ocr", |m| m.ocr(images))
    }
    fn vision(&self, images: &[(String, String)], system: &str, user: &str) -> Result<String> {
        self.delegate("vision", |m| m.vision(images, system, user))
    }
    fn extract_cards(&self, images: &[(String, String)]) -> Result<Vec<CardDraft>> {
        self.delegate("extract_cards", |m| m.extract_cards(images))
    }
    fn gen_image(&self, prompt: &str) -> Result<String> {
        self.delegate("gen_image", |m| m.gen_image(prompt))
    }
}

/// Like `from_spec`, but if the configured spec's provider has no key, fall back
/// to whichever provider DOES have a key. This lets generation work as soon as
/// ANY API key is set, even if a per-task model still points at an unkeyed
/// provider (the common "I added my OpenRouter key but cheatsheet still says no
/// model" case — cheatsheet defaulted to gemini).
///
/// On top of that, the returned model carries a runtime fallback chain of the
/// OTHER keyed providers' fast defaults (`FallbackLlm`): if the primary answers
/// with a 429/5xx/network error mid-session, the next provider takes the call
/// instead of the request dying. Offline mode (keys.offline) never gains cloud
/// fallbacks — the offline promise beats availability.
pub fn from_spec_or_any(spec: &str, keys: &Keys) -> Option<Box<dyn Llm>> {
    let mut chain = build_chain(spec, keys)?;
    Some(if chain.len() == 1 {
        chain.pop().unwrap()
    } else {
        Box::new(FallbackLlm::new(chain))
    })
}

/// Resolution + standby assembly: the primary (spec, or whichever keyed
/// provider answers when the spec's provider has no key) plus the other keyed
/// providers' fast defaults as runtime standbys. Offline keys never gain cloud
/// standbys.
fn build_chain(spec: &str, keys: &Keys) -> Option<Vec<Box<dyn Llm>>> {
    let primary = if let Some(m) = from_spec(spec, keys) {
        m
    } else {
        let fallback = if nonempty(&keys.openrouter).is_some() {
            "openrouter:deepseek/deepseek-v4-flash"
        } else if nonempty(&keys.gemini).is_some() {
            "gemini:gemini-2.5-flash"
        } else if nonempty(&keys.openai).is_some() {
            "openai:gpt-4o-mini"
        } else if nonempty(&keys.claude).is_some() {
            "claude:claude-3-5-sonnet-20241022"
        } else {
            // No custom ("custom:default") guess here: a custom gateway has no
            // universal model name, and sending "default" to e.g. MiniMax 400s
            // with an opaque error. Custom models join only via explicit slots.
            return None;
        };
        from_spec(fallback, keys)?
    };
    let mut chain = vec![primary];
    if !keys.offline {
        let pprov = spec.split(':').next().unwrap_or("");
        let pmodel = spec.split_once(':').map(|(_, m)| m).unwrap_or("");
        for s in AUTO_FALLBACKS {
            let prov = s.split(':').next().unwrap_or("");
            let model = s.split_once(':').map(|(_, m)| m).unwrap_or("");
            if prov == pprov && model == pmodel {
                continue; // the primary itself (or a twin) — no standby needed
            }
            if let Some(m) = from_spec(s, keys) {
                if chain.iter().all(|c| c.name() != m.name()) {
                    chain.push(m);
                }
            }
        }
    }
    Some(chain)
}

/// Extract the first JSON value (object or array) from an LLM reply that may be
/// wrapped in prose or ```json fences.
///
/// Robustness: we DON'T rely on a strict brace-matcher to find the closing
/// delimiter, because an unescaped `"` inside a rich-markdown string value
/// desyncs the in-string tracking and truncates the slice. Instead we bound the
/// JSON as `first opener .. last matching closer` (LLMs emit a single JSON value,
/// optionally followed by prose that rarely contains a stray `}`/`]`) and hand
/// the slice to `parse_lenient`, which repairs the common invalid-JSON shapes.
pub fn extract_json(text: &str) -> Result<serde_json::Value> {
    let t = text.trim();
    // strip a leading code fence
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    let t = t.trim_start_matches("```").trim();

    let start = match t.find(['{', '[']) {
        Some(s) => s,
        None => return Err(Error::Other("no JSON found in LLM reply".into())),
    };
    let open = t.as_bytes()[start] as char;
    let close = if open == '{' { '}' } else { ']' };
    let end = t.rfind(close).filter(|&e| e > start);
    if let Some(end) = end {
        let slice = &t[start..=end];
        if let Ok(v) = parse_lenient(slice) {
            return Ok(v);
        }
    }
    // Fallback: repair from the opener to the end of the text (handles a missing
    // closer or trailing-prose confusion).
    parse_lenient(&t[start..])
}

/// Parse a JSON slice, retrying once with a repair pass. LLMs producing rich
/// content (tables, callouts, multi-line explanations) routinely emit invalid
/// JSON inside string values — LITERAL newlines/tabs and UNESCAPED double quotes
/// — which was the cause of "Model returned unstructured output" for rich
/// cheatsheets. The repair pass fixes both.
fn parse_lenient(slice: &str) -> Result<serde_json::Value> {
    match serde_json::from_str(slice) {
        Ok(v) => Ok(v),
        Err(_) => Ok(serde_json::from_str(&repair_json(slice))?),
    }
}

/// Repair the two invalid-JSON shapes LLMs emit inside string literals:
///   1. Raw control characters (newline, CR, tab, other <0x20) → escaped.
///   2. Unescaped interior double quotes → escaped to `\"`.
/// For (2) a `"` is treated as the string's CLOSER only when the next
/// non-whitespace character is structural (`:` `,` `}` `]`) or end-of-input;
/// otherwise it's interior content and gets escaped. Already-escaped sequences
/// and structure outside strings are left untouched. The heuristic can't cover a
/// quoted phrase that ends immediately before a comma, but it fixes the common
/// cases and is strictly more tolerant than escaping control chars alone.
fn repair_json(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(s.len() + 16);
    let mut in_str = false;
    let mut esc = false;
    let mut i = 0;
    while i < n {
        let ch = chars[i];
        if in_str {
            if esc {
                out.push(ch);
                esc = false;
            } else if ch == '\\' {
                out.push(ch);
                esc = true;
            } else if ch == '"' {
                let mut j = i + 1;
                while j < n && chars[j].is_whitespace() {
                    j += 1;
                }
                let closes = match chars.get(j) {
                    None => true,
                    Some(&c) => matches!(c, ':' | ',' | '}' | ']'),
                };
                if closes {
                    out.push('"');
                    in_str = false;
                } else {
                    out.push_str("\\\"");
                }
            } else {
                match ch {
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
        } else {
            if ch == '"' {
                in_str = true;
            }
            out.push(ch);
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_urls_are_recognised_so_they_bypass_the_env_proxy() {
        for local in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:1234",
            "https://localhost",
            "http://[::1]:8080/v1",
            "http://0.0.0.0:8000",
        ] {
            assert!(is_loopback(local), "{local}");
        }
        for remote in [
            "https://openrouter.ai/api/v1",
            "https://api.openai.com/v1",
            "http://192.168.1.9:11434/v1",
        ] {
            assert!(!is_loopback(remote), "{remote}");
        }
    }

    #[test]
    fn ocr_cards_trim_deduplicate_and_skip_incomplete() {
        let cards = parse_card_drafts(r#"```json
{"cards":[{"front":" Term ","back":" Meaning "},{"front":"term","back":"duplicate"},{"front":"empty","back":" "}]}
```"#).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].front, "Term");
        assert_eq!(cards[0].back, "Meaning");
        assert!(parse_card_drafts(r#"{"cards":[]}"#).is_err());
        assert!(parse_card_drafts(r#"{"wrong":[]}"#).is_err());
    }

    /// The five-state classifier (0077): provider vocabularies differ, so the
    /// mapping must be per-vocabulary and case-insensitive, and an unknown
    /// reason must never downgrade a real answer.
    #[test]
    fn classify_answer_maps_provider_vocabularies() {
        let full = "一个完整的回答。";
        // normal stops across providers → completed
        assert_eq!(classify_answer(Some("stop"), full), AnswerState::Completed);
        assert_eq!(classify_answer(Some("STOP"), full), AnswerState::Completed);
        assert_eq!(classify_answer(Some("end_turn"), full), AnswerState::Completed);
        assert_eq!(classify_answer(None, full), AnswerState::Completed, "stub/unknown provider");
        // output caps → truncated, partial text kept
        assert_eq!(classify_answer(Some("length"), full), AnswerState::Truncated);
        assert_eq!(classify_answer(Some("MAX_TOKENS"), full), AnswerState::Truncated);
        assert_eq!(classify_answer(Some("max_tokens"), full), AnswerState::Truncated);
        // safety/policy stops → blocked
        assert_eq!(classify_answer(Some("content_filter"), full), AnswerState::Blocked);
        assert_eq!(classify_answer(Some("SAFETY"), full), AnswerState::Blocked);
        assert_eq!(classify_answer(Some("refusal"), full), AnswerState::Blocked);
        // empty output: a named filter explains it, anything else fails
        assert_eq!(classify_answer(Some("SAFETY"), "  "), AnswerState::Blocked);
        assert_eq!(classify_answer(Some("MAX_TOKENS"), ""), AnswerState::Failed);
        assert_eq!(classify_answer(None, ""), AnswerState::Failed);
        // unknown reason with text stays completed — never downgrade blindly
        assert_eq!(classify_answer(Some("brand_new_reason"), full), AnswerState::Completed);
        // wire strings round-trip through as_str
        assert_eq!(AnswerState::Truncated.as_str(), "truncated");
        assert_eq!(serde_json::to_string(&AnswerState::Blocked).unwrap(), "\"blocked\"");
    }

    #[test]
    fn ocr_cards_accept_arrays_and_cap_at_fifteen() {
        let raw: Vec<_> = (0..20).map(|i| serde_json::json!({"front": format!("q{i}"), "back":"a"})).collect();
        assert_eq!(parse_card_drafts(&serde_json::to_string(&raw).unwrap()).unwrap().len(), 15);
    }

    #[test]
    fn extract_json_from_fenced_prose() {
        let reply = "Sure! Here it is:\n```json\n{\"sections\":[{\"title\":\"Defs\",\"items\":[]}]}\n```\nDone.";
        let v = extract_json(reply).unwrap();
        assert_eq!(v["sections"][0]["title"], "Defs");
    }

    #[test]
    fn extract_json_array() {
        let v = extract_json("[{\"q\":\"a\",\"a\":\"b\"}]").unwrap();
        assert_eq!(v[0]["q"], "a");
    }

    #[test]
    fn extract_json_fenced_array_with_trailing_prose() {
        // The common "wrapped in ```json + closing prose" failure shape.
        let reply = "```json\n[{\"q\":\"x\",\"a\":\"y\"}]\n```\nHope this helps!";
        let v = extract_json(reply).unwrap();
        assert_eq!(v[0]["a"], "y");
    }

    #[test]
    fn extract_json_repairs_literal_newlines_in_strings() {
        // Rich cheatsheet content: a `d` value with LITERAL newlines + tab (invalid
        // JSON until repaired). This previously produced "unstructured output".
        let reply = "{\"sections\":[{\"title\":\"T\",\"items\":[{\"t\":\"Term\",\"d\":\"Line one\n> [!NOTE]\nLine two\twith tab\"}]}]}";
        let v = extract_json(reply).unwrap();
        assert_eq!(v["sections"][0]["items"][0]["t"], "Term");
        assert!(v["sections"][0]["items"][0]["d"]
            .as_str()
            .unwrap()
            .contains("NOTE"));
    }

    // Live diagnostic (ignored by default). Run with the real key to see exactly
    // what the OpenRouter call returns through the app's own client:
    //   OPENROUTER_KEY=sk-or-... cargo test --lib live_openrouter -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_openrouter() {
        let key = std::env::var("OPENROUTER_KEY").expect("set OPENROUTER_KEY");
        let llm = OpenAiCompatLlm {
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: key,
            model: "google/gemini-2.5-flash".into(),
            label: "openrouter",
            max_tokens: None,
        };
        match llm.complete("You output JSON only.", "Return {\"ok\":true} and nothing else.") {
            Ok(t) => println!("LIVE OK: {}", truncate(&t, 200)),
            Err(e) => println!("LIVE ERR: {e}"),
        }
    }

    // Live diagnostic (ignored by default): the REAL Settings→OCR extraction
    // path (from_spec → extract_cards) against a vision model. Set a key and a
    // screenshot, then:
    //   OPENROUTER_KEY=sk-or-... OCR_TEST_IMAGE=D:\cortex-base\tools\ocr-test-shot.png `
    //     cargo test --lib live_vision_extract -- --ignored --nocapture
    // GEMINI_KEY works too (default spec switches to gemini:gemini-2.5-flash).
    #[test]
    #[ignore]
    fn live_vision_extract() {
        let keys = Keys {
            openrouter: std::env::var("OPENROUTER_KEY").ok(),
            gemini: std::env::var("GEMINI_KEY").ok(),
            ..Default::default()
        };
        let default_spec = if keys.openrouter.is_some() {
            "openrouter:google/gemini-2.5-flash"
        } else {
            "gemini:gemini-2.5-flash"
        };
        let spec = std::env::var("OCR_TEST_SPEC").unwrap_or_else(|_| default_spec.into());
        let img = std::env::var("OCR_TEST_IMAGE").unwrap_or_else(|_| "ocr-test-shot.png".into());
        let bytes = std::fs::read(&img).unwrap_or_else(|e| panic!("read {img}: {e}"));
        let llm = from_spec(&spec, &keys).unwrap_or_else(|| panic!("no API key for {spec}"));
        match llm.extract_cards(&[("image/png".into(), b64_encode(&bytes))]) {
            Ok(cards) => {
                println!("extracted {} cards:", cards.len());
                for c in &cards {
                    println!("  Q: {} || A: {}", truncate(&c.front, 90), truncate(&c.back, 90));
                }
                assert!(!cards.is_empty(), "model returned no usable flashcards");
            }
            Err(e) => panic!("LIVE extract failed: {e}"),
        }
    }

    #[test]
    fn extract_json_repairs_unescaped_interior_quotes() {
        // Rich cheatsheet content where the model forgot to escape interior
        // quotes — the real production cause of "unstructured output".
        let reply = "{\"sections\":[{\"title\":\"Defs\",\"items\":[{\"t\":\"Place\",\"d\":\"A \"place\" is a location with meaning.\"}]}]}";
        let v = extract_json(reply).unwrap();
        assert_eq!(v["sections"][0]["items"][0]["t"], "Place");
        assert!(v["sections"][0]["items"][0]["d"]
            .as_str()
            .unwrap()
            .contains("place"));
    }

    #[test]
    fn extract_json_repairs_quotes_and_newlines_together() {
        // Both failure shapes at once inside one rich body.
        let reply = "Here:\n{\"sections\":[{\"title\":\"T\",\"items\":[{\"t\":\"X\",\"d\":\"Line one\nHe said \"hi\" here\"}]}]}\nDone.";
        let v = extract_json(reply).unwrap();
        let d = v["sections"][0]["items"][0]["d"].as_str().unwrap();
        assert!(d.contains("hi"));
        assert!(d.contains("Line one"));
    }

    #[test]
    fn stub_is_offline_safe() {
        let s = StubLlm;
        let out = s.complete("sys", "hello world").unwrap();
        assert!(out.contains("Offline draft"));
    }

    // ── 2.9 runtime fallback ──────────────────────────────────────────────

    struct FakeLlm {
        tag: &'static str,
        fail: bool,
    }
    impl Llm for FakeLlm {
        fn complete(&self, _s: &str, _u: &str) -> Result<String> {
            if self.fail {
                Err(Error::Other(format!("{} is down", self.tag)))
            } else {
                Ok(self.tag.to_string())
            }
        }
        fn name(&self) -> String {
            self.tag.to_string()
        }
    }

    #[test]
    fn fallback_walks_the_chain_on_runtime_failure() {
        let f = FallbackLlm::new(vec![
            Box::new(FakeLlm { tag: "primary", fail: true }),
            Box::new(FakeLlm { tag: "second", fail: false }),
            Box::new(FakeLlm { tag: "third", fail: false }),
        ]);
        assert_eq!(f.complete("s", "u").unwrap(), "second");
        // attribution follows whichever model actually served
        assert_eq!(f.name(), "second");
    }

    /// A provider that streams fixed chunks and can fail at the END, which is
    /// what separates "down before it said anything" (switch models) from "died
    /// mid-answer" (retrying would repeat what is already on screen).
    struct StreamFake {
        tag: &'static str,
        chunks: &'static [&'static str],
        fail: bool,
    }
    impl Llm for StreamFake {
        fn complete(&self, _s: &str, _u: &str) -> Result<String> {
            Ok(self.chunks.concat())
        }
        fn name(&self) -> String {
            self.tag.to_string()
        }
        fn complete_stream(
            &self,
            _s: &str,
            _u: &str,
            on_delta: &mut dyn FnMut(&str) -> bool,
        ) -> Result<RawReply> {
            for c in self.chunks {
                if !on_delta(c) {
                    break;
                }
            }
            if self.fail {
                return Err(Error::Other(format!("{} died mid-answer", self.tag)));
            }
            let text = self.chunks.concat();
            Ok(RawReply { content: text.clone(), text, finish_reason: Some("end_turn".into()), usage: None })
        }
    }

    #[test]
    fn fallback_switches_only_before_the_ui_has_anything() {
        let mut got = String::new();
        let mut sink = |d: &str| {
            got.push_str(d);
            true
        };
        // Nothing shown → the next model takes over, and the user never learns.
        let f = FallbackLlm::new(vec![
            Box::new(StreamFake { tag: "silent-dead", chunks: &[], fail: true }),
            Box::new(StreamFake { tag: "second", chunks: &["乙", "案"], fail: false }),
        ]);
        let reply = f.complete_stream("s", "u", &mut sink).unwrap();
        assert_eq!(got, "乙案");
        assert_eq!(reply.text, "乙案");
        assert_eq!(f.name(), "second");

        // Half an answer on screen → switching would print it twice, so the
        // failure is surfaced instead.
        let mut got = String::new();
        let mut sink = |d: &str| {
            got.push_str(d);
            true
        };
        let f = FallbackLlm::new(vec![
            Box::new(StreamFake { tag: "primary", chunks: &["甲", "案"], fail: true }),
            Box::new(StreamFake { tag: "second", chunks: &["乙"], fail: false }),
        ]);
        let err = f.complete_stream("s", "u", &mut sink).unwrap_err().to_string();
        assert_eq!(got, "甲案", "what the user already saw is not replayed by another model");
        assert!(err.contains("died mid-answer"), "{err}");
    }

    #[test]
    fn fallback_reports_the_last_error_when_every_model_fails() {
        let f = FallbackLlm::new(vec![
            Box::new(FakeLlm { tag: "a", fail: true }),
            Box::new(FakeLlm { tag: "b", fail: true }),
        ]);
        let err = f.complete("s", "u").unwrap_err().to_string();
        assert!(err.contains("b is down"), "{err}");
        // nothing ever served → name() still points at the primary
        assert_eq!(f.name(), "a");
    }

    #[test]
    fn chain_keeps_other_keyed_providers_on_standby_but_not_offline() {
        let online = Keys {
            openrouter: Some("sk-or".into()),
            gemini: Some("g".into()),
            ..Default::default()
        };
        let chain = build_chain("openrouter:deepseek/deepseek-v4-flash", &online).unwrap();
        assert_eq!(chain.len(), 2, "primary + gemini standby");
        assert_eq!(chain[0].name(), "openrouter:deepseek/deepseek-v4-flash");
        assert_eq!(chain[1].name(), "gemini:gemini-2.5-flash");

        // gemini as primary → openrouter stays on standby (fallback works
        // both directions; the twin model gemini-2.5-flash is skipped)
        let solo = build_chain("gemini:gemini-2.5-flash", &online).unwrap();
        assert_eq!(solo.len(), 2);
        assert_eq!(solo[1].name(), "openrouter:deepseek/deepseek-v4-flash");

        // offline: cloud standby stripped even though both keys exist
        let offline = Keys { offline: true, ..online };
        let chain = build_chain("openrouter:deepseek/deepseek-v4-flash", &offline).unwrap();
        assert_eq!(chain.len(), 1, "offline never gains cloud fallbacks");
    }
    // ---- streaming (a real socket, not a mock) ---------------------------

    /// Serve ONE canned HTTP response to one request, then close. A real socket
    /// rather than a mock, because the thing under test is "read a response body
    /// line by line while it is still arriving" — the part `send_json`, which
    /// buffers the whole body before parsing, never exercised.
    fn serve_once(status: &'static str, content_type: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            // Answer up to three attempts: the 402 retry path sends a second
            // request, and a one-shot server would turn that into a transport
            // error instead of the refusal under test.
            for _ in 0..3 {
                let Ok((mut sock, _)) = listener.accept() else { return };
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf); // request head; the contents don't matter
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes());
                let _ = sock.write_all(body.as_bytes());
                let _ = sock.flush();
            }
        });
        format!("http://{addr}")
    }

    fn sse_test_llm(url: String) -> OpenAiCompatLlm {
        OpenAiCompatLlm {
            base_url: url,
            api_key: "k".into(),
            model: "m".into(),
            label: "test",
            max_tokens: None,
        }
    }

    #[test]
    fn sse_payload_skips_comments_blanks_and_the_done_sentinel() {
        assert_eq!(sse_payload("data: {\"a\":1}\r\n"), Some("{\"a\":1}"));
        assert_eq!(sse_payload("data:[DONE]"), None);
        assert_eq!(sse_payload("data:   "), None);
        assert_eq!(sse_payload(": keep-alive"), None);
        assert_eq!(sse_payload("event: ping"), None);
    }

    #[test]
    fn openai_stream_forwards_each_delta_and_reports_the_real_end() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"光合\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"作用\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"total_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        );
        let llm = sse_test_llm(serve_once("200 OK", "text/event-stream", body));
        let mut got: Vec<String> = Vec::new();
        let reply = llm
            .complete_stream("s", "u", &mut |d| {
                got.push(d.to_string());
                true
            })
            .unwrap();
        assert_eq!(got, vec!["光合", "作用"], "each chunk reaches the UI as it lands");
        assert_eq!(reply.text, "光合作用");
        assert_eq!(reply.finish_reason.as_deref(), Some("length"));
        assert_eq!(reply.usage.unwrap()["total_tokens"], 7);
    }

    #[test]
    fn openai_stream_holds_back_inline_reasoning_instead_of_flashing_it() {
        // Reasoning models that inline <think> must not leak the markup — or the
        // reasoning itself — while the tag is still half-arrived.
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<thi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"nk>secret plan\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"</think>答案\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let llm = sse_test_llm(serve_once("200 OK", "text/event-stream", body));
        let mut got = String::new();
        let reply = llm
            .complete_stream("s", "u", &mut |d| {
                got.push_str(d);
                true
            })
            .unwrap();
        assert_eq!(got, "答案", "reasoning never reaches the bubble");
        assert_eq!(reply.text, "答案");
    }

    #[test]
    fn openai_stream_stops_reading_when_the_user_presses_stop() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"第一句\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"第二句\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let llm = sse_test_llm(serve_once("200 OK", "text/event-stream", body));
        let mut got = String::new();
        let reply = llm
            .complete_stream("s", "u", &mut |d| {
                got.push_str(d);
                false // the caller stopped after the first chunk
            })
            .unwrap();
        assert_eq!(got, "第一句");
        assert_eq!(reply.text, "第一句", "nothing past the stop is kept");
    }

    #[test]
    fn openai_stream_surfaces_a_refused_request_instead_of_an_empty_answer() {
        let llm = sse_test_llm(serve_once(
            "402 Payment Required",
            "application/json",
            "{\"error\":\"can only afford 900 tokens\"}",
        ));
        let err = llm.complete_stream("s", "u", &mut |_| true).unwrap_err().to_string();
        assert!(err.contains("402"), "{err}");
        assert!(err.contains("afford"), "the provider's own reason survives: {err}");
    }

    #[test]
    fn gemini_stream_chunks_keep_parts_and_skip_thoughts() {
        let chunk = serde_json::json!({
            "candidates": [{ "content": { "parts": [
                { "text": "甲" }, { "text": "乙" }, { "text": "想想", "thought": true }
            ]}, "finishReason": "STOP" }],
            "usageMetadata": { "totalTokenCount": 3 }
        });
        let p = gemini_piece(&chunk);
        assert_eq!(p.content.as_deref(), Some("甲乙"), "thought parts are not answer text");
        assert_eq!(p.finish.as_deref(), Some("STOP"));
        assert_eq!(p.usage.unwrap()["totalTokenCount"], 3);
    }

    #[test]
    fn claude_stream_events_carry_text_stop_usage_and_refusals() {
        let text = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "光合" }
        });
        assert_eq!(claude_piece(&text).content.as_deref(), Some("光合"));

        let stop = serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": 42 }
        });
        let p = claude_piece(&stop);
        assert_eq!(p.finish.as_deref(), Some("end_turn"));
        assert_eq!(p.usage.unwrap()["output_tokens"], 42);

        // input_tokens arrives in a different event than output_tokens; both
        // must survive, or accounting only ever records half the turn.
        let start = serde_json::json!({
            "type": "message_start",
            "message": { "usage": { "input_tokens": 1200 } }
        });
        assert_eq!(claude_piece(&start).usage.unwrap()["input_tokens"], 1200);

        let refusal = serde_json::json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "Overloaded" }
        });
        assert_eq!(claude_piece(&refusal).error.as_deref(), Some("Overloaded"));

        // Frames that carry no answer text must not produce a delta.
        for quiet in [
            serde_json::json!({ "type": "ping" }),
            serde_json::json!({ "type": "message_stop" }),
            serde_json::json!({ "type": "content_block_start", "content_block": { "type": "text", "text": "" } }),
        ] {
            let p = claude_piece(&quiet);
            assert!(p.content.is_none(), "{quiet}");
            assert!(p.error.is_none(), "{quiet}");
        }
    }

    #[test]
    fn providers_that_cannot_stream_still_hand_over_one_delta() {
        let llm = StubLlm;
        let mut got = String::new();
        let reply = llm
            .complete_stream("s", "u", &mut |d| {
                got.push_str(d);
                true
            })
            .unwrap();
        assert!(!got.is_empty(), "the non-streaming fallback must not swallow the answer");
        assert_eq!(got, reply.text);
    }
}
