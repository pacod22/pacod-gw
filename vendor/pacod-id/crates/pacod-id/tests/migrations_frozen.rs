//! Applied migrations are never edited (docs/runbook.md, section 5): a database that ran one
//! keeps its checksum, and pacod-id refuses to start once the file no longer matches. Each
//! migration's SHA-384 (what sqlx records) is pinned here; a new migration adds its line.

const FROZEN: &[(&str, &str)] = &[
    (
        "0001_init.sql",
        "9b5f89ceacf3884c55cb647bb6ccde5b35867ad2262b66df193177b20dac2dcb40e212a868f0d40814f51cec5c3b76d8",
    ),
    (
        "0002_imports_realm.sql",
        "5542f1d5b76e3071c9a8bd20e3eedc6c44c1ee7145ffc116659f9a6d8865c48171c9452cc7152100aae8893aa4e86abb",
    ),
];

#[test]
fn applied_migrations_stay_as_they_were() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".sql"))
        .collect();
    found.sort();
    let pinned: Vec<&str> = FROZEN.iter().map(|(n, _)| *n).collect();
    assert_eq!(found, pinned, "a new migration adds its checksum to FROZEN; an old one is never removed");
    for (name, want) in FROZEN {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        let got = hex::encode(ring::digest::digest(&ring::digest::SHA384, &bytes));
        assert_eq!(
            &got, want,
            "{name} changed after databases applied it; restore it and put the change in a new migration"
        );
    }
}
