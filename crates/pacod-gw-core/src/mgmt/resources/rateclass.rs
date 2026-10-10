//! Rate classes: defined by platform admins, named by API keys.

use axum::Router;

use crate::mgmt::AdminState;
use crate::mgmt::crud::{BuildCtx, Ops, Resource, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{ApiKey, Doc, Obj, RateClass, is_slug};
use crate::store::Tx;

impl Resource for RateClass {
    const PLURAL: &'static str = "rate-classes";
    const LABEL: &'static str = "rate class";
    const SCOPE: ScopeKind = ScopeKind::Global;
    const STRICT: bool = true;
    type Input = RateClass;
    type View = RateClass;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, _ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !is_slug(&input.name) {
            return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens"));
        }
        if existing.is_some_and(|e| e.doc.name != input.name) {
            return Err(ApiError::invalid("a rate class keeps its name: keys name it"));
        }
        if input.requests_per_minute == Some(0)
            || input.tokens_per_minute == Some(0)
            || input.max_concurrency == Some(0)
        {
            return Err(ApiError::invalid("a rate of 0 would stop every key in the class; revoke the keys instead"));
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let keys = tx.list(ApiKey::KIND, None).await?;
        let users: Vec<String> = keys
            .into_iter()
            .filter(|r| r.doc.get("rate_class").and_then(|v| v.as_str()) == Some(obj.doc.name.as_str()))
            .filter_map(|r| r.name)
            .collect();
        if !users.is_empty() {
            return Err(ApiError::conflict(format!("keys name this rate class: {}", users.join(", ")))
                .with_details(serde_json::json!({ "keys": users })));
        }
        Ok(())
    }
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    register::<RateClass>(router, api, Ops::ALL)
}
