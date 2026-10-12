//! Compact JSON Web Signatures (RFC 7515) with RS256 and ES256, on ring, and JSON Web Key sets
//! (RFC 7517).
//!
//! Small on purpose: pacod-id signs its id_tokens, access tokens and logout tokens with it, and
//! products check them. Only the two algorithms OpenID Connect and RFC 9068 need are known;
//! `none`, `crit` headers and unknown algorithms are refused. Claims (expiry, issuer, audience)
//! are the caller's business: this crate only proves who signed what.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::SystemRandom;
use ring::signature::{
    self, ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair, RSA_PKCS1_SHA256,
    RsaKeyPair, RsaPublicKeyComponents, UnparsedPublicKey,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The longest token accepted. Ours are well below 4 KiB; this only stops abuse.
pub const MAX_TOKEN_LEN: usize = 16 * 1024;

/// A signature algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Alg {
    RS256,
    ES256,
}

impl Alg {
    pub fn as_str(self) -> &'static str {
        match self {
            Alg::RS256 => "RS256",
            Alg::ES256 => "ES256",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoseError {
    #[error("not a compact JWS")]
    Malformed,
    #[error("the token is too long")]
    TooLong,
    #[error("the algorithm is not accepted here")]
    BadAlg,
    #[error("no key with id {0:?}")]
    UnknownKid(String),
    #[error("the signature does not verify")]
    BadSignature,
    #[error("the key cannot be used: {0}")]
    BadKey(String),
}

/// The protected header of a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub alg: Alg,
    pub kid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typ: Option<String>,
}

/// One public key of a JWKS, RSA or EC P-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub kid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
    #[serde(default, rename = "use", skip_serializing_if = "Option::is_none")]
    pub use_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crv: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<String>,
}

/// A JSON Web Key Set, as `/oidc/jwks` serves it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

enum Signer {
    Rs256(RsaKeyPair),
    Es256(EcdsaKeyPair),
}

/// A private key that signs tokens.
pub struct SigningKey {
    kid: String,
    signer: Signer,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey").field("kid", &self.kid).field("alg", &self.alg()).finish_non_exhaustive()
    }
}

impl SigningKey {
    /// Reads a PKCS#8 private key (DER). RS256 keys must have a 2048-bit modulus or longer.
    pub fn from_pkcs8(kid: impl Into<String>, alg: Alg, der: &[u8]) -> Result<Self, JoseError> {
        let signer = match alg {
            Alg::RS256 => Signer::Rs256(RsaKeyPair::from_pkcs8(der).map_err(|e| JoseError::BadKey(e.to_string()))?),
            Alg::ES256 => Signer::Es256(
                EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, der, &SystemRandom::new())
                    .map_err(|e| JoseError::BadKey(e.to_string()))?,
            ),
        };
        if let Signer::Rs256(k) = &signer
            && k.public().modulus_len() < 256
        {
            return Err(JoseError::BadKey("RSA keys need at least 2048 bits".into()));
        }
        Ok(SigningKey { kid: kid.into(), signer })
    }

    /// A new ES256 key, with its PKCS#8 form to store. RSA keys are made elsewhere (ring
    /// cannot generate them) and read with [`SigningKey::from_pkcs8`].
    pub fn generate_es256(kid: impl Into<String>) -> Result<(Self, Vec<u8>), JoseError> {
        let rng = SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| JoseError::BadKey("could not generate a P-256 key".into()))?;
        let der = doc.as_ref().to_vec();
        Ok((Self::from_pkcs8(kid, Alg::ES256, &der)?, der))
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    pub fn alg(&self) -> Alg {
        match self.signer {
            Signer::Rs256(_) => Alg::RS256,
            Signer::Es256(_) => Alg::ES256,
        }
    }

    /// The public half, for the JWKS.
    pub fn public_jwk(&self) -> Jwk {
        let base = |kty: &str| Jwk {
            kty: kty.into(),
            kid: self.kid.clone(),
            alg: Some(self.alg().as_str().into()),
            use_: Some("sig".into()),
            n: None,
            e: None,
            crv: None,
            x: None,
            y: None,
        };
        match &self.signer {
            Signer::Rs256(k) => {
                let c: RsaPublicKeyComponents<Vec<u8>> = k.public().into();
                Jwk { n: Some(B64.encode(&c.n)), e: Some(B64.encode(&c.e)), ..base("RSA") }
            }
            Signer::Es256(k) => {
                // An uncompressed point: 0x04, then x and y, 32 bytes each.
                let point = k.public_key().as_ref();
                Jwk {
                    crv: Some("P-256".into()),
                    x: Some(B64.encode(&point[1..33])),
                    y: Some(B64.encode(&point[33..65])),
                    ..base("EC")
                }
            }
        }
    }

    /// Signs `claims` as a compact JWS with this key's id and the given `typ` (`JWT`,
    /// `at+jwt`, `logout+jwt`).
    pub fn sign<C: Serialize>(&self, typ: &str, claims: &C) -> Result<String, JoseError> {
        let header = Header { alg: self.alg(), kid: self.kid.clone(), typ: Some(typ.to_owned()) };
        let header = serde_json::to_vec(&header).map_err(|_| JoseError::Malformed)?;
        let payload = serde_json::to_vec(claims).map_err(|_| JoseError::Malformed)?;
        let input = format!("{}.{}", B64.encode(header), B64.encode(payload));
        let rng = SystemRandom::new();
        let sig = match &self.signer {
            Signer::Rs256(k) => {
                let mut sig = vec![0; k.public().modulus_len()];
                k.sign(&RSA_PKCS1_SHA256, &rng, input.as_bytes(), &mut sig)
                    .map_err(|_| JoseError::BadKey("RSA signing failed".into()))?;
                sig
            }
            Signer::Es256(k) => k
                .sign(&rng, input.as_bytes())
                .map_err(|_| JoseError::BadKey("ECDSA signing failed".into()))?
                .as_ref()
                .to_vec(),
        };
        Ok(format!("{input}.{}", B64.encode(sig)))
    }
}

/// A verified token: its header and its claims, still to be checked by the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    pub header: Header,
    pub claims: Value,
}

enum PublicKey {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    Ec { point: Vec<u8> },
}

/// The public keys of a JWKS, by key id, ready to verify.
#[derive(Default)]
pub struct Verifier {
    keys: HashMap<String, (Alg, PublicKey)>,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier").field("kids", &self.keys.keys().collect::<Vec<_>>()).finish()
    }
}

fn b64(s: &Option<String>) -> Option<Vec<u8>> {
    s.as_deref().and_then(|s| B64.decode(s).ok())
}

impl Verifier {
    /// Takes the RSA and P-256 keys of a set; others are skipped.
    pub fn from_jwks(set: &JwkSet) -> Self {
        let mut keys = HashMap::new();
        for jwk in &set.keys {
            if jwk.use_.as_deref().is_some_and(|u| u != "sig") {
                continue;
            }
            let key = match jwk.kty.as_str() {
                "RSA" if jwk.alg.as_deref().is_none_or(|a| a == "RS256") => match (b64(&jwk.n), b64(&jwk.e)) {
                    (Some(n), Some(e)) => Some((Alg::RS256, PublicKey::Rsa { n, e })),
                    _ => None,
                },
                "EC" if jwk.crv.as_deref() == Some("P-256") && jwk.alg.as_deref().is_none_or(|a| a == "ES256") => {
                    match (b64(&jwk.x), b64(&jwk.y)) {
                        (Some(x), Some(y)) if x.len() == 32 && y.len() == 32 => {
                            let mut point = Vec::with_capacity(65);
                            point.push(4);
                            point.extend_from_slice(&x);
                            point.extend_from_slice(&y);
                            Some((Alg::ES256, PublicKey::Ec { point }))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(key) = key {
                keys.insert(jwk.kid.clone(), key);
            }
        }
        Verifier { keys }
    }

    pub fn has_kid(&self, kid: &str) -> bool {
        self.keys.contains_key(kid)
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Checks the signature of a compact JWS made with one of `allowed`.
    pub fn verify(&self, token: &str, allowed: &[Alg]) -> Result<Verified, JoseError> {
        let header = decode_header(token)?;
        if !allowed.contains(&header.alg) {
            return Err(JoseError::BadAlg);
        }
        let (alg, key) = self.keys.get(&header.kid).ok_or_else(|| JoseError::UnknownKid(header.kid.clone()))?;
        if *alg != header.alg {
            return Err(JoseError::BadAlg);
        }
        let (input, sig) = token.rsplit_once('.').ok_or(JoseError::Malformed)?;
        let sig = B64.decode(sig).map_err(|_| JoseError::Malformed)?;
        let ok = match key {
            PublicKey::Rsa { n, e } => RsaPublicKeyComponents { n, e }
                .verify(&signature::RSA_PKCS1_2048_8192_SHA256, input.as_bytes(), &sig)
                .is_ok(),
            PublicKey::Ec { point } => {
                UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, point).verify(input.as_bytes(), &sig).is_ok()
            }
        };
        if !ok {
            return Err(JoseError::BadSignature);
        }
        let payload = input.split_once('.').map(|(_, p)| p).ok_or(JoseError::Malformed)?;
        let claims: Value = serde_json::from_slice(&B64.decode(payload).map_err(|_| JoseError::Malformed)?)
            .map_err(|_| JoseError::Malformed)?;
        if !claims.is_object() {
            return Err(JoseError::Malformed);
        }
        Ok(Verified { header, claims })
    }
}

/// The header of a token, unverified: to find the key.
pub fn decode_header(token: &str) -> Result<Header, JoseError> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(JoseError::TooLong);
    }
    let mut parts = token.split('.');
    let (Some(h), Some(_), Some(_), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(JoseError::Malformed);
    };
    let raw: Value =
        serde_json::from_slice(&B64.decode(h).map_err(|_| JoseError::Malformed)?).map_err(|_| JoseError::Malformed)?;
    let obj = raw.as_object().ok_or(JoseError::Malformed)?;
    // A critical extension we do not know must make the token fail (RFC 7515, 4.1.11).
    if obj.contains_key("crit") {
        return Err(JoseError::BadAlg);
    }
    match obj.get("alg").and_then(Value::as_str) {
        Some("RS256" | "ES256") => {}
        _ => return Err(JoseError::BadAlg),
    }
    serde_json::from_value(raw).map_err(|_| JoseError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn es256(kid: &str) -> SigningKey {
        SigningKey::generate_es256(kid).unwrap().0
    }

    fn verifier(keys: &[&SigningKey]) -> Verifier {
        Verifier::from_jwks(&JwkSet { keys: keys.iter().map(|k| k.public_jwk()).collect() })
    }

    #[test]
    fn es256_round_trip() {
        let key = es256("k1");
        let token = key.sign("at+jwt", &serde_json::json!({ "sub": "usr_1", "aud": "pacod-gw" })).unwrap();
        let v = verifier(&[&key]).verify(&token, &[Alg::ES256]).unwrap();
        assert_eq!(v.header, Header { alg: Alg::ES256, kid: "k1".into(), typ: Some("at+jwt".into()) });
        assert_eq!(v.claims["sub"], "usr_1");
    }

    #[test]
    fn the_jwk_round_trips_through_json() {
        let key = es256("k1");
        let set = JwkSet { keys: vec![key.public_jwk()] };
        let json = serde_json::to_string(&set).unwrap();
        assert!(json.contains(r#""kty":"EC""#) && json.contains(r#""use":"sig""#) && json.contains(r#""crv":"P-256""#));
        let back: JwkSet = serde_json::from_str(&json).unwrap();
        let token = key.sign("JWT", &serde_json::json!({})).unwrap();
        Verifier::from_jwks(&back).verify(&token, &[Alg::ES256]).unwrap();
    }

    #[test]
    fn tampering_unknown_keys_and_algorithms_fail() {
        let key = es256("k1");
        let other = es256("k2");
        let token = key.sign("JWT", &serde_json::json!({ "sub": "a" })).unwrap();
        let v = verifier(&[&key]);
        // Another payload under the same signature.
        let (h, rest) = token.split_once('.').unwrap();
        let (_, s) = rest.split_once('.').unwrap();
        let forged = format!("{h}.{}.{s}", B64.encode(br#"{"sub":"b"}"#));
        assert_eq!(v.verify(&forged, &[Alg::ES256]), Err(JoseError::BadSignature));
        // The algorithm is not accepted by the caller.
        assert_eq!(v.verify(&token, &[Alg::RS256]), Err(JoseError::BadAlg));
        // Signed by a key the set does not have.
        let t2 = other.sign("JWT", &serde_json::json!({})).unwrap();
        assert_eq!(v.verify(&t2, &[Alg::ES256]), Err(JoseError::UnknownKid("k2".into())));
        // A different key under the same kid.
        let impostor = es256("k1");
        let t3 = impostor.sign("JWT", &serde_json::json!({})).unwrap();
        assert_eq!(v.verify(&t3, &[Alg::ES256]), Err(JoseError::BadSignature));
    }

    #[test]
    fn none_crit_and_garbage_are_refused() {
        let none = format!("{}.{}.", B64.encode(br#"{"alg":"none","kid":"k1"}"#), B64.encode(b"{}"));
        assert_eq!(decode_header(&none), Err(JoseError::BadAlg));
        let crit = format!("{}.{}.x", B64.encode(br#"{"alg":"ES256","kid":"k1","crit":["exp"]}"#), B64.encode(b"{}"));
        assert_eq!(decode_header(&crit), Err(JoseError::BadAlg));
        let hs = format!("{}.{}.x", B64.encode(br#"{"alg":"HS256","kid":"k1"}"#), B64.encode(b"{}"));
        assert_eq!(decode_header(&hs), Err(JoseError::BadAlg));
        for bad in ["", "a.b", "a.b.c.d", "!!!.x.y"] {
            assert!(decode_header(bad).is_err(), "{bad}");
        }
        assert_eq!(decode_header(&"a".repeat(MAX_TOKEN_LEN + 1)), Err(JoseError::TooLong));
    }

    #[test]
    fn a_header_without_kid_is_malformed() {
        let t = format!("{}.{}.x", B64.encode(br#"{"alg":"ES256"}"#), B64.encode(b"{}"));
        assert_eq!(decode_header(&t), Err(JoseError::Malformed));
    }

    #[test]
    fn keys_for_other_uses_are_skipped() {
        let key = es256("k1");
        let mut jwk = key.public_jwk();
        jwk.use_ = Some("enc".into());
        assert!(Verifier::from_jwks(&JwkSet { keys: vec![jwk] }).is_empty());
    }

    fn rsa_der(bits: usize) -> Vec<u8> {
        use rsa::pkcs8::EncodePrivateKey;
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, bits).unwrap();
        key.to_pkcs8_der().unwrap().as_bytes().to_vec()
    }

    #[test]
    fn rs256_round_trip_and_jwk() {
        let key = SigningKey::from_pkcs8("r1", Alg::RS256, &rsa_der(2048)).unwrap();
        let jwk = key.public_jwk();
        assert_eq!((jwk.kty.as_str(), jwk.alg.as_deref(), jwk.e.as_deref()), ("RSA", Some("RS256"), Some("AQAB")));
        let token = key.sign("JWT", &serde_json::json!({ "iss": "https://id.example" })).unwrap();
        let v = verifier(&[&key]);
        assert_eq!(v.verify(&token, &[Alg::RS256, Alg::ES256]).unwrap().claims["iss"], "https://id.example");
        assert_eq!(v.verify(&token, &[Alg::ES256]), Err(JoseError::BadAlg));
    }

    #[test]
    fn short_rsa_keys_are_refused() {
        assert!(matches!(SigningKey::from_pkcs8("r1", Alg::RS256, &rsa_der(1024)), Err(JoseError::BadKey(_))));
    }

    #[test]
    fn a_kid_cannot_switch_algorithm() {
        // An ES256 key published under a kid, and a token claiming RS256 for that kid.
        let ec = es256("k1");
        let rs = SigningKey::from_pkcs8("k1", Alg::RS256, &rsa_der(2048)).unwrap();
        let token = rs.sign("JWT", &serde_json::json!({})).unwrap();
        assert_eq!(verifier(&[&ec]).verify(&token, &[Alg::RS256, Alg::ES256]), Err(JoseError::BadAlg));
    }
}
