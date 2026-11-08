//! The contract fixtures, which the tester's tests read too (`tester/tests/test_load_contract.py`
//! and `test_load_histogram.py`): every one reads into its type and writes back the same JSON,
//! and the histogram vectors hold.

use std::path::PathBuf;

use pacod_gw_load::contract::{ErrorBody, FEATURES, Info, Prepare, Prepared, Report, Success};
use pacod_gw_load::histogram::{Histogram, bounds, index};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/contract").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn round_trip<T: Serialize + DeserializeOwned>(name: &str) -> T {
    let original = fixture(name);
    let typed: T = serde_json::from_value(original.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), original, "{name} does not write back the same");
    typed
}

#[test]
fn every_fixture_reads_and_writes_back_unchanged() {
    let info: Info = round_trip("info.json");
    assert_eq!(info.features, FEATURES, "the fixture lists what this agent can do");
    let p: Prepare = round_trip("prepare.json");
    assert!(p.base_url.is_none());
    assert_eq!(p.request.success, Success::Http, "a plan that does not say is plain HTTP");
    assert!(!format!("{p:?}").contains("secret"), "the key stays out of Debug");
    let ext: Prepare = round_trip("prepare-external.json");
    assert_eq!(ext.base_url.as_deref(), Some("http://host.docker.internal:18280"));
    let rpc: Prepare = round_trip("prepare-jsonrpc.json");
    assert_eq!(rpc.request.success, Success::JsonRpc);
    round_trip::<Prepared>("prepared.json");
    let r: Report = round_trip("report.json");
    assert_eq!(r.intervals[0].status.get(&503), Some(&1));
    assert_eq!(r.intervals[0].errors.rpc, 0, "a report without the field has none");
    let r: Report = round_trip("report-jsonrpc.json");
    let iv = &r.intervals[0];
    assert_eq!((iv.errors.rpc, iv.errors.failed()), (3, 4));
    // Every call counted once: a success, another status, or an error.
    let other_status: u64 = iv.status.iter().filter(|(s, _)| !(200..300).contains(*s)).map(|(_, n)| n).sum();
    assert_eq!(iv.ok + other_status + iv.errors.failed(), iv.completed);
    round_trip::<ErrorBody>("error.json");
}

/// What an agent of the first version wrote still reads, and means the same.
#[test]
fn what_an_older_agent_writes_still_reads() {
    let mut info = fixture("info.json");
    info.as_object_mut().unwrap().remove("features");
    let info: Info = serde_json::from_value(info).unwrap();
    assert!(info.features.is_empty());
    assert!(!serde_json::to_string(&info).unwrap().contains("features"), "left out when empty");
}

#[test]
fn the_histogram_vectors_hold() {
    let v = fixture("histogram.json");
    for pair in v["index"].as_array().unwrap() {
        assert_eq!(u64::from(index(pair[0].as_u64().unwrap())), pair[1].as_u64().unwrap(), "{pair}");
    }
    for row in v["bounds"].as_array().unwrap() {
        let b = bounds(row[0].as_u64().unwrap() as u32);
        assert_eq!(b, (row[1].as_u64().unwrap(), row[2].as_u64().unwrap()), "{row}");
    }
    let h: Histogram = serde_json::from_value(v["histogram"].clone()).unwrap();
    for pair in v["percentiles"].as_array().unwrap() {
        assert_eq!(h.percentile(pair[0].as_f64().unwrap()), pair[1].as_f64().unwrap(), "{pair}");
    }
}
