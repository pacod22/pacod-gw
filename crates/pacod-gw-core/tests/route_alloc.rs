//! Routing on the request path allocates nothing: choosing the models of a call is a few
//! comparisons over what the snapshot precomputed.

#[path = "routing_fixture/mod.rs"]
mod routing_fixture;

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use pacod_gw_core::model::Complexity;
use pacod_gw_core::resolve::{ResolveRequest, resolve_routed};
use pacod_gw_core::routing::RouteInput;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations(f: impl Fn()) -> usize {
    f(); // warm up: thread-locals, lazily built state
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..100 {
        f();
    }
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

#[test]
fn choosing_and_resolving_the_models_of_a_call_does_not_allocate() {
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
    for name in ["single", "ten", "sixty-four", "wrr"] {
        let t = &ws.templates[name];
        let input = RouteInput { template: &t.name, ..input };
        let n = allocations(|| {
            let c = t.route.action_for(&input).select(&t.pool, &input, |_| true, 0, &mut fastrand::Rng::with_seed(1));
            assert!(!c.is_empty());
        });
        assert_eq!(n, 0, "selection for {name}");

        let req = ResolveRequest { model: "", template: Some(name), provider: None, families: None };
        let n = allocations(|| {
            let (target, choice) = resolve_routed(&snap, ws, &key, &req, &input).unwrap();
            assert!(choice.is_some() && target.pool_index.is_some());
        });
        assert_eq!(n, 0, "resolution for {name}");
    }
}
