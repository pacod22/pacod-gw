//! OpenAI's speech-to-text endpoints: `/v1/audio/transcriptions` and `/v1/audio/translations`.
//!
//! The transcript is derived from the file's name and size, so that tests can predict it. The
//! audio lasts one second per 16,000 bytes, at least one. Whisper models answer in every
//! format and report seconds (`usage` with `type: duration` in `json`, `duration` in
//! `verbose_json`); `gpt-4o*-transcribe` models answer `json` and `text` only, report tokens,
//! and stream with `stream=true`.

use axum::http::HeaderMap;
use serde_json::json;

use super::{
    objects::form_parts,
    reply::count_tokens,
    server::{ApiError, Call, Reply},
    stream::{Frame, sse},
};

fn clock(ms: u64, separator: char, hours: bool) -> String {
    let (h, m, s, f) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if hours { format!("{h:02}:{m:02}:{s:02}{separator}{f:03}") } else { format!("{m:02}:{s:02}{separator}{f:03}") }
}

/// `POST /v1/audio/transcriptions`, and `/translations` when `translate`.
pub(crate) fn transcribe(call: &Call<'_>, headers: &HeaderMap, translate: bool) -> Result<Reply, ApiError> {
    let parts = form_parts(headers, call.body)?;
    let text =
        |name: &str| parts.iter().find(|p| p.name == name).map(|p| String::from_utf8_lossy(&p.body).trim().to_owned());
    let file = parts.iter().find(|p| p.name == "file").ok_or_else(|| ApiError::bad_request("file: Field required"))?;
    let model = text("model").ok_or_else(|| ApiError::bad_request("model: Field required"))?;
    let format = text("response_format").unwrap_or_else(|| "json".into());
    let seconds = (file.body.len() as f64 / 16_000.0).max(1.0);
    let ms = (seconds * 1000.0).round() as u64;
    let name = file.filename.clone().unwrap_or_else(|| "audio".into());
    let transcript = if translate { format!("English translation of {name}") } else { format!("Transcript of {name}") };
    let tokens = !model.contains("whisper");
    if tokens && !["json", "text"].contains(&format.as_str()) {
        return Err(ApiError::bad_request(format!(
            "response_format '{format}' is not compatible with model '{model}'. Use 'json' or 'text'."
        )));
    }
    let usage = if tokens {
        let audio = (seconds * 10.0).ceil() as u64;
        let prompt = text("prompt").as_deref().map_or(0, count_tokens);
        json!({
            "type": "tokens",
            "input_tokens": audio + prompt,
            "input_token_details": { "text_tokens": prompt, "audio_tokens": audio },
            "output_tokens": count_tokens(&transcript),
            "total_tokens": audio + prompt + count_tokens(&transcript),
        })
    } else {
        json!({ "type": "duration", "seconds": seconds.ceil() as u64 })
    };
    if tokens && text("stream").as_deref() == Some("true") {
        let mut frames = Vec::new();
        for word in transcript.split_inclusive(' ') {
            frames.push(Frame::content(sse(None, &json!({ "type": "transcript.text.delta", "delta": word }))));
        }
        frames.push(Frame::meta(sse(
            None,
            &json!({ "type": "transcript.text.done", "text": transcript, "usage": usage }),
        )));
        return Ok(Reply::Stream { content_type: "text/event-stream; charset=utf-8", frames });
    }
    let cue =
        |separator: char, hours: bool| format!("{} --> {}", clock(0, separator, hours), clock(ms, separator, hours));
    Ok(match format.as_str() {
        "text" => Reply::Bytes { content_type: "text/plain; charset=utf-8", body: format!("{transcript}\n").into() },
        "srt" => Reply::Bytes {
            content_type: "text/plain; charset=utf-8",
            body: format!("1\n{}\n{transcript}\n\n", cue(',', true)).into(),
        },
        "vtt" => Reply::Bytes {
            content_type: "text/plain; charset=utf-8",
            body: format!("WEBVTT\n\n{}\n{transcript}\n\n", cue('.', true)).into(),
        },
        "verbose_json" => Reply::Json(json!({
            "task": if translate { "translate" } else { "transcribe" },
            "language": "english",
            "duration": seconds,
            "text": transcript,
            "segments": [{ "id": 0, "seek": 0, "start": 0.0, "end": seconds, "text": transcript }],
        })),
        _ => Reply::Json(json!({ "text": transcript, "usage": usage })),
    })
}
