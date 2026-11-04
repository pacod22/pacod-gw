//! Building entries for tests and benchmarks.

use std::sync::Arc;

use jiff::Timestamp;
use pacod_broker_model::{LiveStatus, ProviderProfile, ResourceRecord, ResourceSpec, Source, provider_id, resource_id};

use crate::build::{Entry, ProviderEntry};
use crate::live::Live;

pub fn provider(name: &str, profile: serde_json::Value) -> Arc<ProviderEntry> {
    let mut p: ProviderProfile = serde_json::from_value(profile).expect("profile");
    p.name = name.into();
    let now = Timestamp::now();
    Arc::new(ProviderEntry {
        id: provider_id("test", &format!("anon:{name}")),
        principal: format!("anon:{name}"),
        organisation_id: None,
        verified: false,
        profile: p,
        source: Source::Static,
        origin: "test".into(),
        registered_at: now,
        updated_at: now,
    })
}

pub fn entry(p: &Arc<ProviderEntry>, spec: serde_json::Value) -> Arc<Entry> {
    let spec: ResourceSpec = serde_json::from_value(spec).expect("spec");
    let now = Timestamp::now();
    Arc::new(Entry {
        record: ResourceRecord {
            id: resource_id(&p.id, &spec.key),
            provider_id: p.id.clone(),
            spec,
            origin: "test".into(),
            source: Source::Static,
            lease_id: None,
            registered_at: now,
            updated_at: now,
        },
        provider: p.clone(),
        live: Arc::new(Live::permanent(LiveStatus::Active)),
    })
}
