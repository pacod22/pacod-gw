//! Finding a gateway's model in a catalogue: by the vendor and the id the gateway calls it by.
//! Sources write ids their own way (`claude-haiku-5.5` for `claude-haiku-5-5`, a dated snapshot
//! for its model), so the match goes from exact to looser, and takes a looser match only when
//! it is the only one.

use std::collections::HashMap;

use crate::ids::{model_slug, normalise_model_id, undated};
use crate::v1::{Catalog, Model};

/// The models of a catalogue by vendor, for repeated lookups.
pub struct Index<'a> {
    by_vendor: HashMap<&'a str, Vec<&'a Model>>,
    /// Models a file names without a vendor (a price feed's bare ids).
    unvendored: Vec<&'a Model>,
}

/// What a lookup found.
#[derive(Debug, PartialEq)]
pub enum Found<'a> {
    One(&'a Model),
    None,
    /// Several models match equally well; their ids.
    Ambiguous(Vec<String>),
}

impl<'a> Found<'a> {
    pub fn one(self) -> Option<&'a Model> {
        match self {
            Self::One(m) => Some(m),
            _ => None,
        }
    }
}

impl Catalog {
    pub fn index(&self) -> Index<'_> {
        let mut by_vendor: HashMap<&str, Vec<&Model>> = HashMap::new();
        let mut unvendored = Vec::new();
        for m in &self.models {
            match m.vendor.as_deref() {
                Some(v) => by_vendor.entry(v).or_default().push(m),
                None => unvendored.push(m),
            }
        }
        Index { by_vendor, unvendored }
    }
}

impl<'a> Index<'a> {
    /// The vendors the catalogue has models of.
    pub fn vendors(&self) -> impl Iterator<Item = &&'a str> {
        self.by_vendor.keys()
    }

    /// The models of a vendor.
    pub fn models_of(&self, vendor: &str) -> &[&'a Model] {
        self.by_vendor.get(vendor).map(Vec::as_slice).unwrap_or_default()
    }

    /// The vendor's model the gateway calls `upstream_id`. Models without a vendor are searched
    /// after the vendor's, by the id alone.
    pub fn find(&self, vendor: &str, upstream_id: &str) -> Found<'a> {
        match find_in(self.models_of(vendor), upstream_id) {
            Found::None => find_in(&self.unvendored, upstream_id),
            found => found,
        }
    }
}

fn find_in<'a>(models: &[&'a Model], upstream_id: &str) -> Found<'a> {
    let wanted = normalise_model_id(upstream_id);
    let ids = |m: &Model| -> Vec<String> {
        std::iter::once(&m.api_id).chain(&m.aliases).map(|id| normalise_model_id(id)).collect()
    };
    let exact: Vec<&Model> = models.iter().copied().filter(|m| ids(m).contains(&wanted)).collect();
    let slug = model_slug(&wanted);
    let by_slug: Vec<&Model> =
        models.iter().copied().filter(|m| ids(m).iter().any(|id| model_slug(id) == slug)).collect();
    let bare = undated(&slug).to_owned();
    let by_undated: Vec<&Model> =
        models.iter().copied().filter(|m| ids(m).iter().any(|id| undated(&model_slug(id)) == bare)).collect();
    for candidates in [exact, by_slug, by_undated] {
        match candidates.as_slice() {
            [] => continue,
            [one] => return Found::One(one),
            many => return Found::Ambiguous(many.iter().map(|m| m.api_id.clone()).collect()),
        }
    }
    Found::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::Model;

    fn model(vendor: &str, api_id: &str, aliases: &[&str]) -> Model {
        Model {
            vendor: Some(vendor.into()),
            api_id: api_id.into(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_model_is_found_exactly_then_by_slug_then_without_its_date() {
        let catalog = Catalog {
            models: vec![
                model("anthropic", "claude-haiku-5.5", &["claude-haiku-5.5-20261007"]),
                model("openai", "gpt-5-mini", &[]),
                model("openai", "gpt-4o", &[]),
                model("openai", "gpt-4o-mini", &[]),
                model("google", "gemini-2.5-pro", &[]),
                model("xai", "grok-4", &[]),
                model("xai", "grok-4-0709", &[]),
            ],
            ..Default::default()
        };
        let index = catalog.index();
        let found = |v: &str, id: &str| index.find(v, id).one().map(|m| m.api_id.clone());
        assert_eq!(found("anthropic", "claude-haiku-5-5").as_deref(), Some("claude-haiku-5.5"));
        assert_eq!(found("anthropic", "claude-haiku-5-5-20261007").as_deref(), Some("claude-haiku-5.5"));
        assert_eq!(found("openai", "gpt-4o-2024-08-06").as_deref(), Some("gpt-4o"));
        assert_eq!(found("openai", "GPT-5-mini").as_deref(), Some("gpt-5-mini"));
        assert_eq!(found("google", "models/gemini-2.5-pro").as_deref(), Some("gemini-2.5-pro"));
        // Another vendor's model of the same name is not this vendor's.
        assert_eq!(index.find("anthropic", "gpt-5-mini"), Found::None);
        // The exact id wins over a looser match.
        assert_eq!(found("xai", "grok-4").as_deref(), Some("grok-4"));
        assert_eq!(found("xai", "grok-4-0709").as_deref(), Some("grok-4-0709"));
    }

    #[test]
    fn two_equally_good_matches_are_none() {
        let catalog = Catalog {
            models: vec![model("openai", "gpt-x-20250101", &[]), model("openai", "gpt-x-20250601", &[])],
            ..Default::default()
        };
        assert!(matches!(catalog.index().find("openai", "gpt-x"), Found::Ambiguous(ids) if ids.len() == 2));
    }

    #[test]
    fn a_model_without_a_vendor_is_found_by_its_id() {
        let catalog = Catalog {
            models: vec![Model { api_id: "deepseek-chat".into(), ..Default::default() }],
            ..Default::default()
        };
        assert!(catalog.index().find("deepseek", "deepseek-chat").one().is_some());
    }
}
