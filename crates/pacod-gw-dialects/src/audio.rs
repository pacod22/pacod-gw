//! OpenAI's speech-to-text endpoints (`/v1/audio/transcriptions`, `/v1/audio/translations`)
//! and their translation to Gemini, which hears audio through `:generateContent`.
//!
//! What a call costs is read from the provider, never estimated from the audio: tokens where
//! the answer carries them (gpt-4o-transcribe, Gemini), else the seconds it reports
//! (`usage.seconds`, `verbose_json`'s `duration`, the end of the last subtitle cue). For a
//! model priced by the second whose caller asked for `json` or `text`, the gateway asks for
//! `verbose_json` and writes the caller's format from it ([`reshape`]): only `verbose_json`
//! says how long the audio was on every provider.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use pacod_gw_events::Usage;
use serde_json::{Value, json};

use crate::error::DialectError;
use crate::multipart::Form;

/// The most audio Gemini takes inline in one request.
pub const GEMINI_INLINE_LIMIT: usize = 20 << 20;

/// A transcription request in neutral terms. The audio stays in the form.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioRequest {
    /// The audio's media type, from its part or its file name.
    pub media_type: String,
    /// Where the audio is in the body.
    pub file: std::ops::Range<usize>,
    pub language: Option<String>,
    pub prompt: Option<String>,
    /// `json` (the default), `text`, `srt`, `vtt` or `verbose_json`.
    pub response_format: String,
    pub temperature: Option<f64>,
    pub timestamps: bool,
    pub stream: bool,
}

/// The media type of an audio file: its part's type, or else its file name's.
fn media_type(content_type: Option<&str>, filename: Option<&str>) -> String {
    if let Some(t) = content_type.filter(|t| t.starts_with("audio/") || t.starts_with("video/")) {
        return t.to_owned();
    }
    let ext = filename.and_then(|f| f.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("mp3" | "mpga" | "mpeg") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("mp4") => "video/mp4",
        Some("ogg" | "oga") => "audio/ogg",
        Some("webm") => "audio/webm",
        Some("flac") => "audio/flac",
        Some("aac") => "audio/aac",
        Some("aiff" | "aif") => "audio/aiff",
        _ => "audio/wav",
    }
    .to_owned()
}

/// Reads a transcription or translation request (a form with `file`).
pub fn parse_request(form: &Form, body: &[u8]) -> Result<AudioRequest, DialectError> {
    let file = form.part("file").ok_or_else(|| DialectError::invalid("file is required: the audio to transcribe"))?;
    let text = |name: &str| form.text(body, name).filter(|t| !t.is_empty()).map(str::to_owned);
    let response_format = text("response_format").unwrap_or_else(|| "json".into());
    if !["json", "text", "srt", "vtt", "verbose_json"].contains(&response_format.as_str()) {
        return Err(DialectError::invalid("response_format must be json, text, srt, vtt or verbose_json"));
    }
    Ok(AudioRequest {
        media_type: media_type(file.content_type.as_deref(), file.filename.as_deref()),
        file: file.body.clone(),
        language: text("language"),
        prompt: text("prompt"),
        response_format,
        temperature: text("temperature").and_then(|t| t.parse().ok()),
        timestamps: form.part("timestamp_granularities[]").is_some() || form.part("timestamp_granularities").is_some(),
        stream: text("stream").as_deref() == Some("true"),
    })
}

/// The media types Gemini takes as inline audio.
fn gemini_media_type(media_type: &str) -> Option<&'static str> {
    Some(match media_type {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "audio/wav",
        "audio/mpeg" | "audio/mp3" => "audio/mp3",
        "audio/aiff" | "audio/x-aiff" => "audio/aiff",
        "audio/aac" => "audio/aac",
        "audio/ogg" => "audio/ogg",
        "audio/flac" | "audio/x-flac" => "audio/flac",
        _ => return None,
    })
}

/// Gemini's `:generateContent` request: the audio inline and an instruction that says what to
/// do with it. `translate` asks for English, as OpenAI's translations endpoint answers.
pub fn gemini_request(req: &AudioRequest, body: &[u8], translate: bool) -> Result<Value, DialectError> {
    if matches!(req.response_format.as_str(), "srt" | "vtt" | "verbose_json") || req.timestamps {
        return Err(DialectError::unsupported(format!(
            "response_format {} and timestamps: this model gives no reliable timings; use json or text",
            req.response_format
        )));
    }
    if req.stream {
        return Err(DialectError::unsupported("stream: this model's transcription is answered whole"));
    }
    let media_type = gemini_media_type(&req.media_type).ok_or_else(|| {
        DialectError::unsupported(format!(
            "audio of type {}: this model takes WAV, MP3, AIFF, AAC, OGG and FLAC",
            req.media_type
        ))
    })?;
    let mut instruction = if translate {
        "Translate the speech in this audio into English. Answer with the translation only, without comments."
            .to_owned()
    } else {
        "Transcribe the speech in this audio verbatim. Answer with the transcript only, without comments.".to_owned()
    };
    if let Some(l) = &req.language {
        instruction.push_str(&format!(" The speech is in the language with the code {l}."));
    }
    if let Some(p) = &req.prompt {
        instruction.push_str(&format!(" Context, for names and spelling: {p}"));
    }
    let mut out = json!({
        "contents": [{ "role": "user", "parts": [
            { "inlineData": { "mimeType": media_type, "data": STANDARD.encode(&body[req.file.clone()]) } },
            { "text": instruction }
        ] }]
    });
    if let Some(t) = req.temperature {
        out["generationConfig"] = json!({ "temperature": t });
    }
    Ok(out)
}

/// The transcript of Gemini's answer, and its usage.
pub fn read_gemini(body: &[u8]) -> Result<(String, Option<Usage>), DialectError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| DialectError::bad_response("the provider's answer is not JSON".to_owned()))?;
    let text: String = v
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    let usage = v.get("usageMetadata").filter(|u| u.is_object()).map(crate::gemini::usage_of);
    Ok((text.trim().to_owned(), usage))
}

/// The answer in the caller's format, from a transcript and its usage: `json` carries the
/// usage as OpenAI writes it, `text` is the transcript alone, with a line end after it as
/// OpenAI's reference shows it (not checked against a recorded answer).
pub fn answer(text: &str, format: &str, usage: Option<&Usage>, seconds: Option<f64>) -> (Vec<u8>, &'static str) {
    if format == "text" {
        return (format!("{text}\n").into_bytes(), "text/plain; charset=utf-8");
    }
    let mut out = json!({ "text": text });
    match (usage, seconds) {
        (Some(u), _) if u.input_tokens + u.output_tokens > 0 => {
            out["usage"] = json!({
                "type": "tokens",
                "input_tokens": u.prompt_total(),
                "input_token_details": { "text_tokens": u.prompt_total().saturating_sub(u.audio_input_tokens), "audio_tokens": u.audio_input_tokens },
                "output_tokens": u.output_tokens,
                "total_tokens": u.prompt_total() + u.output_tokens,
            });
        }
        (_, Some(s)) => out["usage"] = json!({ "type": "duration", "seconds": s.ceil() as u64 }),
        _ => {}
    }
    (out.to_string().into_bytes(), "application/json")
}

/// The caller's format written from a `verbose_json` answer, which holds the transcript and
/// the audio's duration. `None` when the answer is not one.
pub fn reshape(verbose: &[u8], format: &str) -> Option<(Vec<u8>, &'static str)> {
    let v: Value = serde_json::from_slice(verbose).ok()?;
    let text = v.get("text")?.as_str()?;
    let seconds = v.get("duration").and_then(Value::as_f64);
    Some(answer(text, format, None, seconds))
}

/// The milliseconds of audio an answer reports: `usage.seconds`, `verbose_json`'s `duration`,
/// or the end of the last cue of an SRT or WebVTT answer.
pub fn duration_ms(body: &[u8]) -> Option<u64> {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        let seconds = v
            .pointer("/usage/seconds")
            .and_then(Value::as_f64)
            .filter(|_| v.pointer("/usage/type").and_then(Value::as_str) == Some("duration"))
            .or_else(|| v.get("duration").and_then(Value::as_f64))?;
        return Some((seconds * 1000.0).round() as u64);
    }
    last_cue_end(std::str::from_utf8(body).ok()?)
}

/// The end of the last cue of SRT (`00:00:01,500 --> 00:00:03,250`) or WebVTT
/// (`00:01.500 --> 00:03.250`): a little short of the audio, which may go on in silence.
fn last_cue_end(text: &str) -> Option<u64> {
    let line = text.lines().rev().find(|l| l.contains("-->"))?;
    let end = line.split("-->").nth(1)?.split_whitespace().next()?;
    let (clock, millis) = end.split_once([',', '.'])?;
    let mut seconds = 0u64;
    for field in clock.split(':') {
        seconds = seconds * 60 + field.parse::<u64>().ok()?;
    }
    Some(seconds * 1000 + millis.parse::<u64>().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multipart::{OwnedPart, content_type, write};
    use bytes::Bytes;

    fn form(extra: &[(&str, &str)]) -> (Form, Bytes) {
        let mut parts = vec![OwnedPart {
            name: "file".into(),
            filename: Some("hello.mp3".into()),
            content_type: Some("application/octet-stream".into()),
            body: Bytes::from_static(b"ID3audio"),
        }];
        for (k, v) in extra {
            parts.push(OwnedPart {
                name: (*k).into(),
                filename: None,
                content_type: None,
                body: Bytes::copy_from_slice(v.as_bytes()),
            });
        }
        let body = write("b", &parts);
        (Form::parse(&content_type("b"), &body).unwrap(), body)
    }

    #[test]
    fn a_request_is_read_from_its_form() {
        let (f, body) = form(&[("model", "whisper-1"), ("language", "de"), ("response_format", "text")]);
        let req = parse_request(&f, &body).unwrap();
        assert_eq!((req.media_type.as_str(), req.response_format.as_str()), ("audio/mpeg", "text"));
        assert_eq!(&body[req.file.clone()], b"ID3audio");
        assert_eq!(req.language.as_deref(), Some("de"));
        let (f, body) = form(&[("response_format", "doc")]);
        assert!(parse_request(&f, &body).is_err());
    }

    #[test]
    fn gemini_is_asked_with_the_audio_inline() {
        let (f, body) = form(&[("prompt", "Names: Zoë"), ("temperature", "0")]);
        let req = parse_request(&f, &body).unwrap();
        let v = gemini_request(&req, &body, false).unwrap();
        let parts = &v["contents"][0]["parts"];
        assert_eq!(parts[0]["inlineData"]["mimeType"], "audio/mp3");
        assert_eq!(parts[0]["inlineData"]["data"], STANDARD.encode(b"ID3audio"));
        assert!(parts[1]["text"].as_str().unwrap().contains("Zoë"));
        assert_eq!(v["generationConfig"]["temperature"], 0.0);
        let translated = gemini_request(&req, &body, true).unwrap();
        assert!(translated["contents"][0]["parts"][1]["text"].as_str().unwrap().contains("English"));
        // What Gemini cannot give.
        for extra in [[("response_format", "srt")], [("timestamp_granularities[]", "word")], [("stream", "true")]] {
            let (f, body) = form(&extra);
            let req = parse_request(&f, &body).unwrap();
            assert!(matches!(gemini_request(&req, &body, false), Err(DialectError::Unsupported(_))), "{extra:?}");
        }
    }

    #[test]
    fn answers_carry_the_usage_the_provider_reported() {
        let (text, usage) = read_gemini(
            br#"{"candidates":[{"content":{"parts":[{"text":"Hello there."}]}}],"usageMetadata":{"promptTokenCount":120,"candidatesTokenCount":4,"promptTokensDetails":[{"modality":"AUDIO","tokenCount":100},{"modality":"TEXT","tokenCount":20}]}}"#,
        )
        .unwrap();
        assert_eq!(text, "Hello there.");
        let usage = usage.unwrap();
        assert_eq!(usage.audio_input_tokens, 100);
        let (json, kind) = answer(&text, "json", Some(&usage), None);
        let v: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(
            (kind, v["usage"]["type"].as_str(), v["usage"]["input_token_details"]["audio_tokens"].as_u64()),
            ("application/json", Some("tokens"), Some(100))
        );
        assert_eq!(answer(&text, "text", Some(&usage), None).0, b"Hello there.\n");
    }

    #[test]
    fn the_duration_comes_from_what_the_provider_says() {
        assert_eq!(duration_ms(br#"{"text":"x","usage":{"type":"duration","seconds":3}}"#), Some(3000));
        assert_eq!(duration_ms(br#"{"task":"transcribe","duration":8.47,"text":"x"}"#), Some(8470));
        assert_eq!(duration_ms(br#"{"text":"x","usage":{"type":"tokens","input_tokens":3}}"#), None);
        assert_eq!(
            duration_ms(b"1\n00:00:00,000 --> 00:00:01,500\nHi\n\n2\n00:00:01,500 --> 00:01:02,250\nthere\n"),
            Some(62_250)
        );
        assert_eq!(duration_ms(b"WEBVTT\n\n00:00.000 --> 00:04.100\nHi\n"), Some(4_100));
        assert_eq!(duration_ms(b"just text\n"), None);
    }

    #[test]
    fn verbose_json_is_written_as_the_callers_format() {
        let verbose = br#"{"task":"transcribe","language":"english","duration":8.2,"text":"Hello.","segments":[]}"#;
        let (json, _) = reshape(verbose, "json").unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&json).unwrap(),
            json!({ "text": "Hello.", "usage": { "type": "duration", "seconds": 9 } })
        );
        assert_eq!(reshape(verbose, "text").unwrap().0, b"Hello.\n");
        assert!(reshape(b"Hello.", "json").is_none());
    }
}
