//! Members of organisations and workspaces, and invitations to them, end to end.

mod common;

use std::collections::HashMap;

use common::TestGw;
use serde_json::{Value, json};
use sqlx::Connection;

const PASSWORD: &str = "a long password 1";

/// An organisation with a workspace, and users with the given roles, signed in.
struct Org {
    gw: TestGw,
    org: String,
    ws: String,
    users: HashMap<&'static str, (String, String)>,
}

impl Org {
    async fn start(people: &[(&'static str, &str, &str)]) -> Org {
        let gw = TestGw::start(None).await;
        let org = gw.create("/orgs", json!({ "name": "acme", "display_name": "Acme" })).await.id;
        let ws = gw
            .create(&format!("/orgs/{org}/workspaces"), json!({ "name": "billing", "display_name": "Billing" }))
            .await
            .id;
        let mut users = HashMap::new();
        for &(name, role, scope) in people {
            let email = format!("{name}@example.test");
            let id = gw.create("/users", json!({ "email": email, "password": PASSWORD })).await.id;
            let scope = match scope {
                "org" => json!({ "type": "organisation", "id": org }),
                "ws" => json!({ "type": "workspace", "id": ws }),
                _ => Value::Null,
            };
            if !scope.is_null() {
                gw.create("/role-bindings", json!({ "principal": id, "role": role, "scope": scope })).await;
            }
            let token = gw.login(&email, PASSWORD).await;
            users.insert(name, (id, token));
        }
        Org { gw, org, ws, users }
    }

    fn id(&self, who: &str) -> &str {
        &self.users[who].0
    }

    async fn call(&self, who: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.gw.api_as(&self.users[who].1, method, path, body).await
    }

    /// A call without credentials, as the invitation page makes it for someone signed out.
    async fn public(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.gw.http.post(format!("{}/api/v1{path}", self.gw.admin)).json(&body).send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn invite(&self, who: &str, body: Value) -> Value {
        let (st, v) = self.call(who, "POST", &format!("/orgs/{}/invitations", self.org), Some(body)).await;
        assert_eq!(st, 201, "{v}");
        v
    }

    async fn invitations(&self) -> Vec<Value> {
        let (st, v) = self.gw.api("GET", &format!("/orgs/{}/invitations", self.org), None).await;
        assert_eq!(st, 200, "{v}");
        v["items"].as_array().unwrap().clone()
    }

    /// The roles of a principal, as `role@scope-type`.
    async fn roles_of(&self, principal: &str) -> Vec<String> {
        let (_, v) = self.gw.api("GET", "/role-bindings", None).await;
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
}

fn error_type(v: &Value) -> &str {
    v["error"]["type"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn someone_new_accepts_an_invitation_with_a_password() {
    let o = Org::start(&[("boss", "org_admin", "org")]).await;
    let sent = o.invite("boss", json!({ "email": " New@Example.test ", "role": "viewer", "workspace_id": o.ws })).await;
    // Without email the inviter gets the token to hand on.
    assert_eq!((sent["email"].as_str(), sent["emailed"].as_bool()), (Some("new@example.test"), Some(false)));
    assert_eq!(
        (sent["workspace_name"].as_str(), sent["invited_by"].as_str()),
        (Some("Billing"), Some("boss@example.test"))
    );
    let token = sent["token"].as_str().expect("token").to_owned();
    assert_eq!(o.invitations().await.len(), 1);

    let (st, preview) = o.public("/session/invitation/lookup", json!({ "token": token })).await;
    assert_eq!(st, 200, "{preview}");
    assert_eq!(preview["account_exists"], false);
    assert_eq!(preview["organisation_name"], "Acme");

    // A short password can be corrected: the link still works afterwards.
    let (st, v) = o.public("/session/invitation/accept", json!({ "token": token, "password": "short" })).await;
    assert_eq!(st, 422, "{v}");
    let (st, v) = o
        .public("/session/invitation/accept", json!({ "token": token, "password": PASSWORD, "display_name": "Nora" }))
        .await;
    assert_eq!((st, v["account_created"].as_bool()), (200, Some(true)), "{v}");

    // The link works once.
    let (st, v) = o.public("/session/invitation/accept", json!({ "token": token, "password": PASSWORD })).await;
    assert_eq!((st, error_type(&v)), (400, "invalid_token"), "{v}");
    assert!(o.invitations().await.is_empty());

    // The new account signs in and has the role; the workspace's members include it.
    let session = o.gw.login("new@example.test", PASSWORD).await;
    let (_, me) = o.gw.api_as(&session, "GET", "/session", None).await;
    assert_eq!(me["roles"][0]["role"], "viewer");
    assert_eq!(me["roles"][0]["scope_name"], "Billing");
    let (st, members) =
        o.gw.api_as(&session, "GET", &format!("/orgs/{}/workspaces/{}/members", o.org, o.ws), None).await;
    assert_eq!(st, 200, "{members}");
    let by_name: HashMap<&str, &Value> =
        members["items"].as_array().unwrap().iter().map(|m| (m["name"].as_str().unwrap(), m)).collect();
    assert_eq!(by_name["Nora"]["roles"][0]["inherited"], false);
    assert_eq!(
        by_name["boss@example.test"]["roles"][0]["inherited"], true,
        "the org admin's role applies through the organisation"
    );
}

#[tokio::test]
async fn someone_with_an_account_accepts_while_signed_in() {
    let o = Org::start(&[("boss", "org_admin", "org"), ("bea", "", "")]).await;
    let sent = o.invite("boss", json!({ "email": "bea@example.test", "role": "auditor" })).await;
    let token = sent["token"].as_str().unwrap().to_owned();

    let (_, preview) = o.public("/session/invitation/lookup", json!({ "token": token })).await;
    assert_eq!(preview["account_exists"], true);
    // Signed out, or signed in as someone else, it is refused.
    let (st, v) = o.public("/session/invitation/accept", json!({ "token": token })).await;
    assert_eq!((st, error_type(&v)), (401, "sign_in_required"), "{v}");
    let (st, _) = o.call("boss", "POST", "/session/invitation/accept", Some(json!({ "token": token }))).await;
    assert_eq!(st, 401);
    let (st, v) = o.call("bea", "POST", "/session/invitation/accept", Some(json!({ "token": token }))).await;
    assert_eq!((st, v["account_created"].as_bool()), (200, Some(false)), "{v}");
    assert_eq!(o.roles_of(o.id("bea")).await, ["auditor@organisation"]);

    // Invitations also wait in the account, to accept or decline without the link.
    o.invite("boss", json!({ "email": "bea@example.test", "role": "workspace_admin", "workspace_id": o.ws })).await;
    let (_, mine) = o.call("bea", "GET", "/session/invitations", None).await;
    let items = mine["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{mine}");
    let id = items[0]["id"].as_str().unwrap();
    assert_eq!(o.call("boss", "POST", &format!("/session/invitations/{id}/accept"), None).await.0, 404, "not boss's");
    assert_eq!(o.call("bea", "POST", &format!("/session/invitations/{id}/accept"), None).await.0, 204);
    assert_eq!(o.roles_of(o.id("bea")).await, ["auditor@organisation", "workspace_admin@workspace"]);

    // Accepting replaces the role held in that place: one role per place.
    o.invite("boss", json!({ "email": "bea@example.test", "role": "viewer" })).await;
    let (_, mine) = o.call("bea", "GET", "/session/invitations", None).await;
    let id = mine["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(o.call("bea", "POST", &format!("/session/invitations/{id}/accept"), None).await.0, 204);
    assert_eq!(o.roles_of(o.id("bea")).await, ["viewer@organisation", "workspace_admin@workspace"]);

    // The last organisation admin cannot step down by accepting a lesser role.
    let boss_invite = o.invite("boss", json!({ "email": "boss@example.test", "role": "viewer" })).await;
    let (st, v) =
        o.call("boss", "POST", "/session/invitation/accept", Some(json!({ "token": boss_invite["token"] }))).await;
    assert_eq!((st, v["error"]["details"]["code"].as_str()), (409, Some("last_org_admin")), "{v}");

    o.invite("boss", json!({ "email": "bea@example.test", "role": "viewer", "workspace_id": o.ws })).await;
    let (_, mine) = o.call("bea", "GET", "/session/invitations", None).await;
    let id = mine["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(o.call("bea", "POST", &format!("/session/invitations/{id}/decline"), None).await.0, 204);
    let (_, mine) = o.call("bea", "GET", "/session/invitations", None).await;
    assert!(mine["items"].as_array().unwrap().is_empty());
    assert_eq!(o.roles_of(o.id("bea")).await, ["viewer@organisation", "workspace_admin@workspace"]);
}

#[tokio::test]
async fn invitations_are_replaced_revoked_and_expire() {
    let o =
        Org::start(&[("boss", "org_admin", "org"), ("viewer", "viewer", "org"), ("wsadmin", "workspace_admin", "ws")])
            .await;
    let invitations = format!("/orgs/{}/invitations", o.org);

    // Only organisation admins invite, and only to roles that fit the scope.
    let body = json!({ "email": "x@example.test", "role": "viewer" });
    assert_eq!(o.call("viewer", "POST", &invitations, Some(body.clone())).await.0, 403);
    assert_eq!(o.call("wsadmin", "POST", &invitations, Some(body.clone())).await.0, 403);
    assert_eq!(o.call("viewer", "GET", &invitations, None).await.0, 403);
    let wrong = json!({ "email": "x@example.test", "role": "workspace_admin" });
    assert_eq!(o.call("boss", "POST", &invitations, Some(wrong)).await.0, 422);
    let foreign = json!({ "email": "x@example.test", "role": "viewer", "workspace_id": "ws_nope" });
    assert_eq!(o.call("boss", "POST", &invitations, Some(foreign)).await.0, 422);
    let platform = json!({ "email": "x@example.test", "role": "platform_admin" });
    assert_eq!(o.call("boss", "POST", &invitations, Some(platform)).await.0, 422);

    // Inviting the same address to the same place again replaces the invitation and its link.
    let first = o.invite("boss", body).await;
    let second = o.invite("boss", json!({ "email": "x@example.test", "role": "auditor" })).await;
    assert_eq!(first["id"], second["id"]);
    let list = o.invitations().await;
    assert_eq!((list.len(), list[0]["role"].as_str()), (1, Some("auditor")));
    let lookup = |t: &Value| json!({ "token": t.as_str().unwrap() });
    assert_eq!(o.public("/session/invitation/lookup", lookup(&first["token"])).await.0, 400);
    assert_eq!(o.public("/session/invitation/lookup", lookup(&second["token"])).await.0, 200);

    // Sending it again gives a new link.
    let (st, again) =
        o.call("boss", "POST", &format!("{invitations}/{}/resend", second["id"].as_str().unwrap()), None).await;
    assert_eq!(st, 200, "{again}");
    assert_eq!(o.public("/session/invitation/lookup", lookup(&second["token"])).await.0, 400);
    assert_eq!(o.public("/session/invitation/lookup", lookup(&again["token"])).await.0, 200);

    // Revoked, the link is dead.
    assert_eq!(
        o.call("boss", "DELETE", &format!("{invitations}/{}", again["id"].as_str().unwrap()), None).await.0,
        204
    );
    assert_eq!(o.public("/session/invitation/lookup", lookup(&again["token"])).await.0, 400);
    assert!(o.invitations().await.is_empty());

    // Expired, the link is dead too, and the list says so.
    let late = o.invite("boss", json!({ "email": "late@example.test", "role": "viewer" })).await;
    let mut db = sqlx::PgConnection::connect(&o.gw.database_url).await.unwrap();
    sqlx::query("UPDATE invitations SET expires_at = now() - interval '1 minute'").execute(&mut db).await.unwrap();
    assert_eq!(o.public("/session/invitation/lookup", lookup(&late["token"])).await.0, 400);
    assert_eq!(o.invitations().await[0]["expired"], true);
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
    let sam = o.id("sam").to_owned();
    // Data can hold two roles in one scope; a change replaces both.
    o.gw.create(
        "/role-bindings",
        json!({ "principal": sam, "role": "auditor", "scope": { "type": "organisation", "id": o.org } }),
    )
    .await;

    let (st, list) = o.call("viewer", "GET", &org_members, None).await;
    assert_eq!(st, 200, "{list}");
    let sam_row = list["items"].as_array().unwrap().iter().find(|m| m["principal_id"] == sam.as_str()).unwrap().clone();
    assert_eq!(sam_row["roles"].as_array().unwrap().len(), 2);
    assert_eq!(sam_row["email"], "sam@example.test");

    let role = |r: &str| Some(json!({ "role": r }));
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/{sam}"), role("org_admin")).await.0, 204);
    assert_eq!(o.roles_of(&sam).await, ["org_admin@organisation"]);
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/{sam}"), role("workspace_admin")).await.0, 422);
    assert_eq!(o.call("boss", "PUT", &format!("{ws_members}/{sam}"), role("workspace_admin")).await.0, 204);
    assert_eq!(o.roles_of(&sam).await, ["org_admin@organisation", "workspace_admin@workspace"]);
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/usr_nobody"), role("viewer")).await.0, 404);

    // Workspace admins and viewers see members but do not change them.
    assert_eq!(o.call("wsadmin", "GET", &ws_members, None).await.0, 200);
    assert_eq!(o.call("wsadmin", "GET", &org_members, None).await.0, 403);
    assert_eq!(o.call("wsadmin", "PUT", &format!("{ws_members}/{sam}"), role("viewer")).await.0, 403);
    assert_eq!(o.call("viewer", "DELETE", &format!("{org_members}/{sam}"), None).await.0, 403);

    // No role in the organisation leaves the workspace role; removing them takes everything.
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/{sam}"), Some(json!({ "role": null }))).await.0, 204);
    assert_eq!(o.roles_of(&sam).await, ["workspace_admin@workspace"]);
    assert_eq!(o.call("boss", "PUT", &format!("{org_members}/{sam}"), role("org_admin")).await.0, 204);

    // Removing someone from the organisation takes their workspace roles too.
    assert_eq!(o.call("boss", "DELETE", &format!("{org_members}/{sam}"), None).await.0, 204);
    assert!(o.roles_of(&sam).await.is_empty());
    assert_eq!(o.call("boss", "DELETE", &format!("{org_members}/{sam}"), None).await.0, 404);

    // The last organisation admin does not lock the organisation out; a platform admin may.
    let boss = o.id("boss").to_owned();
    let (st, v) = o.call("boss", "PUT", &format!("{org_members}/{boss}"), role("viewer")).await;
    assert_eq!((st, v["error"]["details"]["code"].as_str()), (409, Some("last_org_admin")), "{v}");
    assert_eq!(o.call("boss", "DELETE", &format!("{org_members}/{boss}"), None).await.0, 409);
    assert_eq!(o.roles_of(&boss).await, ["org_admin@organisation"]);
    assert_eq!(o.gw.api("DELETE", &format!("{org_members}/{boss}"), None).await.0, 204);
}

#[tokio::test]
async fn deleting_a_workspace_or_organisation_withdraws_its_invitations() {
    let o = Org::start(&[("boss", "org_admin", "org")]).await;
    let to_ws = o.invite("boss", json!({ "email": "a@example.test", "role": "viewer", "workspace_id": o.ws })).await;
    o.invite("boss", json!({ "email": "b@example.test", "role": "viewer" })).await;
    assert_eq!(o.gw.api("DELETE", &format!("/orgs/{}/workspaces/{}", o.org, o.ws), None).await.0, 204);
    let left = o.invitations().await;
    assert_eq!((left.len(), left[0]["email"].as_str()), (1, Some("b@example.test")));
    let (st, _) = o.public("/session/invitation/lookup", json!({ "token": to_ws["token"] })).await;
    assert_eq!(st, 400);

    assert_eq!(o.gw.api("DELETE", &format!("/orgs/{}", o.org), None).await.0, 204);
    let mut db = sqlx::PgConnection::connect(&o.gw.database_url).await.unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM invitations").fetch_one(&mut db).await.unwrap();
    assert_eq!(n, 0);
}
