//! The jobs that work on the stored data, run against a real ClickHouse.

use chrono::{DateTime, Utc};
use pacod_gw_analytics::jobs::retention;
use pacod_gw_events::UsageEvent;
use serde_json::json;

use crate::support::{TestApp, id};

fn event(n: u128, workspace: &str, at: &str) -> UsageEvent {
    let mut e = UsageEvent::new(id(n), "gw-1");
    e.timestamp = at.parse().unwrap();
    e.organisation_id = "org-a".into();
    e.workspace_id = workspace.into();
    e.status = "ok".into();
    e.usage_source = "provider".into();
    e
}

#[tokio::test]
async fn retention_deletes_what_each_workspace_no_longer_keeps() {
    let app = TestApp::start().await;
    let old = "2026-08-01T12:00:00Z";
    let recent = "2026-10-08T12:00:00Z";
    app.store_usage(
        "retention",
        &[
            event(1, "ws-short", old),
            event(2, "ws-short", recent),
            event(3, "ws-keeps", old),
            // A workspace that no longer exists falls under the installation's retention.
            event(4, "ws-gone", old),
            event(5, "ws-gone", recent),
        ],
    )
    .await;
    let now: DateTime<Utc> = "2026-10-10T09:00:00Z".parse().unwrap();
    let work = json!({
        "global_days": 30,
        "workspaces": [{ "workspace_id": "ws-short", "days": 7 }, { "workspace_id": "ws-keeps", "days": null }]
    });
    let report = retention::run(&app.clickhouse, &work, now).await.expect("run");
    assert_eq!(report["deletions"][0], json!({ "days": 7, "workspaces": 1, "deleted": 1 }), "{report}");
    assert_eq!(report["deletions"][1]["deleted"], 1, "{report}");
    let mut left: Vec<u128> = app.request_numbers("from=2026-01-01T00:00:00Z&to=2026-12-31T00:00:00Z").await;
    left.sort_unstable();
    assert_eq!(left, [2, 3, 5]);
    // Again: nothing more to delete.
    let again = retention::run(&app.clickhouse, &work, now).await.expect("run");
    assert_eq!(again["deletions"][0]["deleted"], 0);
}

/// What every report the rollups can answer says about the fixture.
async fn rollup_reports(app: &TestApp) -> Vec<serde_json::Value> {
    let range = "from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-a";
    let mut out = vec![
        app.json("/reports/overview?to=2026-03-12T10:30:00Z&organisation_id=org-a").await,
        app.json("/reports/overview?to=2026-03-31T00:00:00Z").await,
        app.json(&format!("/reports/end-users?{range}")).await,
        app.json("/reports/end-users?from=2026-02-01T00:00:00Z&to=2026-03-13T00:00:00Z").await,
    ];
    for by in ["model", "provider", "template", "key_id", "workspace_id", "organisation_id"] {
        out.push(app.json(&format!("/reports/breakdown?{range}&by={by}")).await);
    }
    out.push(app.json(&format!("/reports/breakdown?{range}&by=model&status=ok&provider=anthropic")).await);
    out
}

/// Equal, numbers within a relative 1e-9: costs added up in another order differ in the last bits.
fn assert_close(a: &serde_json::Value, b: &serde_json::Value, path: &str) {
    use serde_json::Value::*;
    match (a, b) {
        (Number(x), Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            assert!((x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0), "{path}: {x} != {y}");
        }
        (Array(x), Array(y)) => {
            assert_eq!(x.len(), y.len(), "{path}: {a} != {b}");
            for (i, (x, y)) in x.iter().zip(y).enumerate() {
                assert_close(x, y, &format!("{path}[{i}]"));
            }
        }
        (Object(x), Object(y)) => {
            assert_eq!(x.keys().collect::<Vec<_>>(), y.keys().collect::<Vec<_>>(), "{path}");
            for (k, v) in x {
                assert_close(v, &y[k], &format!("{path}.{k}"));
            }
        }
        _ => assert_eq!(a, b, "{path}"),
    }
}

/// The reports as the events alone answer them: with no rollup bucket known as built.
async fn truth(app: &TestApp) -> Vec<serde_json::Value> {
    let built = app.select("SELECT granularity, bucket, generation FROM rollup_built FINAL").await;
    app.command("TRUNCATE TABLE rollup_built").await;
    let answers = rollup_reports(app).await;
    // Put the registry back as it was.
    for row in built {
        app.command(&format!(
            "INSERT INTO rollup_built VALUES ('{}', '{}', {})",
            row["granularity"].as_str().unwrap(),
            row["bucket"].as_str().unwrap().trim_end_matches('Z').replace('T', " "),
            row["generation"]
        ))
        .await;
    }
    answers
}

#[tokio::test]
async fn rollups_answer_exactly_as_the_events_do() {
    use crate::support::{app_with_fixture, fixture};
    use pacod_gw_analytics::jobs::rollups;

    let app = app_with_fixture().await;
    let before = rollup_reports(&app).await;
    let now = chrono::Utc::now();
    rollups::run(&app.clickhouse, now).await.expect("rollups");
    let hours = app.select("SELECT count() AS n FROM rollup_built FINAL WHERE granularity = 'hour'").await;
    assert!(hours[0]["n"].as_u64().unwrap() >= 7, "the fixture's hours were built: {hours:?}");
    let days = app.select("SELECT count() AS n FROM rollup_built FINAL WHERE granularity = 'day'").await;
    assert!(days[0]["n"].as_u64().unwrap() >= 4, "{days:?}");
    assert_close(&json!(rollup_reports(&app).await), &json!(before), "built");
    // The backfill walked past the oldest event, so later runs look at the last two days only.
    let frontier =
        app.select("SELECT toString(value) AS v FROM rollup_progress FINAL WHERE name = 'backfill_before'").await;
    let oldest = app.select("SELECT toString(toStartOfHour(min(timestamp))) AS v FROM usage_events").await;
    assert!(frontier[0]["v"].as_str().unwrap() <= oldest[0]["v"].as_str().unwrap(), "{frontier:?} {oldest:?}");

    // The reports do read the rollups: a changed rollup changes them.
    app.command("ALTER TABLE usage_hourly UPDATE cost = cost + 1000 WHERE 1 SETTINGS mutations_sync = 2").await;
    let corrupted = app.json("/reports/overview?to=2026-03-31T00:00:00Z").await;
    assert!(corrupted["periods"]["month"]["cost"].as_f64().unwrap() > 1000.0, "{corrupted}");
    app.command("TRUNCATE TABLE rollup_built").await;
    rollups::run(&app.clickhouse, now).await.expect("rebuilt");
    assert_close(&json!(rollup_reports(&app).await), &json!(before), "rebuilt");

    // A late batch for an hour that was built: read from the events until the hour is rebuilt.
    let mut late = fixture()[0].clone();
    late.id = id(900);
    late.timestamp = "2026-03-12T10:20:00Z".parse().unwrap();
    late.end_user = "carol".into();
    app.store_usage("late", &[late]).await;
    let marks = app.select("SELECT count() AS n FROM rollup_late").await;
    assert_eq!(marks[0]["n"], 1, "ingest marked the hour");
    let expected = truth(&app).await;
    assert_ne!(json!(expected), json!(before), "the late event counts");
    assert_close(&json!(rollup_reports(&app).await), &json!(expected), "late, before the rebuild");
    rollups::run(&app.clickhouse, chrono::Utc::now()).await.expect("rollups");
    assert_eq!(app.select("SELECT count() AS n FROM rollup_late").await[0]["n"], 0, "the mark is cleared");
    assert_close(&json!(rollup_reports(&app).await), &json!(expected), "late, after the rebuild");

    // A batch delivered again under another id changes nothing.
    app.store_usage("fixture-again", &fixture()).await;
    assert_close(&json!(rollup_reports(&app).await), &json!(expected), "again, before the rebuild");
    rollups::run(&app.clickhouse, chrono::Utc::now()).await.expect("rollups");
    assert_close(&json!(rollup_reports(&app).await), &json!(expected), "again, after the rebuild");
}
