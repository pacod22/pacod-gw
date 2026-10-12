//! Runs a file of test vectors (`policy/engine-vectors/README.md`): the engine's own, or a
//! product's (`authz/vectors/*.yaml` next to its manifest).

use serde::Deserialize;
use serde_json::Value;

use crate::{Catalog, HeldBinding, Manifest, ScopeRef, Subject};

fn de<T: for<'de> Deserialize<'de>>(v: &Value, what: &str) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("{what}: {e}"))
}

fn text<'a>(v: &'a Value, what: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("{what} must be a string"))
}

/// Runs every check of a vector file. `load` reads a manifest by the name the file lists under
/// `manifests`. Returns the number of checks, or every failure with its case.
pub fn run(file: &str, load: impl Fn(&str) -> Result<Manifest, String>) -> Result<usize, Vec<String>> {
    let doc: Value = crate::read_yaml(file).map_err(|e| vec![format!("not YAML: {e}")])?;
    let mut catalog = Catalog::new();
    for name in doc["manifests"].as_array().ok_or_else(|| vec!["`manifests` is missing".to_owned()])? {
        let name = name.as_str().ok_or_else(|| vec!["manifest names are strings".to_owned()])?;
        catalog.insert(load(name).map_err(|e| vec![format!("manifest {name}: {e}")])?);
    }
    let cases = doc["cases"].as_array().ok_or_else(|| vec!["`cases` is missing".to_owned()])?;
    let (mut count, mut failures) = (0, Vec::new());
    for case in cases {
        let name = case["name"].as_str().unwrap_or("(unnamed)");
        let who = if case.get("subject").is_some() { &case["subject"] } else { &case["actor"] };
        let subject: Subject = match de(who, "subject") {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        for check in case["checks"].as_array().into_iter().flatten() {
            count += 1;
            match one(&catalog, &subject, check) {
                Ok(None) => {}
                Ok(Some(got)) => failures.push(format!("{name}: {check:?} gave {got}")),
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
    }
    if failures.is_empty() { Ok(count) } else { Err(failures) }
}

/// One check: `Ok(None)` when it holds, `Ok(Some(answer))` when the engine answered otherwise.
fn one(catalog: &Catalog, subject: &Subject, check: &Value) -> Result<Option<String>, String> {
    let expect = &check["expect"];
    let boolean = |got: bool| -> Result<Option<String>, String> {
        let want = expect.as_bool().ok_or("expect must be true or false")?;
        Ok((got != want).then(|| got.to_string()))
    };
    let path = |key: &str| de::<Vec<ScopeRef>>(&check[key], key);
    if let Some(p) = check.get("allowed") {
        return boolean(catalog.allowed(subject, text(p, "allowed")?, &path("at")?));
    }
    if let Some(r) = check.get("has_role") {
        let scope: ScopeRef = de(&check["scope"], "scope")?;
        return boolean(catalog.has_role(subject, text(r, "has_role")?, &scope));
    }
    if let Some(s) = check.get("holds_any_at") {
        return boolean(catalog.holds_any_at(subject, &de(s, "holds_any_at")?));
    }
    if let Some(p) = check.get("allowed_somewhere") {
        return boolean(catalog.allowed_somewhere(subject, text(p, "allowed_somewhere")?));
    }
    if check.get("touches").is_some() {
        return boolean(catalog.touches(subject, &path("touches")?));
    }
    if let Some(r) = check.get("can_grant") {
        let got = match catalog.can_grant(subject, text(r, "can_grant")?, &path("at")?) {
            Ok(()) => "ok",
            Err(e) => e.code(),
        };
        let want = text(expect, "expect")?;
        return Ok((got != want).then(|| got.to_owned()));
    }
    if let Some(p) = check.get("check_protected") {
        let before: Vec<HeldBinding> = de(&p["before"], "before")?;
        let after: Vec<HeldBinding> = de(&p["after"], "after")?;
        let got = catalog.check_protected(subject, &before, &after);
        let holds = match (&got, expect.as_str()) {
            (Ok(()), Some("ok")) => true,
            (Err(v), None) => {
                let want = &expect["violation"];
                v.role == text(&want["role"], "violation.role")?
                    && v.scope.to_string() == text(&want["scope"], "violation.scope")?
            }
            _ => false,
        };
        return Ok((!holds).then(|| format!("{got:?}")));
    }
    Err(format!("unknown check {check:?}"))
}
