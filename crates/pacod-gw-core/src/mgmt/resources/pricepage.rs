//! The price tracker's pages: what the daily check reads for each provider, and what the
//! weekly scan proposes. An administrator adds pages, confirms or dismisses the scan's, and
//! asks for a page to be checked now.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use pacod_gw_events::jobs::JOB_PRICE_CHECK;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Item, Ops, Resource, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{Doc, FeedFormat, FoundBy, Obj, PageKind, PageStatus, PricePage};
use crate::store::ParentFilter;
use crate::store::pricepages::PageState;

/// A page as an administrator adds or changes it. Its status changes through confirm and
/// dismiss; what the scan saw about it stays as the scan saw it.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PricePageInput {
    /// The provider's object id.
    pub provider_id: String,
    /// The page's address. It is stored normalised: no fragment, no tracking parameters, no
    /// slash at the end.
    pub url: String,
    #[serde(default)]
    pub kind: PageKind,
    /// For feeds: the shape of the list.
    #[serde(default)]
    pub format: FeedFormat,
    /// For feeds: the prefix the list files this provider's models under, where it is not the
    /// gateway's name for the provider.
    #[serde(default)]
    pub key_prefix: Option<String>,
    /// Where two pages of a provider say different things about a model, the lower number
    /// wins.
    #[serde(default)]
    pub priority: u32,
    #[serde(default)]
    pub notes: String,
}

impl Resource for PricePage {
    const PLURAL: &'static str = "price-pages";
    const LABEL: &'static str = "price page";
    const SCOPE: ScopeKind = ScopeKind::Global;
    const STRICT: bool = true;
    type Input = PricePageInput;
    type View = PricePage;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(PricePage::unique_name(&doc.provider_id, &doc.url))
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !ctx.snap.providers.contains_key(&input.provider_id) {
            return Err(ApiError::invalid("provider_id names no provider"));
        }
        let url = pacod_gw_events::pages::normalise_url(&input.url).map_err(ApiError::invalid)?;
        // The scraper fetches this address from inside the network: the same rule as for a
        // provider's base URL, applied again when it connects.
        ctx.st.egress.check_url(&url).map_err(|e| ApiError::invalid(format!("url: {e}")))?;
        let key_prefix = input.key_prefix.map(|k| k.trim().to_owned()).filter(|k| !k.is_empty());
        if input.kind == PageKind::Page && key_prefix.is_some() {
            return Err(ApiError::invalid("key_prefix is for feeds; a page is read by a model"));
        }
        if input.priority > 1000 || input.notes.chars().count() > 2000 {
            return Err(ApiError::invalid("priority is at most 1000, notes at most 2000 characters"));
        }
        let base = match existing {
            Some(e) => e.doc.clone(),
            // A page an administrator adds is read from the next check on.
            None => PricePage {
                provider_id: String::new(),
                url: String::new(),
                kind: PageKind::Page,
                format: FeedFormat::default(),
                key_prefix: None,
                status: PageStatus::Active,
                found_by: FoundBy::Admin,
                priority: 0,
                notes: String::new(),
                title: String::new(),
                score: None,
                moved_from: None,
                decided_by: Some(ctx.actor.to_owned()),
                decided_at: Some(Utc::now()),
            },
        };
        Ok(PricePage {
            provider_id: input.provider_id,
            url,
            kind: input.kind,
            format: input.format,
            key_prefix,
            priority: input.priority,
            notes: input.notes,
            ..base
        })
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    /// A new page is checked at the next run, not only at the next scheduled one.
    async fn after_commit(st: &AdminState, obj: &Obj<Self>, existing: Option<&Obj<Self>>) {
        let moved = existing.is_none_or(|e| e.doc.url != obj.doc.url);
        if obj.doc.status == PageStatus::Active && moved {
            request_check(st, obj).await;
        }
    }
}

/// Asks for one page to be checked and read at the next run.
async fn request_check(st: &AdminState, page: &Obj<PricePage>) {
    let ask = async {
        st.store.price_pages_request_check(&[(page.id.clone(), page.doc.provider_id.clone())], true).await?;
        st.store.job_request_run_for(JOB_PRICE_CHECK, &[]).await
    };
    if let Err(e) = ask.await {
        tracing::warn!(error = %e, page = %page.doc.url, "asking for a check of a price page failed");
    }
}

/// A page with its provider's name and what the tracker knows about it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PricePageView {
    #[serde(flatten)]
    pub page: PricePage,
    /// The provider's name.
    pub provider: String,
    /// What the last check found. Absent before the page was first checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<PageState>,
}

#[derive(Debug, Default, Deserialize)]
struct PagesQuery {
    provider_id: Option<String>,
    status: Option<PageStatus>,
}

/// Every page, by provider and priority, with its state.
async fn list(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<PagesQuery>,
) -> ApiResult<Json<Vec<Item<PricePageView>>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let mut states = st.store.price_page_states().await?;
    let mut out = Vec::new();
    for row in st.store.list(PricePage::KIND, ParentFilter::Any, None, 5000).await? {
        let page: Obj<PricePage> = row.into_obj()?;
        if q.provider_id.as_ref().is_some_and(|id| *id != page.doc.provider_id)
            || q.status.is_some_and(|s| s != page.doc.status)
        {
            continue;
        }
        let provider = snap.providers.get(&page.doc.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default();
        out.push(Item {
            meta: page.meta(),
            data: PricePageView { state: states.remove(&page.id), provider, page: page.doc },
        });
    }
    // Ids sort by creation time, so pages of one priority keep the order they were added in.
    out.sort_by(|a, b| {
        (&a.data.provider, a.data.page.priority, &a.meta.id).cmp(&(&b.data.provider, b.data.page.priority, &b.meta.id))
    });
    Ok(Json(out))
}

/// Confirms or dismisses a page. A confirmed page is read at the next run; a dismissed one is
/// not read, and the scan does not propose it again.
async fn decide(st: &AdminState, p: &Principal, id: &str, to: PageStatus) -> ApiResult<Json<Item<PricePage>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let mut page = st.store.get_obj::<PricePage>(id).await.map_err(|_| ApiError::not_found("price page"))?;
    if page.doc.status == to {
        return Err(ApiError::conflict(match to {
            PageStatus::Active => "the page is already confirmed",
            _ => "the page is already dismissed",
        }));
    }
    page.doc.status = to;
    page.doc.decided_by = Some(p.id.clone());
    page.doc.decided_at = Some(Utc::now());
    let mut entry = p.audit(None, None);
    entry.action = match to {
        PageStatus::Active => "price_page_confirmed",
        _ => "price_page_dismissed",
    }
    .into();
    let doc = serde_json::to_value(&page.doc).map_err(ApiError::internal)?;
    let mut tx = st.store.begin().await?;
    let row = tx.update(&page.id, Some(page.version), page.name.as_deref(), &doc, entry).await?;
    st.commit(tx).await?;
    let page: Obj<PricePage> = row.into_obj()?;
    if to == PageStatus::Active {
        request_check(st, &page).await;
    }
    st.announce("price_page", json!({ "id": page.id, "status": page.doc.status }));
    Ok(Json(Item { meta: page.meta(), data: page.doc }))
}

async fn confirm(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<Json<Item<PricePage>>> {
    decide(&st, &p, &id, PageStatus::Active).await
}

async fn dismiss(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<Json<Item<PricePage>>> {
    decide(&st, &p, &id, PageStatus::Dismissed).await
}

/// Asks for a page to be checked, and read even when it did not change, at the next run.
async fn check(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<StatusCode> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let page = st.store.get_obj::<PricePage>(&id).await.map_err(|_| ApiError::not_found("price page"))?;
    if page.doc.status != PageStatus::Active {
        return Err(ApiError::conflict("only a confirmed page is checked; confirm it first"));
    }
    st.store.price_pages_request_check(&[(page.id.clone(), page.doc.provider_id.clone())], true).await?;
    st.store.job_request_run_for(JOB_PRICE_CHECK, &[]).await?;
    let mut entry = p.audit(None, None);
    entry.action = "price_page_check".into();
    entry.kind = PricePage::KIND.into();
    entry.object_id = page.id;
    st.store.audit(&entry).await?;
    Ok(StatusCode::ACCEPTED)
}

/// Reads the sources of an earlier release's price job into price pages, once, and marks the
/// job as migrated. The sources stay in the job for an instance of an earlier release. A
/// source's place in the list becomes its page's priority, so that the first source of a
/// provider still comes first; a page already at the address, or a source whose provider is
/// gone, is left out.
pub async fn migrate_sources(
    tx: &mut crate::store::Tx,
    job: &mut crate::model::PriceJob,
    audit: &crate::store::AuditEntry,
) -> anyhow::Result<usize> {
    use crate::model::{Provider, SourceKind, new_id};
    if job.pages_migrated {
        return Ok(0);
    }
    let known: std::collections::HashSet<String> =
        tx.list(PricePage::KIND, None).await?.into_iter().filter_map(|r| r.name).collect();
    let mut made = 0;
    for (i, s) in job.sources.iter().enumerate() {
        let Ok(url) = pacod_gw_events::pages::normalise_url(&s.url) else { continue };
        let name = PricePage::unique_name(&s.provider_id, &url);
        if known.contains(&name) || tx.get_kind(Provider::KIND, &s.provider_id).await?.is_none() {
            continue;
        }
        let page = PricePage {
            provider_id: s.provider_id.clone(),
            url,
            kind: match s.kind {
                SourceKind::Feed => PageKind::Feed,
                SourceKind::Page => PageKind::Page,
            },
            format: s.format,
            key_prefix: s.key_prefix.clone(),
            status: PageStatus::Active,
            found_by: FoundBy::Migration,
            priority: u32::try_from(i).unwrap_or(u32::MAX),
            notes: String::new(),
            title: String::new(),
            score: None,
            moved_from: None,
            decided_by: None,
            decided_at: None,
        };
        let mut entry = audit.clone();
        entry.action = "price_page_migrated".into();
        let doc = serde_json::to_value(&page)?;
        tx.create(PricePage::KIND, &new_id(PricePage::PREFIX), None, Some(&name), &doc, entry).await?;
        made += 1;
    }
    job.pages_migrated = true;
    Ok(made)
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let router = register::<PricePage>(router, api, Ops { list: false, ..Ops::ALL });
    let view = api.schema::<Item<PricePageView>>();
    let one = api.schema::<Item<PricePage>>();
    api.add(
        api.op("get", "/price-pages", "listPricePages", "price-pages", "The price tracker's pages, with their state")
            .query("provider_id", "Only the pages of this provider.")
            .query("status", "Only pages in this status: proposed, active or dismissed.")
            .ok(json!({ "type": "array", "items": view })),
    );
    api.add(
        api.op("post", "/price-pages/{id}/confirm", "confirmPricePage", "price-pages", "Have the tracker read a page")
            .ok(one.clone()),
    );
    api.add(
        api.op(
            "post",
            "/price-pages/{id}/dismiss",
            "dismissPricePage",
            "price-pages",
            "Stop reading a page, and stop the scan from proposing it",
        )
        .ok(one),
    );
    api.add(
        api.op("post", "/price-pages/{id}/check", "checkPricePage", "price-pages", "Check and read a page now")
            .response("202", "Accepted", None),
    );
    router
        .route("/price-pages", get(list))
        .route("/price-pages/{id}/confirm", post(confirm))
        .route("/price-pages/{id}/dismiss", post(dismiss))
        .route("/price-pages/{id}/check", post(check))
}
