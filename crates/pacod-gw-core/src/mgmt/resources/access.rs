//! Users, role bindings and admin tokens.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crypto::keys::{ADMIN_TOKEN_PREFIX, generate};
use crate::crypto::password;
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Item, Ops, Resource, ScopeIds, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{AdminToken, Doc, Obj, RoleBinding, RoleName, RoleScope, Status, User, new_id};
use crate::snapshot::Snapshot;
use crate::store::{TokenPurpose, Tx};

/// Console passwords must be at least this long.
pub const MIN_PASSWORD_LEN: usize = 12;

// --- users -----------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UserInput {
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    /// Required when creating a user. On replace, leave it out to keep the current password.
    #[serde(default)]
    #[schemars(extend("writeOnly" = true))]
    pub password: Option<String>,
    #[serde(default)]
    pub status: Status,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UserView {
    pub email: String,
    pub display_name: String,
    pub status: Status,
    /// The user confirmed the address through the link in the verification email.
    pub email_verified: bool,
}

impl Resource for User {
    const PLURAL: &'static str = "users";
    const LABEL: &'static str = "user";
    const SCOPE: ScopeKind = ScopeKind::Global;
    type Input = UserInput;
    type View = UserView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.email.to_lowercase())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        let email = input.email.trim().to_lowercase();
        if !email.contains('@') || email.len() > 254 {
            return Err(ApiError::invalid("email must be an email address"));
        }
        let password_hash = match (&input.password, existing) {
            (Some(pw), _) => {
                if pw.chars().count() < MIN_PASSWORD_LEN {
                    return Err(ApiError::invalid(format!("password must be at least {MIN_PASSWORD_LEN} characters")));
                }
                password::hash(pw).map_err(ApiError::internal)?
            }
            (None, Some(e)) => e.doc.password_hash.clone(),
            (None, None) => return Err(ApiError::invalid("password is required")),
        };
        let display_name =
            if input.display_name.trim().is_empty() { email.clone() } else { input.display_name.trim().to_owned() };
        // A new address must be confirmed when the installation can send the email. An admin
        // who changes their own address keeps it confirmed, so nobody locks themselves out.
        let needs_confirmation = ctx.st.mailer.is_some() && ctx.snap.settings.require_verified_email;
        let email_verified = match existing {
            None => !needs_confirmation,
            Some(e) if e.doc.email != email && e.id != ctx.actor => !needs_confirmation,
            Some(e) => e.doc.email_verified,
        };
        Ok(User { email, display_name, password_hash, status: input.status, email_verified })
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        UserView {
            email: obj.doc.email.clone(),
            display_name: obj.doc.display_name.clone(),
            status: obj.doc.status,
            email_verified: obj.doc.email_verified,
        }
    }

    fn can_read(p: &Principal, _scope: &ScopeIds, _snap: &Snapshot) -> bool {
        p.is_platform_admin()
    }

    async fn check(tx: &mut Tx, doc: &Self, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<()> {
        // A password or status change ends the user's sessions, and withdraws reset links.
        if let Some(e) = existing
            && (e.doc.password_hash != doc.password_hash || doc.status != Status::Active)
        {
            ctx.st.store.sessions_delete_for_user(&e.id).await?;
            ctx.st.store.account_tokens_delete_for_user(&e.id, TokenPurpose::ResetPassword).await?;
        }
        let _ = tx;
        Ok(())
    }

    async fn after_commit(st: &AdminState, obj: &Obj<Self>, existing: Option<&Obj<Self>>) {
        // A new account, or a new address, that needs confirming gets the link.
        if !obj.doc.email_verified && existing.is_none_or(|e| e.doc.email != obj.doc.email) {
            crate::mgmt::account::send_in_background(st, obj.clone(), TokenPurpose::VerifyEmail);
        }
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        // Removing the last platform admin would lock everyone out.
        let bindings = tx.list(RoleBinding::KIND, None).await?;
        let mut admins = 0;
        let mut is_admin = false;
        for row in bindings {
            let b: Obj<RoleBinding> = row.into_obj()?;
            if b.doc.role == RoleName::PlatformAdmin {
                admins += 1;
                is_admin |= b.doc.principal == obj.id;
            }
        }
        if is_admin && admins <= 1 {
            return Err(ApiError::conflict("this is the last platform admin; give the role to someone else first"));
        }
        Ok(())
    }
}

// --- role bindings ---------------------------------------------------------------------------

impl Resource for RoleBinding {
    const PLURAL: &'static str = "role-bindings";
    const LABEL: &'static str = "role binding";
    const SCOPE: ScopeKind = ScopeKind::Global;
    type Input = RoleBinding;
    type View = RoleBinding;

    fn unique_name(doc: &Self) -> Option<String> {
        // One binding per (principal, role, scope).
        let scope = match &doc.scope {
            RoleScope::Platform => "platform".to_owned(),
            RoleScope::Organisation(o) => format!("org:{o}"),
            RoleScope::Workspace(w) => format!("ws:{w}"),
        };
        Some(format!("{}|{:?}|{scope}", doc.principal, doc.role))
    }

    fn build(input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !input.is_consistent() {
            return Err(ApiError::invalid("this role cannot be given in this kind of scope"));
        }
        let snap = ctx.snap;
        let known =
            snap.users.contains_key(&input.principal) || snap.admin_tokens.values().any(|t| t.id == input.principal);
        if !known {
            return Err(ApiError::invalid("principal must be the id of a user or an admin token"));
        }
        let target_exists = match &input.scope {
            RoleScope::Platform => true,
            RoleScope::Organisation(o) => snap.organisations.contains_key(o),
            RoleScope::Workspace(w) => snap.workspaces.contains_key(w),
        };
        if !target_exists {
            return Err(ApiError::invalid("the organisation or workspace of the scope does not exist"));
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    fn can_write(_p: &Principal, _scope: &ScopeIds, _snap: &Snapshot) -> bool {
        // Decided per binding in `authorize`.
        true
    }

    /// Platform admins bind anything. Organisation admins bind roles inside their organisation.
    fn authorize(p: &Principal, doc: &Self, snap: &Snapshot) -> bool {
        if p.is_platform_admin() {
            return true;
        }
        match &doc.scope {
            RoleScope::Platform => false,
            RoleScope::Organisation(o) => p.can_admin_org(o),
            RoleScope::Workspace(w) => snap.workspaces.get(w).is_some_and(|ws| p.can_admin_org(&ws.organisation_id)),
        }
    }

    fn visible(p: &Principal, obj: &Obj<Self>, snap: &Snapshot) -> bool {
        obj.doc.principal == p.id || Self::authorize(p, &obj.doc, snap)
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        if obj.doc.role != RoleName::PlatformAdmin {
            return Ok(());
        }
        let mut admins = 0;
        for row in tx.list(RoleBinding::KIND, None).await? {
            let b: Obj<RoleBinding> = row.into_obj()?;
            if b.doc.role == RoleName::PlatformAdmin {
                admins += 1;
            }
        }
        if admins <= 1 {
            return Err(ApiError::conflict("this is the last platform admin binding"));
        }
        Ok(())
    }
}

// --- admin tokens ----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AdminTokenInput {
    pub name: String,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub status: Status,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AdminTokenView {
    pub name: String,
    /// The last four characters of the token.
    pub hint: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub status: Status,
}

impl Resource for AdminToken {
    const PLURAL: &'static str = "admin-tokens";
    const LABEL: &'static str = "admin token";
    const SCOPE: ScopeKind = ScopeKind::Global;
    type Input = AdminTokenInput;
    type View = AdminTokenView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, _ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        let existing =
            existing.ok_or_else(|| ApiError::internal("admin tokens are created through their own endpoint"))?;
        validate_token_name(&input.name)?;
        Ok(AdminToken {
            name: input.name.trim().to_owned(),
            expires_at: input.expires_at,
            status: input.status,
            ..existing.doc.clone()
        })
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        AdminTokenView {
            name: obj.doc.name.clone(),
            hint: obj.doc.hint.clone(),
            expires_at: obj.doc.expires_at,
            status: obj.doc.status,
        }
    }

    fn can_read(p: &Principal, _scope: &ScopeIds, _snap: &Snapshot) -> bool {
        p.is_platform_admin()
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        // Role bindings of a deleted token would dangle: remove them with it.
        for row in tx.list(RoleBinding::KIND, None).await? {
            let b: Obj<RoleBinding> = row.into_obj()?;
            if b.doc.principal == obj.id {
                tx.delete(
                    &b.id,
                    crate::store::AuditEntry {
                        actor: "system".into(),
                        actor_kind: "system".into(),
                        detail: serde_json::json!({ "reason": "admin token deleted" }),
                        ..Default::default()
                    },
                )
                .await?;
            }
        }
        Ok(())
    }
}

fn validate_token_name(name: &str) -> ApiResult<()> {
    if name.trim().is_empty() || name.len() > 100 {
        return Err(ApiError::invalid("name must be 1 to 100 characters"));
    }
    Ok(())
}

/// An admin token together with its secret. Returned once, when the token is created.
#[derive(Debug, Serialize, JsonSchema)]
pub struct AdminTokenWithSecret {
    #[serde(flatten)]
    pub token: Item<AdminTokenView>,
    /// The full token. It cannot be shown again.
    pub secret: String,
}

async fn create_admin_token(
    State(st): State<AdminState>,
    p: Principal,
    Json(input): Json<AdminTokenInput>,
) -> ApiResult<(StatusCode, Json<AdminTokenWithSecret>)> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    validate_token_name(&input.name)?;
    let id = new_id(AdminToken::PREFIX);
    let secret = generate(ADMIN_TOKEN_PREFIX, id.trim_start_matches("tok_")).full;
    let doc = AdminToken {
        name: input.name.trim().to_owned(),
        secret_hmac: hex::encode(st.boot.pepper.hash(&secret)),
        hint: secret.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect(),
        expires_at: input.expires_at,
        status: Status::Active,
    };
    let mut tx = st.store.begin().await?;
    let row = tx
        .create(
            AdminToken::KIND,
            &id,
            None,
            Some(&doc.name),
            &serde_json::to_value(&doc).map_err(ApiError::internal)?,
            p.audit(None, None),
        )
        .await?;
    st.commit(tx).await?;
    let obj: Obj<AdminToken> = row.into_obj()?;
    Ok((
        StatusCode::CREATED,
        Json(AdminTokenWithSecret { token: Item { meta: obj.meta(), data: AdminToken::view(&obj) }, secret }),
    ))
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    router = register::<User>(router, api, Ops::ALL);
    router = register::<RoleBinding>(router, api, Ops { replace: false, ..Ops::ALL });
    router = register::<AdminToken>(router, api, Ops::NO_CREATE);

    let input = api.schema::<AdminTokenInput>();
    let with_secret = api.schema::<AdminTokenWithSecret>();
    api.add(
        api.op(
            "post",
            "/admin-tokens",
            "createAdminToken",
            "admin-tokens",
            "Create an admin token. The secret is returned once.",
        )
        .body(input)
        .created(with_secret),
    );
    router.route("/admin-tokens", post(create_admin_token))
}
