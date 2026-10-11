//! Google service-account tokens, for recording Vertex AI and for the live smoke test.
//!
//! A service-account key (the JSON file Google Cloud hands out) is turned into an OAuth access
//! token by signing a JWT with the key (RS256) and exchanging it at Google's token endpoint
//! (<https://developers.google.com/identity/protocols/oauth2/service-account>). The gateway does
//! the same in `pacod-gw-core` (`upstream/google.rs`); this copy keeps the testkit free of the
//! core, and a test in the core checks that both write the same assertion.
//!
//! `testdata/test-service-account.json` holds a key made for the tests only: it belongs to no
//! Google project and opens nothing.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

/// Google's token endpoint, which is also the audience of the assertion.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// What the token may do: Vertex AI needs the whole cloud platform scope.
pub const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// A key made for tests only (see the module documentation).
pub const TEST_SERVICE_ACCOUNT: &str = include_str!("../testdata/test-service-account.json");

/// The parts of a service-account key that signing needs.
pub struct ServiceAccount {
    pub client_email: String,
    pub private_key_id: String,
    key: RsaKeyPair,
}

impl ServiceAccount {
    /// Reads a service-account key file's JSON.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let v: Value = serde_json::from_str(text).context("the service-account key is not JSON")?;
        if v["type"] != "service_account" {
            bail!("the key file is not a service-account key (type is not \"service_account\")");
        }
        let field = |name: &str| {
            v[name].as_str().filter(|s| !s.is_empty()).map(str::to_owned).with_context(|| format!("{name} is missing"))
        };
        let der = pem_der(&field("private_key")?)?;
        let key = RsaKeyPair::from_pkcs8(&der).map_err(|e| anyhow::anyhow!("the private key is not usable: {e}"))?;
        Ok(Self { client_email: field("client_email")?, private_key_id: field("private_key_id")?, key })
    }

    /// The signed JWT that asks for a token valid for an hour from `now` (seconds since the
    /// epoch), addressed to `audience`.
    pub fn assertion(&self, now: u64, audience: &str) -> anyhow::Result<String> {
        let header = json!({ "alg": "RS256", "typ": "JWT", "kid": self.private_key_id });
        let claims =
            json!({ "iss": self.client_email, "scope": SCOPE, "aud": audience, "iat": now, "exp": now + 3600 });
        let signing_input =
            format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), signing_input.as_bytes(), &mut signature)
            .map_err(|_| anyhow::anyhow!("signing the assertion failed"))?;
        Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }

    /// The public key, as DER `RSAPublicKey`, to check a signature with.
    pub fn public_key(&self) -> Vec<u8> {
        self.key.public().as_ref().to_vec()
    }
}

/// Exchanges a signed assertion for an access token at `token_url`.
pub async fn mint(http: &reqwest::Client, account: &ServiceAccount, token_url: &str) -> anyhow::Result<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let assertion = account.assertion(now, TOKEN_URL)?;
    let form = format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}");
    let response = http
        .post(token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
        .context("the token endpoint did not answer")?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    match body["access_token"].as_str() {
        Some(token) if status.is_success() => Ok(token.to_owned()),
        // Google's error names say what is wrong (invalid_grant, invalid_client) and hold no secret.
        _ => bail!("the token endpoint answered {status}: {}", body["error"].as_str().unwrap_or("no token")),
    }
}

/// The DER bytes of a PEM `PRIVATE KEY` block.
fn pem_der(pem: &str) -> anyhow::Result<Vec<u8>> {
    let body: String = pem.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("-----")).collect();
    if !pem.contains("-----BEGIN PRIVATE KEY-----") {
        bail!("the private key is not a PKCS#8 PEM block");
    }
    STANDARD.decode(body).context("the private key's PEM is not base64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    #[test]
    fn the_assertion_is_signed_with_the_key() {
        let account = ServiceAccount::parse(TEST_SERVICE_ACCOUNT).unwrap();
        let jwt = account.assertion(1_800_000_000, TOKEN_URL).unwrap();
        let mut parts = jwt.split('.');
        let (header, claims, signature) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).unwrap()).unwrap();
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
        assert_eq!(
            (header["alg"].as_str(), header["kid"].as_str()),
            (Some("RS256"), Some(account.private_key_id.as_str()))
        );
        assert_eq!(claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(), 3600);
        assert_eq!((claims["aud"].as_str(), claims["scope"].as_str()), (Some(TOKEN_URL), Some(SCOPE)));
        let signed = &jwt[..jwt.rfind('.').unwrap()];
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, account.public_key())
            .verify(signed.as_bytes(), &URL_SAFE_NO_PAD.decode(signature).unwrap())
            .expect("the signature checks out");
        // PKCS#1 v1.5 signatures are deterministic: the same input gives the same assertion.
        assert_eq!(jwt, account.assertion(1_800_000_000, TOKEN_URL).unwrap());
    }

    #[test]
    fn other_files_are_refused() {
        assert!(ServiceAccount::parse("{}").is_err());
        assert!(ServiceAccount::parse(r#"{"type":"authorized_user"}"#).is_err());
        let mut v: Value = serde_json::from_str(TEST_SERVICE_ACCOUNT).unwrap();
        v["private_key"] = json!("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        assert!(ServiceAccount::parse(&v.to_string()).is_err());
    }
}
