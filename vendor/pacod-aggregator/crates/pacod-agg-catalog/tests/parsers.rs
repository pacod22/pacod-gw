//! Every reader against a sample of the real thing (trimmed copies of OpenRouter's and LiteLLM's
//! lists, taken 2026-11-06), and the catalogue against a file that uses every field.

use pacod_agg_catalog::{Catalog, Format, Found, ModelKind, ParseError, SourceKind, Support, detect, parse};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

fn model<'a>(c: &'a Catalog, vendor: &str, api_id: &str) -> &'a pacod_agg_catalog::Model {
    c.models.iter().find(|m| m.vendor.as_deref() == Some(vendor) && m.api_id == api_id).unwrap_or_else(|| {
        panic!("no {vendor}/{api_id} in {:?}", c.models.iter().map(|m| &m.api_id).collect::<Vec<_>>())
    })
}

#[test]
fn every_format_is_told_apart() {
    for (file, format) in [
        ("catalog-v1.json", Format::Catalog),
        ("openrouter-sample.json", Format::Openrouter),
        ("litellm-sample.json", Format::Litellm),
        ("pacod-feed-sample.json", Format::PacodFeed),
    ] {
        let v: serde_json::Value = serde_json::from_slice(&fixture(file)).unwrap();
        assert_eq!(detect(&v), Some(format), "{file}");
    }
    assert!(matches!(parse(b"{\"something\": 1}", None), Err(ParseError::Unknown)));
    assert!(matches!(parse(b"not json", None), Err(ParseError::Json(_))));
    let later = br#"{"schema": "pacod.catalog/v2", "models": []}"#;
    assert!(matches!(parse(later, None), Err(ParseError::Schema(s)) if s == "pacod.catalog/v2"));
}

#[test]
fn openrouter_gives_prices_per_million_tiers_properties_and_parameters() {
    let c = parse(&fixture("openrouter-sample.json"), None).unwrap();
    assert_eq!(c.source.kind, SourceKind::Openrouter);
    // A `:free` offer and the router are not models.
    assert!(c.models.iter().all(|m| !m.api_id.contains(':') && m.api_id != "auto"), "{:?}", c.models);
    assert_eq!(c.models.len(), 5);

    let haiku = model(&c, "anthropic", "claude-haiku-5.5");
    assert_eq!(haiku.aliases, ["claude-haiku-5.5-20261007"]);
    assert_eq!(haiku.display_name.as_deref(), Some("Claude Haiku 5.5"));
    assert_eq!(haiku.kind, Some(ModelKind::Chat));
    let p = haiku.price.as_ref().unwrap();
    assert_eq!((p.rates.input, p.rates.output, p.rates.cache_read), (Some(0.1), Some(0.5), Some(0.01)));
    assert_eq!((p.rates.cache_write_5m, p.rates.cache_write_1h), (Some(0.125), Some(0.2)));
    assert_eq!(p.rates.per_server_tool_call, Some(0.01));
    assert_eq!(p.tiers.len(), 1);
    assert_eq!((p.tiers[0].above_input_tokens, p.tiers[0].rates.input), (100_000, Some(0.5)));
    let props = &haiku.properties;
    assert_eq!((props.context_window, props.max_output_tokens), (Some(1_000_000), Some(128_000)));
    assert_eq!(props.modalities.as_ref().unwrap().input, ["text", "image", "document"]);
    assert_eq!((props.vision, props.documents, props.tools), (Some(true), Some(true), Some(true)));
    assert_eq!((props.structured_output, props.json_mode, props.reasoning), (Some(true), Some(true), Some(true)));
    assert_eq!(haiku.parameters["tools"].support, Some(Support::Accepted));
    assert!(!haiku.parameters.contains_key("include_reasoning"), "OpenRouter's own switches are not parameters");

    // The author is the vendor, by the catalogue's id for it.
    assert!(c.models.iter().any(|m| m.vendor.as_deref() == Some("xai")));
    assert!(c.vendors.iter().any(|v| v.id == "xai" && v.display_name.as_deref() == Some("xAI")));
}

#[test]
fn litellm_gives_the_vendors_own_entries_once_with_variants_tiers_and_off_peak_hours() {
    let c = parse(&fixture("litellm-sample.json"), None).unwrap();
    assert_eq!(c.source.kind, SourceKind::Litellm);
    // Azure's copy of an OpenAI model, the video model and the sample spec are not read; DeepSeek's
    // two keys for one model are one model.
    assert!(c.models.iter().all(|m| m.api_id != "gpt-5-mini" || m.vendor.as_deref() == Some("openai")));
    assert_eq!(c.models.iter().filter(|m| m.api_id == "gpt-5-mini").count(), 1);
    assert!(c.models.iter().all(|m| !m.api_id.contains("sora")));
    assert_eq!(c.models.iter().filter(|m| m.api_id == "deepseek-chat").count(), 1);

    let sonnet = model(&c, "anthropic", "claude-sonnet-4-5");
    assert_eq!(sonnet.successor.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(sonnet.sunset_at.map(|d| d.date_naive().to_string()).as_deref(), Some("2026-11-30"));
    let p = sonnet.price.as_ref().unwrap();
    assert_eq!((p.rates.input, p.rates.output, p.rates.cache_write_1h), (Some(3.0), Some(15.0), Some(6.0)));
    assert_eq!(p.variants["batch"].input, Some(1.5));
    assert!(c.models.iter().any(|m| m.api_id == "claude-sonnet-4-5-20250929"), "a snapshot is its own entry");

    let gemini = model(&c, "google", "gemini-2.5-pro");
    let p = gemini.price.as_ref().unwrap();
    assert_eq!((p.rates.input, p.rates.per_server_tool_call), (Some(1.25), Some(0.035)));
    assert_eq!(p.variants["priority"].input, Some(2.25));
    assert_eq!(p.tiers.len(), 1);
    let tier = &p.tiers[0];
    assert_eq!((tier.above_input_tokens, tier.rates.input, tier.rates.output), (200_000, Some(2.5), Some(15.0)));
    assert_eq!(tier.variants["priority"].input, Some(4.5));
    assert_eq!(gemini.properties.context_window, Some(1_048_576));

    let flash = model(&c, "deepseek", "deepseek-flash");
    let p = flash.price.as_ref().unwrap();
    assert_eq!(p.variants["off_peak"].input, Some(0.15));
    assert_eq!(p.windows.len(), 2);
    assert_eq!(p.windows[1].hours_utc, ["00:00-00:00"]);
    assert_eq!(p.windows[1].weekdays, [6, 7]);

    // The provider's prefix is not part of the API id.
    model(&c, "zai", "glm-5");
    model(&c, "groq", "openai/gpt-oss-120b");
    assert_eq!(model(&c, "openai", "text-embedding-3-small").kind, Some(ModelKind::Embedding));
}

#[test]
fn a_pacod_feed_names_its_vendor_in_the_key_or_not_at_all() {
    let c = parse(&fixture("pacod-feed-sample.json"), None).unwrap();
    let x = model(&c, "openai", "gpt-x");
    let p = x.price.as_ref().unwrap();
    assert_eq!((p.rates.input, p.variants["batch"].output, p.tiers[0].rates.input), (Some(2.0), Some(5.0), Some(4.0)));
    assert_eq!((x.properties.context_window, x.properties.vision), (Some(400_000), Some(true)));
    let mini = c.models.iter().find(|m| m.api_id == "gpt-x-mini").unwrap();
    assert_eq!(mini.vendor, None);
    assert!(c.index().find("openai", "gpt-x-mini").one().is_some(), "found by its id alone");
}

#[test]
fn a_catalogue_reads_back_as_it_was_written_and_ignores_what_it_does_not_know() {
    let c = parse(&fixture("catalog-v1.json"), None).unwrap();
    assert_eq!(c.models.len(), 2);
    let mini = &c.models[0];
    assert_eq!(mini.parameters["temperature"].support, Some(Support::DefaultOnly));
    assert_eq!(mini.price.as_ref().unwrap().tiers[0].variants["batch"].input, Some(0.25));
    assert!(c.vendor("openai").is_some_and(|v| v.complete));
    let again = parse(serde_json::to_string(&c).unwrap().as_bytes(), Some(Format::Catalog)).unwrap();
    assert_eq!(again, c);
    match c.index().find("openai", "gpt-x-mini-2026-08-07") {
        Found::One(m) => assert_eq!(m.api_id, "gpt-x-mini"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn implausible_rates_are_named() {
    let mut r = pacod_agg_catalog::Rates { input: Some(2.0), output: Some(10.0), ..Default::default() };
    assert_eq!(r.implausible(), None);
    r.per_request = Some(250.0);
    assert_eq!(r.implausible(), Some("per_request"));
    r.per_request = None;
    r.input = Some(-1.0);
    assert_eq!(r.implausible(), Some("input"));
}
