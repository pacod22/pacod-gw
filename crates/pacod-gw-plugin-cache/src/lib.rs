//! The response cache plugin (`docs/plugin-concept.md`, section 15): a call that was answered
//! before is answered again from the kept answer, without the provider.
//!
//! The plugin is policy only: which calls may be answered again, what makes two calls the same
//! (the key), how long an answer is kept, and which answers are worth keeping. The core reads
//! and writes the store (Valkey, or memory) and writes the kept answer in the caller's dialect,
//! as a stream or whole. Answers are kept per workspace and never shared across workspaces.

pub mod key;
pub mod settings;

use std::sync::Arc;
use std::time::Duration;

use pacod_gw_plugin::canonical::{Block, Finish, Response};
use pacod_gw_plugin::{Bound, CacheDirective, CompileCtx, Egress, Engine, Hook, Keep, Manifest, PlannedCall, Plugin};
use serde_json::Value;

use settings::Calls;
pub use settings::Settings;

/// The plugin's id, as bindings name it.
pub const ID: &str = "response_cache";

static MANIFEST: Manifest = Manifest {
    id: ID,
    version: "1.0.0",
    title: "Response cache",
    description: "Answers a call that was answered before with the kept answer, without the provider. Calls with \
                  temperature 0 by default; answers are kept per workspace.",
    engines: &[Engine { id: "store", egress: Egress::None }],
    hooks: &[Hook::Answers],
    installed_by_default: true,
};

/// The response cache plugin.
#[derive(Default)]
pub struct CachePlugin;

impl CachePlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Plugin for CachePlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    /// The time a call waits for the store before it goes on without it.
    fn default_timeout_ms(&self, _settings: &Value) -> u32 {
        100
    }

    fn compile(&self, ctx: &CompileCtx<'_>) -> Result<Arc<dyn Bound>, Vec<String>> {
        let settings: Settings = if ctx.settings.is_null() {
            Settings::default()
        } else {
            serde_json::from_value(ctx.settings.clone()).map_err(|e| vec![format!("settings: {e}")])?
        };
        let problems = settings.problems();
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(Arc::new(CacheBound { settings }))
    }

    fn default_settings(&self) -> Option<Value> {
        serde_json::to_value(Settings::default()).ok()
    }
}

/// The plugin as one workspace has it.
pub struct CacheBound {
    settings: Settings,
}

impl Bound for CacheBound {
    fn answers(&self, call: &PlannedCall<'_>) -> Option<Keep> {
        if call.directive == CacheDirective::Off {
            return None;
        }
        let (key, traits) = key::read(call);
        if traits.unanswerable || (self.settings.calls == Calls::Deterministic && !traits.deterministic) {
            return None;
        }
        Some(Keep {
            key,
            ttl: Duration::from_secs(u64::from(self.settings.ttl_seconds)),
            lookup: call.directive != CacheDirective::Refresh,
            store: true,
            max_bytes: self.settings.max_answer_bytes as usize,
        })
    }

    /// An answer that ended normally, with nothing in it that belongs to the moment it was given
    /// (a provider's own tools, such as a web search) or that a stream could not carry.
    fn keeps(&self, answer: &Response) -> bool {
        matches!(answer.finish, Finish::Stop | Finish::Length | Finish::ToolCalls)
            && answer.usage.is_none_or(|u| u.server_tool_calls == 0)
            && answer.content.iter().all(|b| match b {
                Block::Text { .. } | Block::ToolCall { .. } => true,
                Block::Reasoning { redacted, .. } => redacted.is_none(),
                Block::Provider { .. } | Block::Image { .. } | Block::Document { .. } | Block::ToolResult { .. } => {
                    false
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use pacod_gw_plugin::Dialect;
    use pacod_gw_plugin::canonical::Usage;
    use serde_json::{Map, json};

    use super::*;

    fn bound(settings: Value) -> Arc<dyn Bound> {
        let check = |_: &str, _: pacod_gw_plugin::ModelUse| Ok(());
        let ctx = CompileCtx {
            settings: &settings,
            catalogue: None,
            organisation_id: "org_1",
            workspace_id: "ws_1",
            check_model: &check,
        };
        CachePlugin.compile(&ctx).unwrap_or_else(|p| panic!("{p:?}"))
    }

    fn call(body: &[u8], directive: CacheDirective) -> PlannedCall<'_> {
        PlannedCall {
            workspace_id: "ws_1",
            model_id: "mdl_1",
            upstream_id: "gpt-x",
            provider_id: "prv_1",
            template: None,
            dialect: Dialect::OpenAiChat,
            body,
            headers: &[],
            directive,
            asked_temperature: None,
        }
    }

    #[test]
    fn deterministic_calls_by_default_and_all_when_asked() {
        let cold = br#"{"temperature":0}"#;
        let warm = br#"{"temperature":0.8}"#;
        let b = bound(Value::Null);
        assert!(b.answers(&call(cold, CacheDirective::Default)).is_some_and(|k| k.lookup && k.store));
        assert!(b.answers(&call(warm, CacheDirective::Default)).is_none());
        let all = bound(json!({ "calls": "all", "ttl_seconds": 60 }));
        let k = all.answers(&call(warm, CacheDirective::Default)).unwrap();
        assert_eq!(k.ttl, Duration::from_secs(60));
    }

    #[test]
    fn the_caller_can_bypass_or_refresh() {
        let b = bound(Value::Null);
        let body = br#"{"temperature":0}"#;
        assert!(b.answers(&call(body, CacheDirective::Off)).is_none());
        let refresh = b.answers(&call(body, CacheDirective::Refresh)).unwrap();
        assert!(!refresh.lookup && refresh.store);
    }

    #[test]
    fn settings_are_checked() {
        let check = |_: &str, _: pacod_gw_plugin::ModelUse| Ok(());
        for bad in [json!({ "ttl_seconds": 0 }), json!({ "max_answer_bytes": 10 }), json!({ "colour": "red" })] {
            let ctx = CompileCtx {
                settings: &bad,
                catalogue: None,
                organisation_id: "o",
                workspace_id: "w",
                check_model: &check,
            };
            assert!(CachePlugin.compile(&ctx).is_err(), "{bad}");
        }
        assert_eq!(
            CachePlugin.default_settings(),
            Some(json!({ "calls": "deterministic", "ttl_seconds": 3600, "max_answer_bytes": 1_048_576 }))
        );
    }

    #[test]
    fn only_answers_that_ended_normally_are_kept() {
        let b = bound(Value::Null);
        let answer = |finish, content, server_tool_calls| Response {
            id: "a".into(),
            model: "m".into(),
            content,
            finish,
            native_finish: String::new(),
            usage: Some(Usage { server_tool_calls, ..Default::default() }),
            extra: Map::new(),
        };
        assert!(b.keeps(&answer(Finish::Stop, vec![Block::text("hi")], 0)));
        assert!(!b.keeps(&answer(Finish::ContentFilter, vec![Block::text("no")], 0)));
        assert!(!b.keeps(&answer(Finish::Paused, vec![], 0)));
        assert!(!b.keeps(&answer(Finish::Stop, vec![Block::text("found it")], 1)), "a web search");
        let provider = Block::Provider { family: "anthropic".into(), raw: json!({ "type": "web_search_tool_result" }) };
        assert!(!b.keeps(&answer(Finish::Stop, vec![provider], 0)));
    }
}
