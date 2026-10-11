//! The committed provider fixtures: well formed, scrubbed, and free of anything like a key.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn fixtures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut files = Vec::new();
    for dir in std::fs::read_dir(&root).expect("the fixtures directory exists").flatten() {
        if dir.path().is_dir() {
            for file in std::fs::read_dir(dir.path()).unwrap().flatten() {
                if file.path().extension().is_some_and(|e| e == "json") {
                    files.push(file.path());
                }
            }
        }
    }
    files.sort();
    files
}

/// Shapes of the providers' keys: OpenAI and DeepSeek `sk-…`, Anthropic `sk-ant-…`, Google `AIza…`,
/// z.ai `<32 hex>.<16 alphanumerics>`.
fn looks_like_a_key(text: &str) -> Option<String> {
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')) {
        let long = word.len() >= 30;
        if long && (word.starts_with("sk-") || word.starts_with("AIza")) {
            return Some(word.chars().take(6).collect());
        }
        if let Some((a, b)) = word.split_once('.')
            && a.len() == 32
            && a.chars().all(|c| c.is_ascii_hexdigit())
            && b.len() >= 16
            && b.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Some(a.chars().take(6).collect());
        }
    }
    None
}

#[test]
fn every_fixture_is_scrubbed() {
    for file in fixtures() {
        let text = std::fs::read_to_string(&file).unwrap();
        let name = file.display();
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: not JSON: {e}"));
        let source = v["source"].as_str().unwrap_or_default();
        assert!(matches!(source, "recorded" | "docs"), "{name}: source {source:?}");
        if source == "docs" {
            assert!(
                v["doc_url"].as_str().is_some_and(|u| u.starts_with("https://")),
                "{name}: a docs fixture names its source"
            );
        }
        for key in ["provider", "case"] {
            assert!(v[key].is_string(), "{name}: no {key}");
        }
        assert!(v["response"]["status"].is_u64(), "{name}: no status");
        for (header, value) in v["response"]["headers"].as_object().into_iter().flatten() {
            let kept = matches!(header.as_str(), "content-type" | "retry-after");
            let named = header.starts_with("x-ratelimit-") && value == "fx";
            assert!(kept || named, "{name}: header {header} is not on the allow-list");
        }
        assert!(looks_like_a_key(&text).is_none(), "{name}: something that looks like a key");
        assert!(!text.contains("Bearer "), "{name}: an authorization value");
        // A service account's key, its address, or an access token minted from it.
        assert!(!text.contains("-----BEGIN"), "{name}: a private key");
        assert!(!text.contains("iam.gserviceaccount.com"), "{name}: a service account's address");
        assert!(!text.contains("ya29."), "{name}: a Google access token");
    }
}

#[test]
fn the_key_check_finds_keys() {
    assert!(looks_like_a_key("x sk-0123456789abcdef0123456789abcdef y").is_some());
    assert!(looks_like_a_key("AIzaSyA0123456789abcdefghijklmnopqrstu").is_some());
    assert!(looks_like_a_key("0123456789abcdef0123456789abcdef.AbCdEfGh12345678").is_some());
    assert!(looks_like_a_key("a thought signature c2lnbmF0dXJl and fx-12").is_none());
}
