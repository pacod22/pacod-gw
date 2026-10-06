//! How long routing takes on the request path: choosing the models of a call, and resolving
//! the first of them, for templates routed in different ways.
//!
//! `cargo bench -p pacod-gw-core --bench route`. Budgets (medians): selection with ten typed
//! rules under 300 ns, with 64 rules under 1 µs, with one CEL rule under 5 µs; a single-model
//! template within 100 ns of naming the model directly.

#[path = "../tests/routing_fixture/mod.rs"]
mod routing_fixture;

use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use pacod_gw_core::model::Complexity;
use pacod_gw_core::resolve::{ResolveRequest, resolve, resolve_routed};
use pacod_gw_core::routing::RouteInput;

fn route(c: &mut Criterion) {
    let snap = routing_fixture::snapshot();
    let ws = &snap.workspaces["ws_1"];
    let key = snap.keys[&routing_fixture::KEY].clone();
    let mut tags = BTreeMap::new();
    tags.insert("team".to_owned(), "search".to_owned());
    let input = RouteInput {
        complexity: Some(Complexity::High),
        task: Some("code-review"),
        tags: &tags,
        input_tokens: 2_000,
        ..RouteInput::empty()
    };

    let mut g = c.benchmark_group("select");
    for name in ["single", "ten", "sixty-four", "wrr", "cel"] {
        let t = &ws.templates[name];
        let input = RouteInput { template: &t.name, ..input };
        let mut rng = fastrand::Rng::with_seed(1);
        g.bench_function(name, |b| {
            b.iter(|| black_box(t.route.action_for(black_box(&input)).select(&t.pool, &input, |_| true, 0, &mut rng)))
        });
    }
    g.finish();

    let mut g = c.benchmark_group("resolve");
    let direct = ResolveRequest { model: "x/m0", template: None, provider: None, families: None };
    g.bench_function("model", |b| b.iter(|| black_box(resolve(&snap, ws, &key, black_box(&direct)).unwrap())));
    for name in ["single", "ten", "sixty-four", "cel"] {
        let req = ResolveRequest { model: "", template: Some(name), provider: None, families: None };
        g.bench_function(name, |b| {
            b.iter(|| black_box(resolve_routed(&snap, ws, &key, black_box(&req), &input).unwrap()))
        });
    }
    g.finish();
}

criterion_group!(benches, route);
criterion_main!(benches);
