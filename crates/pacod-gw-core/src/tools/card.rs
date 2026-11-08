//! A remote agent's card (A2A 1.0, its section 4.4): what a probe reads of it, and the card the
//! gateway serves callers in its place.
//!
//! The gateway serves the card as approved, rewritten to itself: one interface, the gateway's
//! JSON-RPC endpoint for the agent, and the gateway's own security scheme (a gateway key). The
//! agent's signatures (JWS over the canonical card) cannot survive that rewriting and are
//! dropped; a caller that checks card signatures has nothing to check. Push notifications and
//! the extended card are switched off, since the gateway passes neither on.

use serde_json::{Value, json};

use super::pin;

/// The protocol version the gateway speaks to callers and to agents.
pub const VERSION: &str = "1.0";

/// Whether a card or a caller names the version the gateway speaks: `1.0`, also with a patch
/// number (`1.0.0`), which does not change the protocol; the official SDKs negotiate so. Another
/// minor version is another protocol.
pub fn is_spoken(version: &str) -> bool {
    let mut parts = version.trim().split('.');
    parts.next() == Some("1")
        && parts.next() == Some("0")
        && parts.all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// What a probe read of a card.
#[derive(Debug, Clone, PartialEq)]
pub struct CardSeen {
    /// The card without its signatures.
    pub card: Value,
    pub hash: String,
    /// The card's JSON-RPC interface of version 1.0.
    pub interface_url: String,
    pub tenant: Option<String>,
}

/// Reads a card: its JSON-RPC interface of version 1.0, and the hash of everything but its
/// signatures.
pub fn read_card(mut card: Value) -> Result<CardSeen, String> {
    let fields = card.as_object_mut().ok_or("the card is not a JSON object")?;
    fields.remove("signatures");
    let interfaces = card["supportedInterfaces"].as_array().ok_or("the card lists no supportedInterfaces")?;
    let jsonrpc = interfaces
        .iter()
        .find(|i| i["protocolBinding"] == "JSONRPC" && i["protocolVersion"].as_str().is_some_and(is_spoken))
        .ok_or("the card offers no JSON-RPC interface of version 1.0, which is the one the gateway speaks")?;
    let interface_url = jsonrpc["url"].as_str().filter(|u| !u.is_empty()).ok_or("the interface has no url")?.to_owned();
    let tenant = jsonrpc["tenant"].as_str().map(str::to_owned);
    let hash = pin::hash(&card);
    Ok(CardSeen { card, hash, interface_url, tenant })
}

/// The card callers are served for an agent the gateway serves at `endpoint`.
pub fn served_card(approved: &Value, endpoint: &str) -> Value {
    let mut card = approved.clone();
    card["supportedInterfaces"] =
        json!([{ "url": endpoint, "protocolBinding": "JSONRPC", "protocolVersion": VERSION }]);
    card["securitySchemes"] = json!({
        "gateway": { "httpAuthSecurityScheme": { "scheme": "bearer", "description": "A gateway API key of the caller's workspace." } }
    });
    card["securityRequirements"] = json!([{ "schemes": { "gateway": { "list": [] } } }]);
    let streaming = card["capabilities"]["streaming"].as_bool().unwrap_or(false);
    card["capabilities"]["streaming"] = json!(streaming);
    card["capabilities"]["pushNotifications"] = json!(false);
    card["capabilities"]["extendedAgentCard"] = json!(false);
    if let Some(fields) = card.as_object_mut() {
        fields.remove("signatures");
    }
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Value {
        json!({
            "name": "Researcher", "description": "Finds things.", "version": "1.2",
            "supportedInterfaces": [
                { "url": "https://agent.example/grpc", "protocolBinding": "GRPC", "protocolVersion": "1.0" },
                { "url": "https://agent.example/a2a", "protocolBinding": "JSONRPC", "protocolVersion": "1.0", "tenant": "t-7" }
            ],
            "capabilities": { "streaming": true, "pushNotifications": true, "extendedAgentCard": true },
            "securitySchemes": { "key": { "apiKeySecurityScheme": { "name": "X-Key", "location": "header" } } },
            "skills": [{ "id": "search", "name": "Search" }],
            "signatures": [{ "protected": "e30", "signature": "sig" }]
        })
    }

    #[test]
    fn a_card_is_read_for_its_json_rpc_interface_and_hashed_without_signatures() {
        let seen = read_card(card()).unwrap();
        assert_eq!(seen.interface_url, "https://agent.example/a2a");
        assert_eq!(seen.tenant.as_deref(), Some("t-7"));
        assert!(seen.card.get("signatures").is_none());
        let mut resigned = card();
        resigned["signatures"] = json!([{ "protected": "e30", "signature": "other" }]);
        assert_eq!(read_card(resigned).unwrap().hash, seen.hash, "signatures do not change the hash");
        let mut reworded = card();
        reworded["skills"][0]["name"] = json!("Search and send mail");
        assert_ne!(read_card(reworded).unwrap().hash, seen.hash);
        let mut grpc_only = card();
        grpc_only["supportedInterfaces"].as_array_mut().unwrap().pop();
        assert!(read_card(grpc_only).is_err());
        // A patch number is the same version; another minor one is not.
        let mut patched = card();
        patched["supportedInterfaces"][1]["protocolVersion"] = json!("1.0.0");
        assert_eq!(read_card(patched).unwrap().interface_url, "https://agent.example/a2a");
        let mut newer = card();
        newer["supportedInterfaces"][1]["protocolVersion"] = json!("1.1");
        assert!(read_card(newer).is_err());
    }

    #[test]
    fn version_1_0_is_spoken_with_or_without_a_patch_number() {
        for v in ["1.0", "1.0.0", " 1.0.3 "] {
            assert!(is_spoken(v), "{v}");
        }
        for v in ["0.3", "1", "1.1", "1.0.x", "1.0.", "2.0", "", "1.00"] {
            assert!(!is_spoken(v), "{v}");
        }
    }

    #[test]
    fn the_served_card_points_at_the_gateway_and_offers_what_it_passes_on() {
        let seen = read_card(card()).unwrap();
        let served = served_card(&seen.card, "https://gw.example/a2a/researcher");
        assert_eq!(
            served["supportedInterfaces"],
            json!([{ "url": "https://gw.example/a2a/researcher", "protocolBinding": "JSONRPC", "protocolVersion": "1.0" }])
        );
        assert!(served["securitySchemes"]["gateway"].is_object() && served["securitySchemes"].get("key").is_none());
        assert_eq!(
            served["capabilities"],
            json!({ "streaming": true, "pushNotifications": false, "extendedAgentCard": false })
        );
        assert_eq!(served["skills"], card()["skills"]);
        assert!(!served.to_string().contains("t-7"), "the agent's tenant is the gateway's business");
    }
}
