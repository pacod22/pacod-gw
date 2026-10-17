//! Schema migrations.
//!
//! The files in `migrations/` are compiled into the binary and applied in order at start-up.
//! Each applied file is recorded in the table `schema_migrations`.
//!
//! Rules for a migration file:
//!
//! * The name is `<number>_<name>.sql`; the number is the version and versions only grow.
//! * Statements end with `;`. ClickHouse's HTTP interface takes one statement per request, so
//!   the file is split there: no `;` inside a string literal or a trailing comment.
//! * Statements are idempotent (`IF NOT EXISTS`). Two instances that start together may both
//!   run a file, and a file that failed half-way is run again from its start.
//! * A file that has been released is never edited. Changes go into a new file.

use serde::Deserialize;

use super::clickhouse::{Binds, ClickHouse, ClickHouseError};

pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every file in `migrations/`, in order. A unit test fails when a file is missing here.
pub const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, name: "0001_usage_events", sql: include_str!("../../migrations/0001_usage_events.sql") },
    Migration { version: 2, name: "0002_message_audit", sql: include_str!("../../migrations/0002_message_audit.sql") },
    Migration { version: 3, name: "0003_attribution", sql: include_str!("../../migrations/0003_attribution.sql") },
    Migration { version: 4, name: "0004_routing", sql: include_str!("../../migrations/0004_routing.sql") },
    Migration {
        version: 5,
        name: "0005_model_reported",
        sql: include_str!("../../migrations/0005_model_reported.sql"),
    },
    Migration {
        version: 6,
        name: "0006_cost_reporting",
        sql: include_str!("../../migrations/0006_cost_reporting.sql"),
    },
    Migration {
        version: 7,
        name: "0007_audit_attachments",
        sql: include_str!("../../migrations/0007_audit_attachments.sql"),
    },
    Migration { version: 8, name: "0008_rollups", sql: include_str!("../../migrations/0008_rollups.sql") },
    Migration {
        version: 9,
        name: "0009_rollup_progress",
        sql: include_str!("../../migrations/0009_rollup_progress.sql"),
    },
    Migration { version: 10, name: "0010_plugins", sql: include_str!("../../migrations/0010_plugins.sql") },
];

const CREATE_LEDGER: &str = "CREATE TABLE IF NOT EXISTS schema_migrations
(
    version    UInt32,
    name       String,
    applied_at DateTime('UTC') DEFAULT now()
)
ENGINE = ReplacingMergeTree
ORDER BY version";

/// The version the schema has once every migration is applied.
pub fn latest_version() -> u32 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

#[derive(Deserialize)]
struct Version {
    version: u32,
}

/// The highest version recorded in the database. Fails when the ledger does not exist.
pub async fn applied_version(clickhouse: &ClickHouse) -> Result<u32, ClickHouseError> {
    let row: Version = clickhouse.probe("SELECT max(version) AS version FROM schema_migrations").await?;
    Ok(row.version)
}

/// Creates the database and the ledger when missing and applies the migrations that the ledger
/// does not list. Returns how many were applied.
pub async fn apply(clickhouse: &ClickHouse) -> Result<usize, ClickHouseError> {
    clickhouse.create_database().await?;
    clickhouse.command(CREATE_LEDGER, &Binds::new(), &[]).await?;

    let applied: Vec<Version> =
        clickhouse.fetch("SELECT DISTINCT version FROM schema_migrations", &Binds::new()).await?;

    let mut count = 0;
    for migration in MIGRATIONS {
        if applied.iter().any(|row| row.version == migration.version) {
            continue;
        }
        for statement in statements(migration.sql) {
            clickhouse.command(&statement, &Binds::new(), &[]).await?;
        }
        let mut binds = Binds::new();
        let record = format!(
            "INSERT INTO schema_migrations (version, name) SELECT {}, {}",
            binds.uint64(u64::from(migration.version)),
            binds.string(migration.name),
        );
        clickhouse.command(&record, &binds, &[]).await?;
        tracing::info!(version = migration.version, name = migration.name, "migration applied");
        count += 1;
    }
    Ok(count)
}

/// Splits a migration file into statements, without its comment lines.
fn statements(sql: &str) -> Vec<String> {
    let without_comments: String =
        sql.lines().filter(|line| !line.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n");
    without_comments.split(';').map(str::trim).filter(|statement| !statement.is_empty()).map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_in_the_directory_is_listed_in_order() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut on_disk: Vec<String> = std::fs::read_dir(&directory)
            .expect("migrations directory")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
            .map(|path| path.file_stem().expect("file stem").to_string_lossy().into_owned())
            .collect();
        on_disk.sort();

        let listed: Vec<&str> = MIGRATIONS.iter().map(|migration| migration.name).collect();
        assert_eq!(listed, on_disk, "MIGRATIONS must list migrations/*.sql");
    }

    #[test]
    fn versions_match_the_file_names_and_grow() {
        let mut previous = 0;
        for migration in MIGRATIONS {
            let prefix = migration.name.split('_').next().expect("prefix");
            assert_eq!(prefix.parse::<u32>().expect("number"), migration.version);
            assert!(migration.version > previous, "{}", migration.name);
            previous = migration.version;
        }
        assert_eq!(latest_version(), previous);
    }

    #[test]
    fn every_statement_is_an_idempotent_definition() {
        for migration in MIGRATIONS {
            let statements = statements(migration.sql);
            assert!(!statements.is_empty(), "{}", migration.name);
            for statement in statements {
                assert!(
                    statement.starts_with("CREATE TABLE IF NOT EXISTS") || statement.starts_with("ALTER TABLE"),
                    "{}: unexpected statement start: {}",
                    migration.name,
                    statement.lines().next().unwrap_or_default()
                );
            }
        }
    }

    #[test]
    fn files_are_split_at_semicolons_and_comment_lines_are_dropped() {
        let sql = "-- first\nCREATE TABLE a (x UInt8) ENGINE = Memory;\n\n  -- second\nCREATE TABLE b\n(\n    -- inside\n    y UInt8\n)\nENGINE = Memory;\n";
        assert_eq!(
            statements(sql),
            vec![
                "CREATE TABLE a (x UInt8) ENGINE = Memory".to_owned(),
                "CREATE TABLE b\n(\n    y UInt8\n)\nENGINE = Memory".to_owned(),
            ]
        );
    }
}
