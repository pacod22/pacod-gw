//! Runs the shared engine vectors (`policy/engine-vectors/`), the specification every port of
//! the engine must pass.

use std::path::{Path, PathBuf};

use pacod_id_authz::{Manifest, ManifestSource};
use serde_json::Value;

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy/engine-vectors")
}

fn load(name: &str) -> Result<Manifest, String> {
    let text =
        std::fs::read_to_string(vectors_dir().join(format!("manifests/{name}.yaml"))).map_err(|e| e.to_string())?;
    Manifest::from_yaml(&text).map_err(|e| e.to_string())
}

fn run(file: &str) -> usize {
    let text = std::fs::read_to_string(vectors_dir().join(file)).unwrap();
    pacod_id_authz::vectors::run(&text, load).unwrap_or_else(|failures| panic!("{file}:\n{}", failures.join("\n")))
}

#[test]
fn evaluation() {
    assert!(run("evaluation.yaml") > 50);
}

#[test]
fn grants() {
    assert!(run("grants.yaml") > 20);
}

#[test]
fn protected() {
    assert!(run("protected.yaml") >= 8);
}

#[test]
fn manifest_errors() {
    let text = std::fs::read_to_string(vectors_dir().join("manifest-errors.yaml")).unwrap();
    let file: Value =
        serde_saphyr::from_str_with_options(&text, serde_saphyr::options! { strict_booleans: true }).unwrap();
    for case in file["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let got = match serde_json::from_value::<ManifestSource>(case["manifest"].clone()) {
            Err(_) => "bad_manifest".to_owned(),
            Ok(source) => match Manifest::from_source(source) {
                Ok(_) => "ok".to_owned(),
                Err(e) => e.code().to_owned(),
            },
        };
        assert_eq!(got, case["expect"].as_str().unwrap(), "{name}");
    }
}

#[test]
fn the_runner_reports_failures() {
    let file = r#"
manifests: [acme]
cases:
  - name: wrong on purpose
    subject: { kind: user, bindings: [] }
    checks:
      - { allowed: acme.org.read, at: [platform], expect: true }
      - { can_grant: acme.viewer, at: [platform], expect: ok }
"#;
    let failures = pacod_id_authz::vectors::run(file, load).unwrap_err();
    assert_eq!(failures.len(), 2, "{failures:?}");
}

#[test]
fn the_test_manifests_round_trip_through_their_source() {
    for name in ["acme", "shop"] {
        let manifest = load(name).unwrap();
        let json = serde_json::to_string(manifest.source()).unwrap();
        let again: ManifestSource = serde_json::from_str(&json).unwrap();
        assert_eq!(Manifest::from_source(again).unwrap(), manifest);
    }
}

#[test]
fn documents_of_the_wrong_shape_are_refused() {
    let bad = [
        "null",
        "[]",
        "{ product: p, grant: { default: x.y }, extra: 1 }",
        "{ product: p, roles: { a: { permissions: [x.y] } }, grant: { default: x.y } }",
        "{ product: p, roles: { a: { at: [platform], implicit: 'yes' } }, grant: { default: x.y } }",
    ];
    for text in bad {
        let err = Manifest::from_yaml(text).expect_err(text);
        assert_eq!(err.code(), "bad_manifest", "{text}");
    }
}
