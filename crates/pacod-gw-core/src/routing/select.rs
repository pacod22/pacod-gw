//! Binding a scheme to a template's pool, and choosing the models for one call.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use http::HeaderValue;

use super::compile::CompiledScheme;
use super::live::TargetState;
use super::{Candidates, RouteInput, stable_hash};
use crate::model::{Affinity, Algorithm, FailoverPolicy, Overflow, PoolEntry, RouteAction, RouteSource};
use crate::snapshot::ModelRt;

/// Longest smooth weighted round-robin sequence; larger weights are scaled down to fit.
const MAX_WRR: u64 = 1024;
/// The share of calls, in percent, that `lowest_latency` sends elsewhere to keep measuring.
const EXPLORE_PERCENT: u32 = 5;

/// One model of a template's pool, ready to route to.
#[derive(Debug)]
pub struct PoolMember {
    /// The entry, its model normalised to the model's object id.
    pub entry: PoolEntry,
    /// The model the entry names (before any successor replaces it).
    pub model: Arc<ModelRt>,
    /// What this instance knows about the model at the workspace's credential.
    pub state: Arc<TargetState>,
    /// For session affinity.
    pub id_hash: u64,
    /// Input plus output price per million tokens when the snapshot was built. Unset: the
    /// model has no price.
    pub price: Option<f64>,
}

impl PoolMember {
    fn weight(&self) -> u64 {
        u64::from(self.entry.weight)
    }
}

/// Candidates of one tier, by priority.
#[derive(Debug)]
struct Group {
    members: Candidates,
    /// Smooth weighted round-robin order of the members, as pool positions.
    wrr: Box<[u8]>,
}

/// A rule's action bound to one pool.
#[derive(Debug)]
pub struct BoundAction {
    /// The rule's name. Unset for the default action.
    pub rule: Option<Arc<str>>,
    /// False when none of the tiers the rule names are in this pool: the rule is skipped.
    pub applicable: bool,
    groups: Box<[Group]>,
    /// The rest of the pool, for `overflow: rest_of_pool`.
    rest: Candidates,
    pub algorithm: Algorithm,
    pub affinity: Affinity,
    pub failover: FailoverPolicy,
    /// The value of `x-gw-route`: `scheme/rule`. Unset without a scheme.
    pub header: Option<HeaderValue>,
    turn: AtomicU64,
}

/// The routing of one template: its scheme bound to its pool.
#[derive(Debug)]
pub struct BoundRoute {
    pub scheme: Option<Arc<CompiledScheme>>,
    pub source: RouteSource,
    /// One per rule of the scheme, in the same order.
    pub actions: Box<[BoundAction]>,
    pub default: BoundAction,
}

/// Positions of the pool by priority, then by position.
fn by_priority(pool: &[PoolMember], filter: impl Fn(usize) -> bool) -> Candidates {
    let mut v: Vec<usize> = (0..pool.len()).filter(|&i| filter(i)).collect();
    v.sort_by_key(|&i| (pool[i].entry.priority, i));
    v.into_iter().collect()
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The smooth weighted round-robin sequence (as nginx spreads upstreams): each member as
/// often as its weight, evenly interleaved.
fn wrr_sequence(pool: &[PoolMember], members: &Candidates) -> Box<[u8]> {
    let mut weights: Vec<(usize, u64)> =
        members.iter().map(|i| (i, pool[i].weight())).filter(|&(_, w)| w > 0).collect();
    let g = weights.iter().fold(0, |g, &(_, w)| gcd(g, w));
    if g > 1 {
        weights.iter_mut().for_each(|(_, w)| *w /= g);
    }
    let total: u64 = weights.iter().map(|&(_, w)| w).sum();
    if total > MAX_WRR {
        weights.iter_mut().for_each(|(_, w)| *w = (*w * MAX_WRR / total).max(1));
    }
    let total: i64 = weights.iter().map(|&(_, w)| w as i64).sum();
    let mut current = vec![0i64; weights.len()];
    let mut seq = Vec::with_capacity(total as usize);
    for _ in 0..total {
        let mut best = 0;
        for (k, &(_, w)) in weights.iter().enumerate() {
            current[k] += w as i64;
            if current[k] > current[best] {
                best = k;
            }
        }
        current[best] -= total;
        seq.push(weights[best].0 as u8);
    }
    seq.into()
}

fn bind_action(
    pool: &[PoolMember],
    action: &RouteAction,
    failover: FailoverPolicy,
    rule: Option<Arc<str>>,
    header: Option<HeaderValue>,
) -> BoundAction {
    let mut groups: Vec<Group> = Vec::new();
    if action.tiers.is_empty() {
        groups.push(Group { members: by_priority(pool, |_| true), wrr: Box::default() });
    } else {
        for t in &action.tiers {
            let members = by_priority(pool, |i| pool[i].entry.tier.as_deref() == Some(t.as_str()));
            if !members.is_empty() {
                groups.push(Group { members, wrr: Box::default() });
            }
        }
    }
    for g in &mut groups {
        g.wrr = wrr_sequence(pool, &g.members);
    }
    let rest = match action.overflow {
        Overflow::None => Candidates::default(),
        Overflow::RestOfPool => by_priority(pool, |i| !groups.iter().any(|g| g.members.contains(i))),
    };
    BoundAction {
        rule,
        applicable: !groups.is_empty(),
        groups: groups.into(),
        rest,
        algorithm: action.algorithm,
        affinity: action.affinity,
        failover,
        header,
        turn: AtomicU64::new(fastrand::u64(..)),
    }
}

fn header(scheme: &str, rule: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!("{scheme}/{rule}")).ok()
}

/// Binds a scheme (or, without one, the built-in routing: the whole pool by priority) to a
/// template's pool. Problems worth showing go to `problems`.
pub fn bind(
    scheme: Option<(Arc<CompiledScheme>, RouteSource)>,
    pool: &[PoolMember],
    template: &str,
    problems: &mut Vec<String>,
) -> BoundRoute {
    let Some((scheme, source)) = scheme else {
        let failover = FailoverPolicy { max_targets: pool.len().clamp(1, 3) as u8, ..FailoverPolicy::default() };
        return BoundRoute {
            scheme: None,
            source: RouteSource::BuiltIn,
            actions: Box::default(),
            default: bind_action(pool, &RouteAction::default(), failover, None, None),
        };
    };
    let actions: Box<[BoundAction]> = scheme
        .rules
        .iter()
        .map(|r| {
            let a = bind_action(pool, &r.action, r.failover, Some(r.name.clone()), header(&scheme.name, &r.name));
            if !a.applicable && source == RouteSource::Template {
                problems.push(format!(
                    "template {template}: rule \"{}\" of the routing scheme {} names tiers its pool does not have; the rule is skipped",
                    r.name, scheme.name
                ));
            }
            a
        })
        .collect();
    let mut default =
        bind_action(pool, &scheme.default, scheme.default_failover, None, header(&scheme.name, "default"));
    if !default.applicable {
        problems.push(format!(
            "template {template}: the default of the routing scheme {} names tiers its pool does not have; the whole pool applies",
            scheme.name
        ));
        let whole = RouteAction { tiers: Vec::new(), ..scheme.default.clone() };
        default = bind_action(pool, &whole, scheme.default_failover, None, header(&scheme.name, "default"));
    }
    BoundRoute { scheme: Some(scheme), source, actions, default }
}

impl BoundRoute {
    /// The action of the first rule that matches, else the default.
    pub fn action_for(&self, input: &RouteInput<'_>) -> &BoundAction {
        if let Some(s) = &self.scheme {
            for (rule, action) in s.rules.iter().zip(self.actions.iter()) {
                if action.applicable && rule.matches(input) {
                    return action;
                }
            }
        }
        &self.default
    }
}

/// A uniform number in (0, 1] from a hash.
fn unit(h: u64) -> f64 {
    // splitmix64 finaliser: spreads similar inputs.
    let mut z = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    ((z >> 11) as f64 + 1.0) / (1u64 << 53) as f64
}

impl BoundAction {
    /// The models to try for one call, in order: the one the algorithm picks from the first
    /// tier that has a usable model, the rest of that tier by priority, the later tiers, then
    /// the overflow. Models the call may not use (`allow`) are left out; models whose breaker
    /// is open go last rather than out, so that a call is still tried when every model looks
    /// unhealthy. At most `failover.max_targets` models.
    pub fn select(
        &self,
        pool: &[PoolMember],
        input: &RouteInput<'_>,
        allow: impl Fn(&PoolMember) -> bool,
        now: u64,
        rng: &mut fastrand::Rng,
    ) -> Candidates {
        let mut out = Candidates::default();
        let mut later = Candidates::default();
        let mut picked = false;
        let rest = Group { members: self.rest, wrr: Box::default() };
        for g in self.groups.iter().chain(std::iter::once(&rest)) {
            let mut ready = Candidates::default();
            for i in g.members.iter() {
                let m = &pool[i];
                if !allow(m) {
                    continue;
                }
                if m.state.admits(now) { ready.push(i) } else { later.push(i) }
            }
            if !picked && ready.iter().any(|i| pool[i].entry.weight > 0) {
                let first = self.pick(g, &ready, pool, input, rng);
                out.push(first);
                ready.iter().filter(|&i| i != first).for_each(|i| out.push(i));
                picked = true;
            } else {
                ready.iter().for_each(|i| out.push(i));
            }
        }
        later.iter().for_each(|i| out.push(i));
        out.truncate(self.failover.max_targets as usize);
        out
    }

    fn pick(
        &self,
        g: &Group,
        ready: &Candidates,
        pool: &[PoolMember],
        input: &RouteInput<'_>,
        rng: &mut fastrand::Rng,
    ) -> usize {
        let live = || ready.iter().filter(|&i| pool[i].entry.weight > 0);
        let first = live().next().unwrap_or_default();
        if self.affinity == Affinity::Session
            && let Some(session) = input.session_id
        {
            // Weighted rendezvous hashing: losing a model moves only its own sessions.
            let h = stable_hash(session);
            return live()
                .map(|i| (i, pool[i].weight() as f64 / -unit(h ^ pool[i].id_hash).ln()))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map_or(first, |(i, _)| i);
        }
        let weighted = |rng: &mut fastrand::Rng| {
            let total: u64 = live().map(|i| pool[i].weight()).sum();
            let mut r = rng.u64(..total.max(1));
            for i in live() {
                let w = pool[i].weight();
                if r < w {
                    return i;
                }
                r -= w;
            }
            first
        };
        match self.algorithm {
            Algorithm::Priority => first,
            Algorithm::Weighted => weighted(rng),
            Algorithm::RoundRobin => {
                let n = live().count() as u64;
                let k = self.turn.fetch_add(1, Relaxed) % n.max(1);
                live().nth(k as usize).unwrap_or(first)
            }
            Algorithm::WeightedRoundRobin => {
                let len = g.wrr.len() as u64;
                if len == 0 {
                    return first;
                }
                let start = self.turn.fetch_add(1, Relaxed);
                (0..len)
                    .map(|j| g.wrr[((start + j) % len) as usize] as usize)
                    .find(|&i| ready.contains(i))
                    .unwrap_or(first)
            }
            Algorithm::LeastInFlight => {
                // Power of two choices: two weighted picks, the less loaded one wins.
                let a = weighted(rng);
                let b = weighted(rng);
                let load = |i: usize| u64::from(pool[i].state.in_flight()) * pool[if i == a { b } else { a }].weight();
                if load(b) < load(a) { b } else { a }
            }
            Algorithm::LowestLatency => {
                if rng.u32(..100) < EXPLORE_PERCENT {
                    return weighted(rng);
                }
                // A model without a measurement yet counts as fastest, so it gets one.
                live().min_by_key(|&i| pool[i].state.ewma_ttfb_us()).unwrap_or(first)
            }
            Algorithm::Cheapest => live()
                .min_by(|&a, &b| {
                    let p = |i: usize| pool[i].price.unwrap_or(f64::INFINITY);
                    p(a).total_cmp(&p(b))
                })
                .unwrap_or(first),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::time::Duration;

    use super::*;
    use crate::model::RoutingScheme;
    use crate::routing::compile;
    use crate::routing::live::FAILURES_TO_OPEN;
    use serde_json::json;

    fn member(name: &str, tier: Option<&str>, weight: u32, priority: u8) -> PoolMember {
        let model: crate::model::Model = serde_json::from_value(
            json!({ "name": name, "provider_id": "prv_1", "upstream_id": name, "state": "active" }),
        )
        .unwrap();
        PoolMember {
            entry: PoolEntry { model: name.into(), tier: tier.map(Into::into), weight, priority },
            model: Arc::new(ModelRt { id: name.into(), doc: model }),
            state: Arc::new(TargetState::default()),
            id_hash: stable_hash(name),
            price: None,
        }
    }

    fn scheme(v: serde_json::Value) -> Arc<CompiledScheme> {
        Arc::new(compile("rsc_1", 1, &serde_json::from_value::<RoutingScheme>(v).unwrap()).unwrap())
    }

    fn names(pool: &[PoolMember], c: Candidates) -> Vec<&str> {
        c.iter().map(|i| pool[i].entry.model.as_str()).collect()
    }

    fn pick_many(route: &BoundRoute, pool: &[PoolMember], input: &RouteInput<'_>, n: usize) -> HashMap<String, usize> {
        let mut rng = fastrand::Rng::with_seed(7);
        let mut seen = HashMap::new();
        for _ in 0..n {
            let c = route.action_for(input).select(pool, input, |_| true, 0, &mut rng);
            *seen.entry(pool[c.get(0).unwrap()].entry.model.clone()).or_default() += 1;
        }
        seen
    }

    fn pool() -> Vec<PoolMember> {
        vec![
            member("s1", Some("small"), 3, 1),
            member("s2", Some("small"), 1, 0),
            member("l1", Some("large"), 1, 0),
            member("l2", Some("large"), 1, 1),
            member("standby", None, 0, 9),
        ]
    }

    #[test]
    fn the_built_in_route_is_priority_over_the_whole_pool() {
        let pool = pool();
        let route = bind(None, &pool, "t", &mut Vec::new());
        let input = RouteInput::empty();
        let c = route.action_for(&input).select(&pool, &input, |_| true, 0, &mut fastrand::Rng::with_seed(1));
        // s2 and l1 share priority 0: the pool order breaks the tie. Three at most.
        assert_eq!(names(&pool, c), ["s2", "l1", "s1"]);
        assert!(route.default.header.is_none());
        let one = vec![member("only", None, 1, 0)];
        let route = bind(None, &one, "t", &mut Vec::new());
        assert_eq!(route.default.failover.max_targets, 1);
    }

    #[test]
    fn rules_choose_tiers_and_backups_follow() {
        let pool = pool();
        let s = scheme(json!({ "name": "s", "failover": { "max_targets": 8 }, "rules": [
            { "name": "cheap", "when": { "complexity": ["low"] }, "then": { "tiers": ["small"] } },
            { "name": "big", "when": { "task": { "in": ["review"] } },
              "then": { "tiers": ["large", "small"], "overflow": "rest_of_pool" } }
        ], "default": { "tiers": ["large"] } }));
        let route = bind(Some((s, RouteSource::Organisation)), &pool, "t", &mut Vec::new());
        let mut rng = fastrand::Rng::with_seed(1);
        let low = RouteInput { complexity: Some(crate::model::Complexity::Low), ..RouteInput::empty() };
        let a = route.action_for(&low);
        assert_eq!(a.rule.as_deref(), Some("cheap"));
        assert_eq!(a.header.as_ref().unwrap(), "s/cheap");
        assert_eq!(names(&pool, a.select(&pool, &low, |_| true, 0, &mut rng)), ["s2", "s1"]);

        let review = RouteInput { task: Some("review"), ..RouteInput::empty() };
        let a = route.action_for(&review);
        assert_eq!(names(&pool, a.select(&pool, &review, |_| true, 0, &mut rng)), ["l1", "l2", "s2", "s1", "standby"]);

        let other = RouteInput::empty();
        let a = route.action_for(&other);
        assert_eq!((a.rule.as_ref(), a.header.as_ref().unwrap().to_str().unwrap()), (None, "s/default"));
        assert_eq!(names(&pool, a.select(&pool, &other, |_| true, 0, &mut rng)), ["l1", "l2"]);
    }

    #[test]
    fn a_rule_whose_tiers_are_missing_is_skipped() {
        let pool = vec![member("a", None, 1, 0)];
        let s = scheme(json!({ "name": "s", "rules": [
            { "name": "r", "then": { "tiers": ["gpu"] } } ], "default": { "tiers": ["gpu"] } }));
        let mut problems = Vec::new();
        let route = bind(Some((s, RouteSource::Template)), &pool, "t", &mut problems);
        assert_eq!(problems.len(), 2, "{problems:?}");
        let input = RouteInput::empty();
        let a = route.action_for(&input);
        assert!(a.rule.is_none());
        assert_eq!(names(&pool, a.select(&pool, &input, |_| true, 0, &mut fastrand::Rng::new())), ["a"]);
    }

    #[test]
    fn weighted_follows_the_weights_and_weight_zero_is_never_first() {
        let pool = pool();
        let s = scheme(json!({ "name": "s", "default": { "algorithm": "weighted" } }));
        let route = bind(Some((s, RouteSource::Organisation)), &pool, "t", &mut Vec::new());
        let n = 100_000;
        let seen = pick_many(&route, &pool, &RouteInput::empty(), n);
        assert!(!seen.contains_key("standby"));
        // Weights 3:1:1:1 over s1, s2, l1, l2.
        let share = |k: &str| seen.get(k).copied().unwrap_or_default() as f64 / n as f64;
        assert!((share("s1") - 0.5).abs() < 0.015, "{seen:?}");
        for k in ["s2", "l1", "l2"] {
            assert!((share(k) - 1.0 / 6.0).abs() < 0.015, "{seen:?}");
        }
    }

    #[test]
    fn round_robin_and_smooth_weighted_round_robin() {
        let pool = vec![member("a", None, 5, 0), member("b", None, 1, 0), member("c", None, 1, 0)];
        let input = RouteInput::empty();
        let firsts = |algorithm: &str, n: usize| {
            let s = scheme(json!({ "name": "s", "default": { "algorithm": algorithm } }));
            let route = bind(Some((s, RouteSource::Organisation)), &pool, "t", &mut Vec::new());
            route.default.turn.store(0, Relaxed);
            (0..n)
                .map(|_| {
                    let c = route.default.select(&pool, &input, |_| true, 0, &mut fastrand::Rng::new());
                    pool[c.get(0).unwrap()].entry.model.clone()
                })
                .collect::<Vec<_>>()
                .join("")
        };
        assert_eq!(firsts("round_robin", 6), "abcabc");
        // nginx's order for weights 5, 1, 1.
        assert_eq!(firsts("weighted_round_robin", 7), "aabacaa");
    }

    #[test]
    fn wrr_sequences_are_reduced_and_bounded() {
        let pool = vec![member("a", None, 10_000, 0), member("b", None, 5_000, 0), member("c", None, 1, 0)];
        let all: Candidates = (0..3).collect();
        let seq = wrr_sequence(&pool, &all);
        assert!(seq.len() <= 1024 + 3 && seq.contains(&2), "a tiny weight still appears");
        let pool = vec![member("a", None, 400, 0), member("b", None, 200, 0)];
        assert_eq!(wrr_sequence(&pool, &(0..2).collect()).len(), 3, "divided by the common divisor");
    }

    #[test]
    fn allow_and_open_breakers() {
        let pool = pool();
        let route = bind(None, &pool, "t", &mut Vec::new());
        let input = RouteInput::empty();
        let mut rng = fastrand::Rng::new();
        let c = route.default.select(&pool, &input, |m| m.entry.model != "s2", 0, &mut rng);
        assert_eq!(names(&pool, c), ["l1", "s1", "l2"]);
        for _ in 0..FAILURES_TO_OPEN {
            pool[1].state.failure(0, None); // s2
            pool[2].state.failure(0, None); // l1
        }
        let c = route.default.select(&pool, &input, |_| true, 1, &mut rng);
        assert_eq!(names(&pool, c), ["s1", "l2", "standby"], "open breakers go last, cut off by max_targets");
        let s = scheme(json!({ "name": "s", "failover": { "max_targets": 8 } }));
        let route = bind(Some((s, RouteSource::Organisation)), &pool, "t", &mut Vec::new());
        let c = route.default.select(&pool, &input, |_| true, 1, &mut rng);
        assert_eq!(names(&pool, c), ["s1", "l2", "standby", "s2", "l1"]);
        // Every model unhealthy: still tried, in order.
        let two = &pool[1..3];
        let route = bind(None, two, "t", &mut Vec::new());
        let c = route.default.select(two, &input, |_| true, 1, &mut rng);
        assert_eq!(names(two, c), ["s2", "l1"]);
    }

    #[test]
    fn least_in_flight_lowest_latency_and_cheapest() {
        let mut pool = vec![member("a", None, 1, 0), member("b", None, 1, 0)];
        let input = RouteInput::empty();
        let first = |pool: &[PoolMember], algorithm: &str| {
            let s = scheme(json!({ "name": "s", "default": { "algorithm": algorithm } }));
            let route = bind(Some((s, RouteSource::Organisation)), pool, "t", &mut Vec::new());
            pick_many(&route, pool, &input, 1_000)
        };
        let _busy: Vec<_> = (0..5).map(|_| pool[0].state.begin()).collect();
        let seen = first(&pool, "least_in_flight");
        assert!(seen.get("b").copied().unwrap_or_default() > 650, "{seen:?}");

        pool[0].state.success(Duration::from_millis(50));
        pool[1].state.success(Duration::from_millis(500));
        let seen = first(&pool, "lowest_latency");
        let b = seen.get("b").copied().unwrap_or_default();
        assert!((5..=60).contains(&b), "about 5 % exploration: {seen:?}");

        pool[0].price = Some(15.0);
        pool[1].price = Some(3.0);
        assert_eq!(first(&pool, "cheapest").get("b"), Some(&1_000));
        pool[1].price = None;
        assert_eq!(first(&pool, "cheapest").get("a"), Some(&1_000), "an unpriced model is never the cheapest");
    }

    #[test]
    fn session_affinity_is_stable_and_moves_only_the_lost_models_sessions() {
        let pool: Vec<PoolMember> = (0..4).map(|i| member(&format!("m{i}"), None, 1, 0)).collect();
        let s = scheme(json!({ "name": "s", "default": { "algorithm": "weighted", "affinity": "session" } }));
        let route = bind(Some((s, RouteSource::Organisation)), &pool, "t", &mut Vec::new());
        let tags = BTreeMap::new();
        let first = |session: &str, allow: &dyn Fn(&PoolMember) -> bool| {
            let input = RouteInput { session_id: Some(session), tags: &tags, ..RouteInput::empty() };
            route.default.select(&pool, &input, allow, 0, &mut fastrand::Rng::new()).get(0).unwrap()
        };
        let sessions: Vec<String> = (0..2_000).map(|i| format!("session-{i}")).collect();
        let before: Vec<usize> = sessions.iter().map(|s| first(s, &|_| true)).collect();
        assert_eq!(before, sessions.iter().map(|s| first(s, &|_| true)).collect::<Vec<_>>(), "stable");
        let mut counts = [0usize; 4];
        before.iter().for_each(|&i| counts[i] += 1);
        assert!(counts.iter().all(|&c| (400..=600).contains(&c)), "spread: {counts:?}");
        let without_m2 = |m: &PoolMember| m.entry.model != "m2";
        for (s, &b) in sessions.iter().zip(&before) {
            let after = first(s, &without_m2);
            if b != 2 {
                assert_eq!(after, b, "a session not on the lost model stays");
            }
        }
    }
}
