//! The contract between pacod-gw-core (producer) and pacod-gw-analytics (consumer).
//! The UI mirrors this in `ui/src/lib/types.ts`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One record per proxied LLM request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub tenant_id: String,
    pub api_key_id: String,
    pub provider: String,
    pub model: String,
    pub route: String,
    pub status_code: u16,
    pub latency_ms: u64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
    pub cached: bool,
}
