//! The report fixtures, which pacod-benchmark's tests read too (it vendors this crate): the
//! valid ones are accepted and write back what they read, the invalid ones are refused with the
//! problem named, and `schema.json` is the JSON Schema of the types as they are.
//!
//! `UPDATE_SCHEMA=1 cargo test -p pacod-gw-bench-report` writes `schema.json` again.

use std::path::PathBuf;

use pacod_gw_bench_report::{Class, MAX_BYTES, MAX_CELLS, Report, SCHEMA, parse};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1")
}

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn the_valid_fixtures_are_accepted() {
    for name in ["minimal.json", "full.json", "external-third-party.json", "unknown-fields.json"] {
        let r = parse(&bytes(name)).unwrap_or_else(|p| panic!("{name}: {p:?}"));
        assert_eq!(r.schema, SCHEMA, "{name}");
    }
    let full = parse(&bytes("full.json")).unwrap();
    assert_eq!(full.measurement.class, Class::Behaviour);
    assert_eq!(full.fact("gateway_version").as_deref(), Some("0.1.0"));
    assert!(full.cells[0].steady.as_ref().unwrap().gw_delay.is_none(), "a baseline has no gateway figure");
    // What a report says it is, it says again: written back, it reads into the same report.
    let again: Report = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
    assert_eq!(again, full);
}

#[test]
fn a_newer_minor_version_is_read_with_what_this_version_knows() {
    let r = parse(&bytes("unknown-fields.json")).unwrap();
    assert_eq!(r.minor, 3);
    assert_eq!(r.cells.len(), 3);
}

#[test]
fn the_invalid_fixtures_are_refused_with_the_problem_named() {
    for (name, path) in [
        ("bad-histogram.json", "cells[0].histograms.total"),
        ("negative-rate.json", "cells[1].steady.rps_mean"),
        ("wrong-schema.json", "schema"),
        ("bad-key.json", "cells[2].comparison_key"),
    ] {
        let problems = parse(&bytes(name)).expect_err(name);
        assert!(problems.iter().any(|p| p.path == path), "{name}: {problems:?}");
    }
}

#[test]
fn size_and_count_are_capped() {
    let mut r = parse(&bytes("full.json")).unwrap();
    let cell = r.cells[1].clone();
    r.cells = vec![cell; MAX_CELLS + 1];
    let text = serde_json::to_vec(&r).unwrap();
    let problems = parse(&text).expect_err("too many cells");
    assert!(problems.iter().any(|p| p.path == "cells" && p.message.contains("more than")));
    let mut long = bytes("full.json");
    long.extend(std::iter::repeat_n(b' ', MAX_BYTES));
    assert!(parse(&long).unwrap_err()[0].message.contains("longer than"));
    assert!(parse(b"{not json").is_err());
}

#[test]
fn the_json_schema_is_the_types_as_they_are() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(Report)).unwrap() + "\n";
    let path = dir().join("schema.json");
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let stored = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        stored == schema,
        "tests/fixtures/v1/schema.json is out of date: UPDATE_SCHEMA=1 cargo test -p pacod-gw-bench-report"
    );
}
