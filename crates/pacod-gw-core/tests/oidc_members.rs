//! Members of organisations and workspaces, and invitations to them, in OIDC mode: the
//! gateway's endpoints answered from pacod-id's scope members and invitations (started in
//! process), as `tests/members.rs` checks them in local mode.

mod common;
#[path = "common/id.rs"]
mod id;

use std::collections::HashMap;
use std::time::Duration;

use id::Id;
use pacod_id_authz::ScopeRef;
use serde_json::{Value, json};

/// An organisation with a workspace, registered at pacod-id, a platform admin (`root`) and
/// people with the given roles, signed in.
struct Org {
    id: Id,
    gw: common::TestGw,
    org: String,
    ws: String,
    /// name → (account id, access token)
    people: HashMap<&'static str, (String, String)>,
}

impl Org {
    async fn start(people: &[(&'static str, &str, &str)]) -> Org {
        let id = Id::start().await;
        let mut gw = id::gateway(&id, |_| {}).await;
        id.wait_for_gw_catalog().await;
        let root = id.account("root@example.com").await;
        id.bind(&root, "gw.platform_admin", ScopeRef::platform()).await;
        gw.token = token(&id, "root@example.com").await;
        signed_in(&gw, &gw.token).await;
        let org = gw.create("/orgs", json!({ "name": "acme", "display_name": "Acme" })).await.id;
        let ws = gw
            .create(&format!("/orgs/{org}/workspaces"), json!({ "name": "billing", "display_name": "Billing" }))
            .await
            .id;
        until("the workspace reaches pacod-id", async || registered(&id, &ws).await).await;
        let mut who = HashMap::new();
        for &(name, role, at) in people {
            let email = format!("{name}@example.com");
            let account = id.account(&email).await;
            let scope = if at == "ws" { ScopeRef::of("gw.workspace", &ws) } else { ScopeRef::organisation(&org) };
            id.bind(&account, &format!("gw.{role}"), scope).await;
            let t = token(&id, &email).await;
            signed_in(&gw, &t).await;
            who.insert(name, (account, t));
        }
        Org { id, gw, org, ws, people: who }
    }

    fn id_of(&self, who: &str) -> &str {
        &self.people[who].0
    }

    async fn call(&self, who: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.gw.api_as(&self.people[who].1, method, path, body).await
    }

    async fn invite(&self, who: &str, body: Value) -> Value {
        let (s, v) = self.call(who, "POST", &format!("/orgs/{}/invitations", self.org), Some(body)).await;
        assert_eq!(s, 201, "{v}");
        v
    }

    async fn invitations(&self) -> Vec<Value> {
        let (s, v) = self.gw.api("GET", &format!("/orgs/{}/invitations", self.org), None).await;
        assert_eq!(s, 200, "{v}");
        v["items"].as_array().unwrap().clone()
    }

    /// The roles of a principal, as `role@scope-type`, from this instance's copy of the feed.
    async fn roles_of(&self, principal: &str) -> Vec<String> {
        let (_, v) = self.gw.api("GET", "/role-bindings?limit=500", None).await;
        let mut out: Vec<String> = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["principal"] == principal)
            .map(|b| format!("{}@{}", b["role"].as_str().unwrap(), b["scope"]["type"].as_str().unwrap()))
            .collect();
        out.sort();
        out
    }

    /// The link token of the newest invitation mail to `to`.
    async fn link_to(&self, to: &str) -> String {
        for _ in 0..100 {
            if let Some(m) = self.id.mail().await.into_iter().rev().find(|m| m.to == to) {
                let text = &m.mail.text;
                let at = text.find("token=").expect("a link in the mail") + "token=".len();
                return text[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no invitation mail to {to}");
    }

    /// Whether an invitation link still works, as the invitation page asks pacod-id.
    async fn link_works(&self, token: &str) -> bool {
        let (s, v) = self.id.ui("/invitations/lookup", json!({ "token": token })).await;
        assert!(s == 200 || s == 410, "{s} {v}");
        s == 200
    }

    async fn stop(self) {
        self.gw.app.shutdown().await;
        self.id.running.stop().await;
    }
}

async fn token(id: &Id, email: &str) -> String {
    id.sign_in(email).await["access_token"].as_str().unwrap().to_owned()
}

/// Waits until the gateway's copy of the feed gives the token's holder a role.
async fn signed_in(gw: &common::TestGw, token: &str) {
    until("the role reaches the gateway", async || {
        gw.api_as(token, "GET", "/session", None).await.1["roles"].as_array().is_some_and(|r| !r.is_empty())
    })
    .await;
}

async fn registered(id: &Id, ws: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scopes WHERE type = 'gw.workspace' AND id = $1")
        .bind(ws)
        .fetch_one(id.store().pool())
        .await
        .unwrap()
        == 1
}

async fn until<F: AsyncFn() -> bool>(what: &str, check: F) {
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn organisation_admins_change_and_remove_members() {
    let o = Org::start(&[
        ("boss", "org_admin", "org"),
        ("sam", "viewer", "org"),
        ("viewer", "viewer", "org"),
        ("wsadmin", "workspace_admin", "ws"),
    ])
    .await;
    let org_members = format!("/orgs/{}/members", o.org);
    let ws_members = format!("/orgs/{}/workspaces/{}/members", o.org, o.ws);
    let sam = o.id_of("sam").to_owned();
    // Two roles in one scope; a change replaces both.
    o.id.bind(&sam, "gw.auditor", ScopeRef::organisation(&o.org)).await;

    let (s, list) = o.call("viewer", "GET", &org_members, None).await;
    assert_eq!(s, 200, "{list}");
    let names: Vec<&str> = list["items"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["boss", "sam", "viewer", "wsadmin"], "by name, as today");
    let sam_row = list["items"].as_array().unwrap().iter().find(|m| m["principal_id"] == sam.as_str()).unwrap();
    assert_eq!(sam_row["roles"].as_array().unwrap().len(), 2, "{sam_row}");
    assert_eq!((sam_row["email"].as_str(), sam_row["kind"].as_str()), (Some("sam@example.com"), Some("user")));
    assert_eq!(sam_row["roles"][0]["scope_name"], "Acme");

    let role = |r: &str| Some(json!({ "role": r }));
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/{sam}"), role("org_admin")).await.0, 204);
    assert_eq!(o.roles_of(&sam).await, ["org_admin@organisation"]);
    // Read your own writes: the member's next call has the new role.
    let (_, me) = o.call("sam", "GET", "/session", None).await;
    assert_eq!(me["roles"][0]["role"], "org_admin", "{me}");
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/{sam}"), role("workspace_admin")).await.0, 422);
    assert_eq!(o.call("boss", "PUT", &format!("{ws_members}/{sam}"), role("workspace_admin")).await.0, 204);
    assert_eq!(o.roles_of(&sam).await, ["org_admin@organisation", "workspace_admin@workspace"]);
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/usr_nobody"), role("viewer")).await.0, 404);

    // The workspace's list: its own roles, and the organisation's as inherited.
    let (s, list) = o.call("wsadmin", "GET", &ws_members, None).await;
    assert_eq!(s, 200, "{list}");
    let sam_row = list["items"].as_array().unwrap().iter().find(|m| m["principal_id"] == sam.as_str()).unwrap();
    let roles: Vec<(&str, bool)> = sam_row["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["role"].as_str().unwrap(), r["inherited"].as_bool().unwrap()))
        .collect();
    assert_eq!(roles, [("org_admin", true), ("workspace_admin", false)]);
    // Workspace admins and viewers see members but do not change them.
    assert_eq!(o.call("wsadmin", "GET", &org_members, None).await.0, 403);
    assert_eq!(o.call("wsadmin", "PUT", &format!("{ws_members}/{sam}"), role("viewer")).await.0, 403);
    assert_eq!(o.call("viewer", "DELETE", &format!("{org_members}/{sam}"), None).await.0, 403);

    // No role in the organisation leaves the workspace role; removing them takes everything.
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/{sam}"), Some(json!({ "role": null }))).await.0, 204);
    assert_eq!(o.roles_of(&sam).await, ["workspace_admin@workspace"]);
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/{sam}"), role("org_admin")).await.0, 204);
    assert_eq!(o.call("boss", "DELETE", &format!("{org_members}/{sam}"), None).await.0, 204);
    assert!(o.roles_of(&sam).await.is_empty());
    assert_eq!(o.call("boss", "DELETE", &format!("{org_members}/{sam}"), None).await.0, 404);

    // The last organisation admin does not lock the organisation out; a platform admin may.
    let boss = o.id_of("boss").to_owned();
    let (s, v) = o.call("boss", "PUT", &format!("{org_members}/{boss}"), role("viewer")).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (409, Some("last_org_admin")), "{v}");
    assert_eq!(o.call("boss", "DELETE", &format!("{org_members}/{boss}"), None).await.0, 409);
    assert_eq!(o.roles_of(&boss).await, ["org_admin@organisation"]);
    assert_eq!(o.gw.api("DELETE", &format!("{org_members}/{boss}"), None).await.0, 204);

    // The writes are in the organisation's slice of the gateway's audit log (boss, removed, no
    // longer reads it).
    let log = format!("/audit-log?organisation_id={}&limit=50", o.org);
    until("the writes are in the organisation's audit log", async || {
        let (_, v) = o.gw.api("GET", &log, None).await;
        let actions: Vec<&str> =
            v["items"].as_array().into_iter().flatten().filter_map(|e| e["action"].as_str()).collect();
        actions.contains(&"set_member_role") && actions.contains(&"remove_member")
    })
    .await;
    o.stop().await;
}

#[tokio::test]
async fn invitations_are_replaced_revoked_and_expire() {
    let o =
        Org::start(&[("boss", "org_admin", "org"), ("viewer", "viewer", "org"), ("wsadmin", "workspace_admin", "ws")])
            .await;
    let invitations = format!("/orgs/{}/invitations", o.org);

    // Only organisation admins invite, and only to roles that fit the scope.
    let body = json!({ "email": "x@example.com", "role": "viewer" });
    assert_eq!(o.call("viewer", "POST", &invitations, Some(body.clone())).await.0, 403);
    assert_eq!(o.call("wsadmin", "POST", &invitations, Some(body.clone())).await.0, 403);
    assert_eq!(o.call("viewer", "GET", &invitations, None).await.0, 403);
    let wrong = json!({ "email": "x@example.com", "role": "workspace_admin" });
    assert_eq!(o.call("boss", "POST", &invitations, Some(wrong)).await.0, 422);
    let foreign = json!({ "email": "x@example.com", "role": "viewer", "workspace_id": "ws_nope" });
    assert_eq!(o.call("boss", "POST", &invitations, Some(foreign)).await.0, 422);
    let platform = json!({ "email": "x@example.com", "role": "platform_admin" });
    assert_eq!(o.call("boss", "POST", &invitations, Some(platform)).await.0, 422);
    let (s, v) =
        o.call("boss", "POST", &invitations, Some(json!({ "email": "not an address", "role": "viewer" }))).await;
    assert_eq!((s, v["error"]["message"].as_str()), (422, Some("email must be an email address")));

    // Inviting the same address to the same place again replaces the invitation and its link.
    let first = o.invite("boss", body).await;
    assert_eq!((first["emailed"].as_bool(), first["token"].is_null()), (Some(true), true), "{first}");
    assert_eq!(
        (first["invited_by"].as_str(), first["organisation_name"].as_str(), first["role"].as_str()),
        (Some("boss"), Some("Acme"), Some("viewer"))
    );
    let first_link = o.link_to("x@example.com").await;
    let second = o.invite("boss", json!({ "email": "X@example.com", "role": "auditor" })).await;
    assert_eq!(first["id"], second["id"]);
    let second_link = o.link_to("x@example.com").await;
    let list = o.invitations().await;
    assert_eq!((list.len(), list[0]["role"].as_str()), (1, Some("auditor")));
    assert!(!o.link_works(&first_link).await);
    assert!(o.link_works(&second_link).await);

    // Sending it again gives a new link.
    let id = second["id"].as_str().unwrap();
    let (s, again) = o.call("boss", "POST", &format!("{invitations}/{id}/resend"), None).await;
    assert_eq!((s, again["id"].as_str()), (200, Some(id)), "{again}");
    let third_link = o.link_to("x@example.com").await;
    assert!(!o.link_works(&second_link).await);
    assert!(o.link_works(&third_link).await);
    // Not through another organisation's path.
    let other = o.gw.create("/orgs", json!({ "name": "other" })).await.id;
    assert_eq!(o.gw.api("DELETE", &format!("/orgs/{other}/invitations/{id}"), None).await.0, 404);
    assert_eq!(o.gw.api("POST", &format!("/orgs/{other}/invitations/{id}/resend"), None).await.0, 404);

    // Revoked, the link is dead.
    assert_eq!(o.call("boss", "DELETE", &format!("{invitations}/{id}"), None).await.0, 204);
    assert_eq!(o.call("boss", "DELETE", &format!("{invitations}/{id}"), None).await.0, 404);
    assert!(!o.link_works(&third_link).await);
    assert!(o.invitations().await.is_empty());

    // To a workspace: listed with the organisation's and on its own.
    let to_ws = o.invite("boss", json!({ "email": "w@example.com", "role": "viewer", "workspace_id": o.ws })).await;
    assert_eq!(to_ws["workspace_name"], "Billing");
    let (_, v) = o.gw.api("GET", &format!("{invitations}?workspace_id={}", o.ws), None).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1, "{v}");
    let (_, v) = o.gw.api("GET", &format!("{invitations}?workspace_id=ws_nope"), None).await;
    assert_eq!(v["items"], json!([]));

    // Expired, the list says so.
    sqlx::query("UPDATE invitations SET expires_at = now() - interval '1 minute'")
        .execute(o.id.store().pool())
        .await
        .unwrap();
    assert_eq!(o.invitations().await[0]["expired"], true);
    o.stop().await;
}

#[tokio::test]
async fn someone_new_accepts_at_pacod_id_and_becomes_a_member() {
    let o = Org::start(&[("boss", "org_admin", "org")]).await;
    o.invite("boss", json!({ "email": "nia@example.com", "role": "viewer", "workspace_id": o.ws })).await;
    let link = o.link_to("nia@example.com").await;
    let (s, v) =
        o.id.ui(
            "/invitations/accept",
            json!({ "token": link, "password": "a long passphrase for nia", "display_name": "Nia" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let ws_members = format!("/orgs/{}/workspaces/{}/members", o.org, o.ws);
    until("Nia is a member of the workspace", async || {
        o.call("boss", "GET", &ws_members, None).await.1["items"]
            .as_array()
            .is_some_and(|m| m.iter().any(|m| m["name"] == "Nia" && m["roles"][0]["role"] == "viewer"))
    })
    .await;
    assert!(o.invitations().await.is_empty(), "accepted, it is gone");
    o.stop().await;
}

#[tokio::test]
async fn a_new_organisation_takes_invitations_at_once() {
    // The console creates an organisation and invites its admin in one go: the organisation
    // may still be in the identity outbox when the invitation comes.
    let o = Org::start(&[]).await;
    for n in 0..4 {
        let org = o.gw.create("/orgs", json!({ "name": format!("new-{n}") })).await.id;
        let (s, v) =
            o.gw.api(
                "POST",
                &format!("/orgs/{org}/invitations"),
                Some(json!({ "email": format!("admin{n}@example.com"), "role": "org_admin" })),
            )
            .await;
        assert_eq!(s, 201, "{v}");
    }
    o.stop().await;
}

#[tokio::test]
async fn deleting_a_workspace_or_organisation_withdraws_its_invitations() {
    let o = Org::start(&[("boss", "org_admin", "org")]).await;
    o.invite("boss", json!({ "email": "a@example.com", "role": "viewer", "workspace_id": o.ws })).await;
    let to_ws = o.link_to("a@example.com").await;
    o.invite("boss", json!({ "email": "b@example.com", "role": "viewer" })).await;
    assert_eq!(o.gw.api("DELETE", &format!("/orgs/{}/workspaces/{}", o.org, o.ws), None).await.0, 204);
    until("the workspace leaves pacod-id", async || !registered(&o.id, &o.ws).await).await;
    let left = o.invitations().await;
    assert_eq!((left.len(), left[0]["email"].as_str()), (1, Some("b@example.com")));
    assert!(!o.link_works(&to_ws).await);

    assert_eq!(o.gw.api("DELETE", &format!("/orgs/{}", o.org), None).await.0, 204);
    until("the organisation's invitations are gone", async || {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM invitations").fetch_one(o.id.store().pool()).await.unwrap()
            == 0
    })
    .await;
    o.stop().await;
}
