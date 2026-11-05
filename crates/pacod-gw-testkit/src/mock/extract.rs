//! The mock's answer to the price tracker's extraction (a JSON schema named `price_page`): the
//! tables of the page in the prompt, read as a capable model would read them. A table's first
//! row names its columns; each row after it is one model. The columns it knows:
//!
//! | Column | Field |
//! | --- | --- |
//! | `Model` | the model's id (an id is a word without spaces; a name with spaces is not one) |
//! | `Input`, `Output`, `Cached input` | rates per million tokens, `$2.20 / 1M tokens` |
//! | `Context`, `Max output` | tokens, `128K`, `1M` or `16384` |
//! | `Vision` | `yes` or `no` |
//! | `Modalities` | input modalities, comma separated |
//!
//! A model several tables name gets the figures of each.

use serde_json::{Map, Value, json};

/// A figure as a page writes it: `$2.20 / 1M tokens` is 2.2, `128K` is 128000.
fn number(cell: &str) -> Option<f64> {
    let cell = cell.trim().trim_start_matches('$');
    let end = cell.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(cell.len());
    let value: f64 = cell[..end].replace(',', "").parse().ok()?;
    let unit = cell[end..].trim_start().chars().next().map(|c| c.to_ascii_uppercase());
    Some(match unit {
        Some('K') => value * 1_000.0,
        Some('M') => value * 1_000_000.0,
        _ => value,
    })
}

/// The answer for a page's text, in the extraction's shape: `{"models": [...]}`.
pub(crate) fn price_page_reply(prompt: &str) -> Value {
    let page = prompt.split_once("--- page ---").map_or(prompt, |(_, page)| page);
    let mut models: Vec<Map<String, Value>> = Vec::new();
    let mut columns: Option<Vec<String>> = None;
    for line in page.lines() {
        if !line.contains(" | ") {
            columns = None;
            continue;
        }
        let cells: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let Some(names) = &columns else {
            if cells[0].eq_ignore_ascii_case("model") {
                columns = Some(cells.iter().map(|c| c.to_ascii_lowercase()).collect());
            }
            continue;
        };
        let id = cells[0];
        let at = match models.iter().position(|m| m["model"] == id) {
            Some(i) => i,
            None => {
                let mut m = Map::new();
                m.insert("model".into(), json!(id));
                m.insert("api_id_stated".into(), json!(!id.contains(' ')));
                m.insert("currency".into(), json!("USD"));
                m.insert("evidence".into(), json!(line));
                models.push(m);
                models.len() - 1
            }
        };
        let m = &mut models[at];
        for (name, cell) in names.iter().zip(&cells).skip(1) {
            let field = match name.as_str() {
                "input" => "input",
                "output" => "output",
                "cached input" | "cache read" => "cache_read",
                "context" | "context window" => "context_window",
                "max output" | "output limit" => "max_output_tokens",
                "vision" => {
                    m.insert("vision".into(), json!(cell.eq_ignore_ascii_case("yes")));
                    continue;
                }
                "modalities" => {
                    let list: Vec<String> = cell.split(',').map(|s| s.trim().to_ascii_lowercase()).collect();
                    m.insert("input_modalities".into(), json!(list));
                    continue;
                }
                _ => continue,
            };
            if let Some(n) = number(cell) {
                let value =
                    if matches!(field, "context_window" | "max_output_tokens") { json!(n as u64) } else { json!(n) };
                m.insert(field.into(), value);
            }
        }
    }
    json!({ "models": models })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_of_a_page_become_its_models() {
        let prompt = "Find the prices.\n\n--- page ---\nPricing\nModel | Input | Output | Cached input\n\
                      tracked-steady | $2.20 / 1M tokens | $10.00 / 1M tokens | $0.22 / 1M tokens\n\
                      Tracked Name | $1.00 / 1M tokens | $2.00 / 1M tokens | –\n\
                      Footnote.\nModel | Context | Max output | Vision\ntracked-steady | 128K | 16384 | yes\n";
        let answer = price_page_reply(prompt);
        let models = answer["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        let steady = &models[0];
        assert_eq!((steady["input"].as_f64(), steady["cache_read"].as_f64()), (Some(2.2), Some(0.22)));
        assert_eq!(
            (steady["context_window"].as_u64(), steady["max_output_tokens"].as_u64()),
            (Some(128_000), Some(16_384))
        );
        assert_eq!((steady["vision"].as_bool(), steady["api_id_stated"].as_bool()), (Some(true), Some(true)));
        assert_eq!(models[1]["api_id_stated"], false, "a name with spaces is no id");
        assert_eq!((number("1M"), number("1M tokens"), number("$2 / 1M")), (Some(1e6), Some(1e6), Some(2.0)));
    }
}
