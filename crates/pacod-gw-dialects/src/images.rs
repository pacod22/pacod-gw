//! OpenAI's image endpoints (`/v1/images/generations`, `/v1/images/edits`) and their
//! translation to Google's image models: Imagen through `:predict`, and Gemini's image models
//! (`…-image`) through `:generateContent` with images as output.
//!
//! An OpenAI-compatible provider is given the caller's request with its model changed; none of
//! this runs then. Translated, images always come back as base64 (`b64_json`): the gateway
//! keeps no images to link to.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use crate::error::DialectError;
use crate::multipart::Form;
use crate::util::{object, str_of, take_string, take_u64};

/// The most images one request may ask for, as OpenAI's API allows.
const MAX_IMAGES: u64 = 10;

/// An image request in neutral terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImageRequest {
    pub prompt: String,
    pub n: u64,
    pub size: Option<String>,
    pub quality: Option<String>,
    /// `url` or `b64_json`.
    pub response_format: Option<String>,
    /// `png`, `jpeg` or `webp`.
    pub output_format: Option<String>,
    /// Edits: the images to edit, as (media type, base64).
    pub images: Vec<(String, String)>,
    /// Edits: a mask was sent.
    pub mask: bool,
}

/// One image an answer holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub media_type: String,
    /// Base64.
    pub data: String,
}

/// Reads a generation request (JSON).
pub fn parse_generation(body: &[u8]) -> Result<ImageRequest, DialectError> {
    let mut top = object(body)?;
    let prompt = take_string(&mut top, "prompt").filter(|p| !p.is_empty());
    let prompt = prompt.ok_or_else(|| DialectError::invalid("prompt is required: the image to make"))?;
    let n = take_u64(&mut top, "n").unwrap_or(1);
    if !(1..=MAX_IMAGES).contains(&n) {
        return Err(DialectError::invalid(format!("n must be 1 to {MAX_IMAGES}")));
    }
    Ok(ImageRequest {
        prompt,
        n,
        size: take_string(&mut top, "size"),
        quality: take_string(&mut top, "quality"),
        response_format: take_string(&mut top, "response_format"),
        output_format: take_string(&mut top, "output_format"),
        images: Vec::new(),
        mask: false,
    })
}

/// The media type of an uploaded image: its part's type, or else its file name's.
fn media_type(content_type: Option<&str>, filename: Option<&str>) -> String {
    if let Some(t) = content_type.filter(|t| t.starts_with("image/")) {
        return t.to_owned();
    }
    let ext = filename.and_then(|f| f.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
    .to_owned()
}

/// Reads an edit request (a form). `image` may be repeated, or sent as `image[]`.
pub fn parse_edit(form: &Form, body: &[u8]) -> Result<ImageRequest, DialectError> {
    let text = |name: &str| form.text(body, name).filter(|t| !t.is_empty()).map(str::to_owned);
    let prompt = text("prompt").ok_or_else(|| DialectError::invalid("prompt is required: what to change"))?;
    let n = match text("n") {
        Some(n) => n.parse::<u64>().map_err(|_| DialectError::invalid("n must be a number"))?,
        None => 1,
    };
    if !(1..=MAX_IMAGES).contains(&n) {
        return Err(DialectError::invalid(format!("n must be 1 to {MAX_IMAGES}")));
    }
    let images: Vec<(String, String)> = form
        .parts("image")
        .chain(form.parts("image[]"))
        .map(|p| (media_type(p.content_type.as_deref(), p.filename.as_deref()), STANDARD.encode(&body[p.body.clone()])))
        .collect();
    if images.is_empty() {
        return Err(DialectError::invalid("image is required: the image to edit"));
    }
    Ok(ImageRequest {
        prompt,
        n,
        size: text("size"),
        quality: text("quality"),
        response_format: text("response_format"),
        output_format: text("output_format"),
        images,
        mask: form.part("mask").is_some(),
    })
}

/// The aspect ratio Google's models take for an OpenAI size. `None` leaves it to the model.
pub fn aspect_ratio(size: Option<&str>) -> Result<Option<&'static str>, DialectError> {
    Ok(match size {
        None | Some("auto") => None,
        Some("1024x1024" | "512x512" | "256x256") => Some("1:1"),
        Some("1792x1024" | "1536x1024") => Some("16:9"),
        Some("1024x1792" | "1024x1536") => Some("9:16"),
        Some(other) => {
            return Err(DialectError::unsupported(format!(
                "the size {other} has no aspect ratio Google's image models take; use 1024x1024, 1536x1024 or 1024x1536"
            )));
        }
    })
}

/// What a translation cannot give a caller.
fn translatable(req: &ImageRequest) -> Result<(), DialectError> {
    if req.response_format.as_deref() == Some("url") {
        return Err(DialectError::unsupported(
            "response_format url: this model's images come back as b64_json only, the gateway keeps no images to link to",
        ));
    }
    if req.mask {
        return Err(DialectError::unsupported("a mask: this model edits an image by its prompt only"));
    }
    Ok(())
}

fn mime_of(format: Option<&str>) -> Option<&'static str> {
    match format {
        Some("jpeg" | "jpg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

/// Imagen's `:predict` request, for a generation.
pub fn imagen_request(req: &ImageRequest) -> Result<Value, DialectError> {
    translatable(req)?;
    if !req.images.is_empty() {
        return Err(DialectError::unsupported(
            "an edit: Imagen edits through another API; use a Gemini image model to edit images",
        ));
    }
    let mut parameters = Map::new();
    parameters.insert("sampleCount".into(), json!(req.n));
    if let Some(r) = aspect_ratio(req.size.as_deref())? {
        parameters.insert("aspectRatio".into(), json!(r));
    }
    if let Some(m) = mime_of(req.output_format.as_deref()) {
        parameters.insert("outputOptions".into(), json!({ "mimeType": m }));
    }
    Ok(json!({ "instances": [{ "prompt": req.prompt }], "parameters": Value::Object(parameters) }))
}

/// A Gemini image model's `:generateContent` request, for one image.
pub fn gemini_request(req: &ImageRequest) -> Result<Value, DialectError> {
    translatable(req)?;
    let mut parts = vec![json!({ "text": req.prompt })];
    for (media_type, data) in &req.images {
        parts.push(json!({ "inlineData": { "mimeType": media_type, "data": data } }));
    }
    let mut config = json!({ "responseModalities": ["TEXT", "IMAGE"] });
    if let Some(r) = aspect_ratio(req.size.as_deref())? {
        config["imageConfig"] = json!({ "aspectRatio": r });
    }
    Ok(json!({ "contents": [{ "role": "user", "parts": parts }], "generationConfig": config }))
}

fn bad(what: &str) -> DialectError {
    DialectError::bad_response(format!("the provider's answer {what}"))
}

/// The images of Imagen's `:predict` answer. Imagen reports no usage: it is priced per image.
pub fn read_imagen(body: &[u8]) -> Result<Vec<Image>, DialectError> {
    let v: Value = serde_json::from_slice(body).map_err(|_| bad("is not JSON"))?;
    let images: Vec<Image> = v
        .get("predictions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Image {
                media_type: str_of(p, "mimeType").unwrap_or("image/png").to_owned(),
                data: str_of(p, "bytesBase64Encoded")?.to_owned(),
            })
        })
        .collect();
    if images.is_empty() {
        // Imagen filters an image it judges unsafe without saying why in the predictions.
        return Err(DialectError::bad_response(
            "the provider made no image (it may have filtered the prompt or the result)".to_owned(),
        ));
    }
    Ok(images)
}

/// The images of a Gemini image model's answer, and its usage.
pub fn read_gemini(body: &[u8]) -> Result<(Vec<Image>, Option<Usage>), DialectError> {
    let v: Value = serde_json::from_slice(body).map_err(|_| bad("is not JSON"))?;
    let images: Vec<Image> = v
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("inlineData").or_else(|| p.get("inline_data")))
        .filter_map(|d| {
            Some(Image {
                media_type: str_of(d, "mimeType").or_else(|| str_of(d, "mime_type")).unwrap_or("image/png").to_owned(),
                data: str_of(d, "data")?.to_owned(),
            })
        })
        .collect();
    if images.is_empty() {
        let why = v.pointer("/candidates/0/finishReason").and_then(Value::as_str).unwrap_or("no image part");
        return Err(DialectError::bad_response(format!("the provider made no image ({why})")));
    }
    let usage = v.get("usageMetadata").filter(|u| u.is_object()).map(crate::gemini::usage_of);
    Ok((images, usage))
}

/// The answer an OpenAI caller reads: `{created, data: [{b64_json}], usage?}`.
pub fn openai_answer(created: u64, images: &[Image], usage: Option<&Usage>, cost: Option<&Value>) -> Value {
    let data: Vec<Value> = images.iter().map(|i| json!({ "b64_json": i.data })).collect();
    let mut out = json!({ "created": created, "data": data });
    if let Some(u) = usage {
        let input = u.prompt_total();
        let mut usage = json!({
            "input_tokens": input,
            "input_tokens_details": { "text_tokens": input.saturating_sub(u.image_input_tokens), "image_tokens": u.image_input_tokens },
            "output_tokens": u.output_tokens,
            "total_tokens": input + u.output_tokens,
        });
        if let Some(c) = cost {
            usage["cost"] = c.clone();
        }
        out["usage"] = usage;
    }
    if let Some(format) = images.first().and_then(|i| i.media_type.strip_prefix("image/")) {
        out["output_format"] = json!(format);
    }
    out
}

/// The price variant an image request calls for, among those a price has: `<quality>-<size>`,
/// then `<quality>`, then `<size>`. Empty for the standard rates.
pub fn price_variant(req: &ImageRequest, has: impl Fn(&str) -> bool) -> String {
    let quality = req.quality.as_deref().filter(|q| *q != "auto");
    let size = req.size.as_deref().filter(|s| *s != "auto");
    let candidates =
        [quality.zip(size).map(|(q, s)| format!("{q}-{s}")), quality.map(str::to_owned), size.map(str::to_owned)];
    candidates.into_iter().flatten().find(|c| has(c)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multipart::{OwnedPart, write};
    use bytes::Bytes;

    #[test]
    fn a_generation_is_read_and_written_for_imagen_and_gemini() {
        let req =
            parse_generation(br#"{"model":"m","prompt":"a cat","n":2,"size":"1536x1024","output_format":"jpeg"}"#)
                .unwrap();
        assert_eq!((req.n, req.size.as_deref()), (2, Some("1536x1024")));
        let imagen = imagen_request(&req).unwrap();
        assert_eq!(imagen["instances"][0]["prompt"], "a cat");
        assert_eq!(imagen["parameters"]["sampleCount"], 2);
        assert_eq!(imagen["parameters"]["aspectRatio"], "16:9");
        assert_eq!(imagen["parameters"]["outputOptions"]["mimeType"], "image/jpeg");
        let gemini = gemini_request(&req).unwrap();
        assert_eq!(gemini["generationConfig"]["responseModalities"], json!(["TEXT", "IMAGE"]));
        assert_eq!(gemini["generationConfig"]["imageConfig"]["aspectRatio"], "16:9");
        assert!(parse_generation(br#"{"prompt":""}"#).is_err());
        assert!(parse_generation(br#"{"prompt":"x","n":11}"#).is_err());
    }

    #[test]
    fn what_a_translation_cannot_give_is_refused() {
        let url = parse_generation(br#"{"prompt":"x","response_format":"url"}"#).unwrap();
        assert!(matches!(imagen_request(&url), Err(DialectError::Unsupported(_))));
        let odd = parse_generation(br#"{"prompt":"x","size":"640x480"}"#).unwrap();
        assert!(matches!(gemini_request(&odd), Err(DialectError::Unsupported(_))));
    }

    #[test]
    fn an_edit_reads_its_images_from_the_form() {
        let part = |name: &str, filename: Option<&str>, body: &'static [u8]| OwnedPart {
            name: name.into(),
            filename: filename.map(str::to_owned),
            content_type: None,
            body: Bytes::from_static(body),
        };
        let body = write(
            "b",
            &[
                part("prompt", None, b"add a hat"),
                part("image[]", Some("a.jpg"), b"\xff\xd8"),
                part("image[]", Some("b.png"), b"\x89P"),
            ],
        );
        let form = Form::parse("multipart/form-data; boundary=b", &body).unwrap();
        let req = parse_edit(&form, &body).unwrap();
        assert_eq!(req.prompt, "add a hat");
        assert_eq!(
            req.images,
            vec![("image/jpeg".into(), STANDARD.encode(b"\xff\xd8")), ("image/png".into(), STANDARD.encode(b"\x89P"))]
        );
        let gemini = gemini_request(&req).unwrap();
        assert_eq!(gemini["contents"][0]["parts"][1]["inlineData"]["mimeType"], "image/jpeg");
        assert!(matches!(imagen_request(&req), Err(DialectError::Unsupported(_))), "Imagen does not edit here");
    }

    #[test]
    fn answers_are_read_and_written_as_openai_writes_them() {
        let imagen = read_imagen(br#"{"predictions":[{"bytesBase64Encoded":"QUJD","mimeType":"image/png"}]}"#).unwrap();
        assert_eq!(imagen, vec![Image { media_type: "image/png".into(), data: "QUJD".into() }]);
        assert!(read_imagen(br#"{"predictions":[]}"#).is_err());
        let (images, usage) = read_gemini(
            br#"{"candidates":[{"content":{"parts":[{"text":"Here."},{"inlineData":{"mimeType":"image/png","data":"WFla"}}]}}],
                 "usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":1300,"candidatesTokensDetails":[{"modality":"IMAGE","tokenCount":1290},{"modality":"TEXT","tokenCount":10}]}}"#,
        )
        .unwrap();
        assert_eq!(images[0].data, "WFla");
        let usage = usage.unwrap();
        assert_eq!((usage.output_tokens, usage.image_output_tokens), (1300, 1290));
        let answer = openai_answer(1_700_000_000, &images, Some(&usage), Some(&json!({ "amount": 0.04 })));
        assert_eq!(answer["data"][0]["b64_json"], "WFla");
        assert_eq!(answer["usage"]["output_tokens"], 1300);
        assert_eq!(answer["usage"]["cost"]["amount"], 0.04);
        assert_eq!(answer["output_format"], "png");
    }

    #[test]
    fn the_price_variant_is_the_most_specific_the_price_has() {
        let req = parse_generation(br#"{"prompt":"x","quality":"hd","size":"1024x1792"}"#).unwrap();
        assert_eq!(price_variant(&req, |v| v == "hd-1024x1792" || v == "hd"), "hd-1024x1792");
        assert_eq!(price_variant(&req, |v| v == "hd"), "hd");
        assert_eq!(price_variant(&req, |v| v == "1024x1792"), "1024x1792");
        assert_eq!(price_variant(&req, |_| false), "");
    }
}
