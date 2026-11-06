//! pacod-gw vendors this crate (its `scripts/vendor-pacod-aggregator.sh`). A manifest that
//! inherits from this workspace (`version.workspace`, `dep.workspace = true`) does not build
//! outside it, so this one spells everything out.

#[test]
fn the_vendored_crate_inherits_nothing_from_the_workspace() {
    let text = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    for line in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
        assert!(!line.contains("workspace"), "Cargo.toml inherits from the workspace: {line}");
        assert!(!line.contains("path ="), "Cargo.toml depends on a crate by path: {line}");
    }
}
