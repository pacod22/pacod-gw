//! Building an index generation from the registry's records.

use std::collections::HashMap;
use std::sync::Arc;

use jiff::Timestamp;
use pacod_broker_model::{HardwareItem, ProviderProfile, ResourceRecord, SoftwareItem, Source, Visibility};
use schemars::JsonSchema;
use serde::Serialize;

use crate::live::Live;
use crate::text::{Doc, TextIndex};

/// A provider as the index holds it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProviderEntry {
    pub id: String,
    /// The pacod-id subject, or `anon:<name>` in open mode.
    pub principal: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organisation_id: Option<String>,
    /// Set by the broker from pacod-id or by an admin, never by the provider.
    pub verified: bool,
    pub profile: ProviderProfile,
    pub source: Source,
    pub origin: String,
    pub registered_at: Timestamp,
    pub updated_at: Timestamp,
}

/// One resource with its provider and live cell.
#[derive(Debug)]
pub struct Entry {
    pub record: ResourceRecord,
    pub provider: Arc<ProviderEntry>,
    pub live: Arc<Live>,
}

impl Entry {
    /// The inventory items this resource runs on: those `runs_on` names, or the provider's whole
    /// inventory when it names none.
    pub fn hardware(&self) -> impl Iterator<Item = &HardwareItem> {
        let inv = self.provider.profile.inventory.as_ref();
        let named = self.record.spec.runs_on.as_ref().map(|r| &r.hardware).filter(|h| !h.is_empty());
        inv.into_iter()
            .flat_map(move |inv| inv.hardware.iter().filter(move |h| named.is_none_or(|n| n.contains(&h.id))))
    }

    pub fn software(&self) -> impl Iterator<Item = &SoftwareItem> {
        let inv = self.provider.profile.inventory.as_ref();
        let named = self.record.spec.runs_on.as_ref().map(|r| &r.software).filter(|s| !s.is_empty());
        inv.into_iter()
            .flat_map(move |inv| inv.software.iter().filter(move |s| named.is_none_or(|n| n.contains(&s.id))))
    }

    pub fn inventory_visibility(&self) -> Option<&Visibility> {
        self.provider.profile.inventory.as_ref().map(|i| &i.visibility)
    }
}

/// An immutable generation of the catalogue.
#[derive(Debug, Default)]
pub struct Index {
    pub generation: u64,
    pub built_at: Option<Timestamp>,
    pub(crate) entries: Vec<Arc<Entry>>,
    pub(crate) by_id: HashMap<String, u32>,
    pub(crate) providers: HashMap<String, Arc<ProviderEntry>>,
    pub(crate) by_kind: HashMap<String, Vec<u32>>,
    pub(crate) by_tag: HashMap<String, Vec<u32>>,
    pub(crate) by_protocol: HashMap<String, Vec<u32>>,
    pub(crate) by_capability: HashMap<String, Vec<u32>>,
    pub(crate) by_region: HashMap<String, Vec<u32>>,
    pub(crate) by_provider: HashMap<String, Vec<u32>>,
    pub(crate) by_gpu: HashMap<String, Vec<u32>>,
    pub(crate) by_software: HashMap<String, Vec<u32>>,
    pub(crate) text: TextIndex,
    /// Each entry's position in name order, so sorting by name compares numbers.
    pub(crate) name_rank: Vec<u32>,
}

fn push(map: &mut HashMap<String, Vec<u32>>, key: impl Into<String>, i: u32) {
    let list = map.entry(key.into()).or_default();
    // Entries are added in order, so a repeated key for the same entry is always the last one.
    if list.last() != Some(&i) {
        list.push(i);
    }
}

/// The capability names discovery filters on: the flags, `mcp_tool:<name>` and `extra:<key>`.
pub(crate) fn capability_names(r: &ResourceRecord) -> Vec<String> {
    let c = &r.spec.capabilities;
    let mut out: Vec<String> = [
        ("tools", c.tools),
        ("vision", c.vision),
        ("reasoning", c.reasoning),
        ("structured_output", c.structured_output),
        ("streaming", c.streaming),
        ("prompt_cache", c.prompt_cache),
        ("embeddings", c.embeddings),
    ]
    .into_iter()
    .filter(|(_, on)| *on)
    .map(|(n, _)| n.to_string())
    .collect();
    out.extend(c.mcp_tools.iter().map(|t| format!("mcp_tool:{}", t.name)));
    out.extend(
        c.extra
            .iter()
            .filter(|(_, v)| !matches!(v, serde_json::Value::Bool(false) | serde_json::Value::Null))
            .map(|(k, _)| format!("extra:{k}")),
    );
    out
}

impl Index {
    /// Builds a generation. Entries are sorted by id, so results that tie come back in the same
    /// order in every generation.
    pub fn build(
        generation: u64,
        providers: impl IntoIterator<Item = Arc<ProviderEntry>>,
        mut entries: Vec<Arc<Entry>>,
    ) -> Self {
        entries.sort_by(|a, b| a.record.id.cmp(&b.record.id));
        let mut idx = Index {
            generation,
            built_at: Some(Timestamp::now()),
            providers: providers.into_iter().map(|p| (p.id.clone(), p)).collect(),
            ..Index::default()
        };
        let mut docs = Vec::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            let i = i as u32;
            let r = &e.record;
            let s = &r.spec;
            idx.by_id.insert(r.id.clone(), i);
            push(&mut idx.by_kind, s.kind.clone(), i);
            for t in &s.tags {
                push(&mut idx.by_tag, t.clone(), i);
            }
            for f in &s.interfaces {
                push(&mut idx.by_protocol, f.protocol.clone(), i);
            }
            for c in capability_names(r) {
                push(&mut idx.by_capability, c, i);
            }
            for g in &s.availability.regions {
                push(&mut idx.by_region, g.clone(), i);
            }
            push(&mut idx.by_provider, r.provider_id.clone(), i);
            for h in e.hardware().filter(|h| h.kind == pacod_broker_model::HardwareType::Gpu) {
                if let Some(m) = h.model_key() {
                    push(&mut idx.by_gpu, m, i);
                }
            }
            for sw in e.software() {
                push(&mut idx.by_software, sw.name.clone(), i);
            }
            let mut d = Doc::default();
            d.add(&s.name, 3);
            d.add(s.title(), 3);
            for t in &s.tags {
                d.add(t, 3);
            }
            for t in s.text.searchable() {
                d.add(t, 1);
            }
            if let Some(f) = s.model.as_ref().and_then(|m| m.family.as_deref()) {
                d.add(f, 2);
            }
            for t in &s.capabilities.mcp_tools {
                d.add(&t.name, 2);
                d.add(&t.description, 1);
            }
            d.add(&e.provider.profile.name, 2);
            docs.push(d);
        }
        // Lists built per entry in order are sorted already; postings by provider and GPU may
        // have been pushed out of order only if entries were, and they were sorted above.
        idx.text = TextIndex::build(docs);
        let mut order: Vec<u32> = (0..entries.len() as u32).collect();
        order.sort_by(|a, b| entries[*a as usize].record.spec.name.cmp(&entries[*b as usize].record.spec.name));
        idx.name_rank = vec![0; entries.len()];
        for (rank, i) in order.into_iter().enumerate() {
            idx.name_rank[i as usize] = rank as u32;
        }
        idx.entries = entries;
        idx
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Arc<Entry>> {
        self.by_id.get(id).map(|i| &self.entries[*i as usize])
    }

    pub fn provider(&self, id: &str) -> Option<&Arc<ProviderEntry>> {
        self.providers.get(id)
    }

    pub fn providers(&self) -> impl Iterator<Item = &Arc<ProviderEntry>> {
        self.providers.values()
    }

    pub fn entries(&self) -> impl Iterator<Item = &Arc<Entry>> {
        self.entries.iter()
    }

    pub fn of_provider(&self, id: &str) -> impl Iterator<Item = &Arc<Entry>> {
        self.by_provider.get(id).into_iter().flatten().map(|i| &self.entries[*i as usize])
    }

    /// Resources by kind, for the overview.
    pub fn kinds(&self) -> impl Iterator<Item = (&str, usize)> {
        self.by_kind.iter().map(|(k, v)| (k.as_str(), v.len()))
    }
}
