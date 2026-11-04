//! Human-readable descriptions, for people choosing a resource or a provider.
//!
//! Every resource and provider has a one-line plain-text `summary` (shown in result lists) and
//! may have a longer Markdown `description` with usage examples. The broker stores Markdown as
//! given and never renders it; the console renders it sanitised. The text search covers title,
//! summary and description.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits;
use crate::validate::{self, Problems};

/// The human-readable text of a resource or provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Text {
    /// A display name, plain text. Defaults to the resource's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// One line of plain text, at most 200 characters.
    #[serde(default)]
    pub summary: String,
    /// Markdown, at most `BROKER_MAX_DESCRIPTION_BYTES` (16 KiB by default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    /// Usage snippets: how to call the resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<Example>,
    /// Translations, by BCP 47 language tag (`bg`, `de`, `pt-BR`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub i18n: BTreeMap<String, Translation>,
}

/// A translation of the summary and description.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Translation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One usage snippet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Example {
    pub title: String,
    /// The language of the snippet, for highlighting: `bash`, `python`, `json`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub code: String,
}

impl Text {
    pub fn validate(&self, p: &mut Problems, path: &str, max_description: usize) {
        if let Some(t) = &self.title {
            validate::text_len(p, &format!("{path}.title"), t, limits::TITLE_CHARS);
        }
        validate::text_len(p, &format!("{path}.summary"), &self.summary, limits::SUMMARY_CHARS);
        p.check(!self.summary.contains('\n'), format!("{path}.summary"), "one line");
        check_description(p, &format!("{path}.description"), &self.description, max_description);
        if let Some(u) = &self.documentation_url {
            validate::url(p, &format!("{path}.documentation_url"), u);
        }
        p.check(self.examples.len() <= limits::EXAMPLES, format!("{path}.examples"), "too many examples");
        for (i, e) in self.examples.iter().enumerate() {
            validate::text_len(p, &format!("{path}.examples[{i}].title"), &e.title, limits::TITLE_CHARS);
            p.check(e.code.len() <= limits::EXAMPLE_BYTES, format!("{path}.examples[{i}].code"), "too long");
        }
        p.check(self.i18n.len() <= limits::LANGUAGES, format!("{path}.i18n"), "too many languages");
        for (lang, t) in &self.i18n {
            let at = format!("{path}.i18n.{lang}");
            p.check(is_language_tag(lang), &at, "a BCP 47 language tag");
            if let Some(title) = &t.title {
                validate::text_len(p, &format!("{at}.title"), title, limits::TITLE_CHARS);
            }
            validate::text_len(p, &format!("{at}.summary"), &t.summary, limits::SUMMARY_CHARS);
            check_description(p, &format!("{at}.description"), &t.description, max_description);
        }
    }

    /// The bytes this text holds, for the broker's memory accounting.
    pub fn weight(&self) -> usize {
        self.summary.len()
            + self.description.len()
            + self.title.as_ref().map_or(0, String::len)
            + self.examples.iter().map(|e| e.code.len() + e.title.len()).sum::<usize>()
            + self.i18n.values().map(|t| t.summary.len() + t.description.len()).sum::<usize>()
    }

    /// The words the text search indexes: title, summary and description in every language.
    pub fn searchable(&self) -> impl Iterator<Item = &str> {
        self.title
            .iter()
            .map(String::as_str)
            .chain([self.summary.as_str(), self.description.as_str()])
            .chain(self.i18n.values().flat_map(|t| [t.summary.as_str(), t.description.as_str()]))
    }
}

fn check_description(p: &mut Problems, path: &str, value: &str, max: usize) {
    if value.len() > max {
        p.add(path, format_args!("at most {max} bytes"));
    }
    if value.contains('\0') {
        p.add(path, "NUL is not allowed");
    }
}

fn is_language_tag(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.split('-')
            .all(|part| !part.is_empty() && part.len() <= 8 && part.bytes().all(|b| b.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_enforced() {
        let t = Text {
            summary: "x".repeat(201),
            description: "y".repeat(20),
            i18n: [("not a tag".to_string(), Translation::default())].into(),
            ..Text::default()
        };
        let mut p = Problems::new();
        t.validate(&mut p, "resource", 10);
        let all = p.to_string();
        assert!(all.contains("resource.summary: at most 200"), "{all}");
        assert!(all.contains("resource.description: at most 10 bytes"), "{all}");
        assert!(all.contains("resource.i18n.not a tag: a BCP 47"), "{all}");
    }
}
