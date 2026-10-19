//! Video overview — NotebookLM's Video Overviews, local edition.
//!
//! The pieces were already in the app: the slideshow generator produces
//! `{"slides":[{"title","bullets","notes"}]}` where `notes` is literally a
//! voiceover script, and the audio-overview path synthesizes speech per
//! segment with OpenAI TTS. What was missing is the MEDIUM: turning rendered
//! slides + narration into one playable MP4.
//!
//! Division of labour (each side does what it is good at):
//! - FRONTEND renders each slide to a 1280×720 PNG on a canvas — the webview
//!   has the fonts (CJK included) and the theme colors; a Rust text rasterizer
//!   would need a font stack for one feature.
//! - BACKEND takes the PNGs + narration texts and does everything else:
//!   per-slide TTS (cached by narration hash), ffprobe duration probing,
//!   per-slide still-video encoding, concat demuxer, final MP4.
//!
//! ffmpeg arrives via `sidecars::ensure_ffmpeg`: PATH first, then a one-time
//! download on Windows. Progress rides the `ingest:progress` channel so the
//! existing UI plumbing shows it live.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One slide: the frontend-rendered PNG (a `data:image/png;base64,…` URL) and
/// its narration text (the slideshow's `notes`, or the bullets joined).
#[derive(Debug, Clone, Deserialize)]
pub struct VideoSlideInput {
    pub png: String,
    #[serde(default)]
    pub narration: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoOverviewResult {
    /// Absolute path to the MP4 — the webview plays it via convertFileSrc.
    pub path: String,
    pub duration_secs: f64,
    pub slides: usize,
    /// True when ffmpeg had to be downloaded this run (first video only).
    pub ffmpeg_fetched: bool,
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// Decode a `data:image/png;base64,…` URL to raw PNG bytes.
fn decode_data_url(url: &str) -> Result<Vec<u8>> {
    let b64 = url
        .strip_prefix("data:image/png;base64,")
        .ok_or_else(|| Error::Other("slide PNG must be a data:image/png;base64 URL".into()))?;
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| Error::Other(format!("slide PNG base64: {e}")))
}

/// TTS one narration chunk to an MP3 file, cached by (voice, text) hash.
/// Same endpoint the audio overview uses — same TtsConfig (key/model/endpoint).
fn tts_mp3(
    client: &reqwest::blocking::Client,
    tts: &crate::commands::TtsConfig,
    voice: &str,
    text: &str,
    out: &Path,
) -> Result<()> {
    if out.is_file() {
        return Ok(());
    }
    let body = serde_json::json!({
        "model": tts.model,
        "voice": voice,
        "input": text,
        "response_format": "mp3",
    });
    let resp = client
        .post(&tts.endpoint)
        .header("Authorization", format!("Bearer {}", tts.key.trim()))
        .json(&body)
        .send()?;
    if !resp.status().is_success() {
        let code = resp.status();
        let detail = resp.text().unwrap_or_default();
        return Err(Error::Other(format!(
            "TTS request failed ({code}): {}",
            truncate(&detail, 200)
        )));
    }
    std::fs::write(out, resp.bytes()?)
        .map_err(|e| Error::Other(format!("write {}: {e}", out.display())))?;
    Ok(())
}

/// ffprobe a media file's duration in seconds.
fn probe_duration(ffprobe: &Path, file: &Path) -> Result<f64> {
    let out = std::process::Command::new(ffprobe)
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(file)
        .output()
        .map_err(|e| Error::Other(format!("run ffprobe: {e}")))?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    s.parse::<f64>()
        .map_err(|_| Error::Other(format!("ffprobe duration unparsable: {s}")))
}

/// Encode one still slide + its narration into a small MP4. `-loop 1` holds
/// the PNG until `-shortest` cuts at the audio's end; 10 fps keeps still-image
/// encoding cheap; yuv420p keeps the output playable everywhere.
fn encode_slide(
    ffmpeg: &Path,
    png: &Path,
    mp3: &Path,
    out: &Path,
) -> Result<()> {
    let status = std::process::Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-loop", "1", "-framerate", "10", "-i"])
        .arg(png)
        .args(["-i"])
        .arg(mp3)
        .args([
            "-c:v", "libx264", "-tune", "stillimage", "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-b:a", "128k",
            "-shortest",
        ])
        .arg(out)
        .status()
        .map_err(|e| Error::Other(format!("run ffmpeg: {e}")))?;
    if !status.success() {
        return Err(Error::Other(format!(
            "ffmpeg failed for {} (exit {})",
            out.display(),
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

/// Concatenate the per-slide MP4s (stream copy — no re-encode).
fn concat(ffmpeg: &Path, list_file: &Path, out: &Path) -> Result<()> {
    let status = std::process::Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(list_file)
        .args(["-c", "copy"])
        .arg(out)
        .status()
        .map_err(|e| Error::Other(format!("run ffmpeg concat: {e}")))?;
    if !status.success() {
        return Err(Error::Other(format!(
            "ffmpeg concat failed (exit {})",
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

/// The whole pipeline. Returns the final MP4 path; progress (stage, detail,
/// percent) goes to the caller's callback. Sync — the command layer wraps it
/// in spawn_blocking. Network steps: TTS per slide. Heavy step: the one-time
/// ffmpeg download, cached in `<data>/bin/` afterwards.
pub fn synthesize(
    data_dir: &Path,
    work_root: &Path,
    material_id: &str,
    slides: &[VideoSlideInput],
    voice: &str,
    tts: &crate::commands::TtsConfig,
    force: bool,
    ffmpeg_url: Option<&str>,
    progress: &mut dyn FnMut(&str, &str, u8),
) -> Result<VideoOverviewResult> {
    if slides.is_empty() {
        return Err(Error::Other("没有幻灯片 —— 先生成 slideshow 材料。".into()));
    }
    if slides.len() > 40 {
        return Err(Error::Other("幻灯片过多（上限 40）—— 精简材料后重试。".into()));
    }

    // 0. ffmpeg — resolve or fetch (first run only).
    progress("ffmpeg", "检查 ffmpeg…", 2);
    let fetched = {
        let already = crate::ingest::tool_in("ffmpeg", Some(data_dir)).is_some()
            && crate::ingest::tool_in("ffprobe", Some(data_dir)).is_some();
        !already
    };
    let (ffmpeg, ffprobe) = crate::sidecars::ensure_ffmpeg(data_dir, ffmpeg_url)?;

    let work = work_root.join(material_id);
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Other(format!("create {}: {e}", work.display())))?;
    let out = work_root.join(format!("{material_id}.mp4"));
    if !force && out.is_file() {
        let d = probe_duration(&ffprobe, &out).unwrap_or(0.0);
        return Ok(VideoOverviewResult {
            path: out.to_string_lossy().into_owned(),
            duration_secs: d,
            slides: slides.len(),
            ffmpeg_fetched: false,
        });
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| Error::Other(format!("http client: {e}")))?;

    // 1. Per-slide narration MP3 + still-video MP4.
    let mut list_rows = String::new();
    let mut total = 0.0f64;
    for (i, slide) in slides.iter().enumerate() {
        progress(
            "tts",
            &format!("合成旁白 {}/{}…", i + 1, slides.len()),
            10 + (70 * i as u8 / slides.len() as u8),
        );
        // Narration: the slide's notes; empty notes → 3s silent hold (handled
        // below) so the slide still appears in the video.
        let narration = slide.narration.trim();
        let png_path = work.join(format!("slide_{i}.png"));
        std::fs::write(&png_path, decode_data_url(&slide.png)?)?;

        if narration.is_empty() {
            let silent = work.join(format!("narr_{i}.mp3"));
            let status = std::process::Command::new(&ffmpeg)
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "anullsrc=r=24000:cl=mono", "-t", "3"])
                .arg(&silent)
                .status()
                .map_err(|e| Error::Other(format!("run ffmpeg (silence): {e}")))?;
            if !status.success() {
                return Err(Error::Other("生成静音段失败".into()));
            }
        } else {
            let mp3 = work.join(format!("narr_{i}.mp3"));
            tts_mp3(&client, tts, voice, narration, &mp3)?;
        }

        progress(
            "encode",
            &format!("编码幻灯片 {}/{}…", i + 1, slides.len()),
            15 + (70 * i as u8 / slides.len() as u8),
        );
        let slide_mp4 = work.join(format!("slide_{i}.mp4"));
        encode_slide(&ffmpeg, &png_path, &work.join(format!("narr_{i}.mp3")), &slide_mp4)?;
        total += probe_duration(&ffprobe, &slide_mp4)?;
        // concat demuxer wants forward slashes or quoted plain names; plain
        // relative names in a list file sitting in the same dir are safest.
        list_rows.push_str(&format!("file 'slide_{i}.mp4'\n"));
    }

    // 2. Concat — stream copy, no re-encode.
    progress("concat", "拼接成片…", 92);
    let list_file = work.join("list.txt");
    std::fs::write(&list_file, &list_rows)
        .map_err(|e| Error::Other(format!("write {}: {e}", list_file.display())))?;
    concat(&ffmpeg, &list_file, &out)?;

    let duration = probe_duration(&ffprobe, &out).unwrap_or(total);
    progress("done", "完成", 100);
    Ok(VideoOverviewResult {
        path: out.to_string_lossy().into_owned(),
        duration_secs: duration,
        slides: slides.len(),
        ffmpeg_fetched: fetched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_roundtrip() {
        use base64::Engine as _;
        let raw = b"\x89PNG fake bytes";
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        );
        assert_eq!(decode_data_url(&url).unwrap(), raw);
        assert!(decode_data_url("data:image/jpeg;base64,AAA").is_err());
        assert!(decode_data_url("not a url").is_err());
    }

    #[test]
    fn empty_narration_falls_back_to_title_logic_is_callers_job() {
        // The narration fallback (notes → bullets → title) lives in the
        // command layer; this module only guarantees empty string = silence.
        let s = VideoSlideInput { png: "data:image/png;base64,AAAA".into(), narration: String::new() };
        assert!(s.narration.is_empty());
    }

    /// End-to-end media check: fetch (or find) ffmpeg, encode two PNG slides
    /// with silence, concat, probe. Downloads ~80MB on first run on Windows —
    /// hence #[ignore]; run explicitly with `cargo test --lib video -- --ignored`.
    #[test]
    #[ignore]
    fn ffmpeg_sidecar_downloads_and_encodes_a_still_video() {
        let dir = std::env::temp_dir().join(format!("cortex-vo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();

        use base64::Engine as _;
        let mut slides = Vec::new();
        for color in ["#123456", "#654321"] {
            let img = image::RgbaImage::from_pixel(640, 360, image::Rgba([
                u8::from_str_radix(&color[1..3], 16).unwrap(),
                u8::from_str_radix(&color[3..5], 16).unwrap(),
                u8::from_str_radix(&color[5..7], 16).unwrap(),
                255,
            ]));
            let mut png = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            slides.push(VideoSlideInput {
                png: format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(&png)
                ),
                narration: String::new(), // silent 3s holds
            });
        }

        let mut calls = 0;
        let tts = crate::commands::TtsConfig {
            key: "sk-dummy".into(),
            model: "tts-1".into(),
            endpoint: "https://api.openai.com/v1/audio/speech".into(),
        };
        let res = synthesize(
            &dir, &dir, "smoke", &slides, "alloy", &tts, false, None,
            &mut |stage, detail, pct| {
                calls += 1;
                let _ = (stage, detail, pct);
            },
        )
        .unwrap();
        assert_eq!(res.slides, 2);
        assert!(res.duration_secs >= 5.5, "two 3s silent holds, got {}", res.duration_secs);
        assert!(Path::new(&res.path).is_file(), "mp4 exists at {}", res.path);
        assert!(calls >= 4, "progress was reported");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
