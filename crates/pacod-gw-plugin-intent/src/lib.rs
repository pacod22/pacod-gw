//! The intent plugin (`docs/plugin-concept.md`, section 5): what is this call for?
//!
//! It classifies a call against a catalogue of intents an organisation or workspace writes,
//! answers the catalogue's further questions (flags, scores, choices), and applies its rules:
//! allow, block or flag. Routing and limits read the intent it finds.
//!
//! Engines: `llm`, a chat model the organisation chooses, called through the gateway.

pub mod cache;
pub mod catalogue;
pub mod llm;
pub mod rules;
pub mod select;
pub mod settings;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use pacod_gw_plugin::{
    Bound, BoxFuture, CompileCtx, Egress, Engine, ErrorKind, Host, IntentSignal, Manifest, ModelUse, Plugin,
    PluginError, RequestInput, Signals, Verdict,
};
use serde_json::{Value, json};

pub use catalogue::Catalogue;
use rules::Answer;
use select::Selection;
use settings::EngineChoice;
pub use settings::Settings;

/// The plugin's id, as bindings name it.
pub const ID: &str = "intent";

/// The third party the Jev engine sends text to.
pub const TYPESAFE: &str = "typesafe";

static MANIFEST: Manifest = Manifest {
    id: ID,
    version: "1.0.0",
    title: "Intent",
    description: "Detects and classifies what a call is for against a catalogue of intents, and blocks, flags, \
                  routes and limits calls by it.",
    engines: &[
        Engine { id: "llm", egress: Egress::Provider },
        Engine { id: "jev", egress: Egress::ThirdParty(TYPESAFE) },
        Engine { id: "cascade", egress: Egress::ThirdParty(TYPESAFE) },
    ],
};

/// The intent plugin. One per process: its cache outlives configuration changes.
#[derive(Default)]
pub struct IntentPlugin {
    cache: Arc<cache::Cache>,
}

impl IntentPlugin {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The engine a binding's settings name, read leniently: the compiler reports bad settings.
fn engine_of(settings: &Value) -> EngineChoice {
    settings.get("engine").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

impl Plugin for IntentPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn engines(&self, settings: &Value) -> Vec<&'static str> {
        vec![engine_of(settings).name()]
    }

    fn default_timeout_ms(&self, settings: &Value) -> u32 {
        match engine_of(settings) {
            EngineChoice::Llm => 3_000,
            EngineChoice::Jev => 1_000,
            EngineChoice::Cascade => 4_000,
        }
    }

    fn compile(&self, ctx: &CompileCtx<'_>) -> Result<Arc<dyn Bound>, Vec<String>> {
        let settings: Settings = if ctx.settings.is_null() {
            Settings::default()
        } else {
            serde_json::from_value(ctx.settings.clone()).map_err(|e| vec![format!("settings: {e}")])?
        };
        let mut problems = settings.problems();
        let Some(found) = ctx.catalogue else {
            if !settings.catalogue.is_empty() {
                problems.push(format!("no intent catalogue \"{}\" is here", settings.catalogue));
            }
            return Err(problems);
        };
        let catalogue: Catalogue = match serde_json::from_value(found.doc.clone()) {
            Ok(c) => c,
            Err(e) => {
                problems.push(format!("the catalogue \"{}\" cannot be read: {e}", settings.catalogue));
                return Err(problems);
            }
        };
        problems.extend(catalogue.problems());
        match settings.engine {
            EngineChoice::Llm => {
                if let Err(e) = (ctx.check_model)(&settings.llm.model, ModelUse::StructuredChat) {
                    problems.push(format!("llm.model: {e}"));
                }
            }
            EngineChoice::Jev | EngineChoice::Cascade => {
                problems.push(format!("the engine \"{}\" is not available yet", settings.engine.name()));
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let mut h = DefaultHasher::new();
        ctx.settings.to_string().hash(&mut h);
        found.id.hash(&mut h);
        found.version.hash(&mut h);
        let llm = llm::prepare(&catalogue, &settings.llm);
        Ok(Arc::new(IntentBound {
            cache: self.cache.clone(),
            fingerprint: h.finish(),
            catalogue_id: found.id.to_owned(),
            catalogue_version: found.version,
            workspace_id: ctx.workspace_id.to_owned(),
            settings,
            catalogue,
            llm,
        }))
    }
}

/// The plugin as one workspace has it.
struct IntentBound {
    cache: Arc<cache::Cache>,
    /// The settings and the catalogue's version, hashed: a change asks the engines again.
    fingerprint: u64,
    catalogue_id: String,
    catalogue_version: i32,
    workspace_id: String,
    settings: Settings,
    catalogue: Catalogue,
    llm: llm::Prepared,
}

/// A marker the caller cannot predict, around the text an LLM classifies.
fn nonce() -> String {
    format!("{:016x}", fastrand::u64(..))
}

impl IntentBound {
    async fn llm(&self, sel: &Selection, host: &dyn Host) -> Result<Answer, PluginError> {
        let request = llm::request(&self.llm, &self.settings.llm.model, sel, &nonce());
        let answer = host.complete(request).await?;
        llm::parse(&self.catalogue, &self.settings.llm, &answer)
            .map_err(|e| PluginError::new(ErrorKind::InvalidAnswer, e))
    }

    async fn classify(&self, sel: &Selection, host: &dyn Host) -> Result<Answer, PluginError> {
        match self.settings.engine {
            EngineChoice::Llm => self.llm(sel, host).await,
            other => {
                Err(PluginError::new(ErrorKind::NotBuilt, format!("the engine \"{}\" is not available", other.name())))
            }
        }
    }

    fn verdict(&self, answer: Answer, sel: &Selection, cached: bool) -> Verdict {
        let decision = rules::decide(&self.catalogue, &answer);
        let act = if answer.confidence >= self.settings.act_min_confidence {
            answer.label.clone()
        } else {
            catalogue::UNKNOWN.to_owned()
        };
        let mut detail = json!({
            "catalogue": { "id": self.catalogue_id, "name": self.catalogue.name, "version": self.catalogue_version },
            "engine": answer.engine,
            "cached": cached,
            "truncated": sel.truncated,
        });
        if !answer.choices.is_empty() {
            detail["choices"] = json!(answer.choices);
        }
        if let (Some(d), Value::Object(extra)) = (detail.as_object_mut(), &answer.detail) {
            d.extend(extra.clone());
        }
        Verdict {
            signals: Signals {
                intent: Some(IntentSignal {
                    label: answer.label,
                    confidence: answer.confidence,
                    act,
                    engine: answer.engine,
                    cached,
                }),
                flags: answer.flags,
                scores: answer.scores,
            },
            action: decision.action,
            flag: decision.flag,
            alerts: decision.alerts,
            rule: decision.rule,
            trial: decision.trial,
            detail,
        }
    }
}

impl Bound for IntentBound {
    fn on_request<'a>(
        &'a self,
        input: &'a RequestInput,
        host: &'a dyn Host,
    ) -> BoxFuture<'a, Result<Verdict, PluginError>> {
        Box::pin(async move {
            let sel = select::select(&input.conversation, &self.settings);
            // Nothing to classify (an image alone): unknown, without asking an engine.
            if sel.is_empty() {
                let answer = Answer {
                    label: catalogue::UNKNOWN.into(),
                    confidence: 1.0,
                    flags: Default::default(),
                    scores: Default::default(),
                    choices: Default::default(),
                    engine: self.settings.engine.name().into(),
                    detail: json!({ "empty": true }),
                };
                return Ok(self.verdict(answer, &sel, false));
            }
            let ttl = Duration::from_secs(u64::from(self.settings.cache_seconds));
            let key = self.cache.key(&self.workspace_id, self.fingerprint, &sel.transcript());
            if !ttl.is_zero()
                && let Some(answer) = self.cache.get(key, ttl)
            {
                return Ok(self.verdict(answer, &sel, true));
            }
            let answer = self.classify(&sel, host).await?;
            if !ttl.is_zero() {
                self.cache.put(key, answer.clone());
            }
            Ok(self.verdict(answer, &sel, false))
        })
    }
}
