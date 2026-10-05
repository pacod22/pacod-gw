//! Rebuilds a scanned JSON object with parameter members replaced, added or removed.
//!
//! The output is a list of segments. Every member the patch does not name is emitted as a
//! slice of the original buffer, so its bytes are identical to the input by construction.

use std::ops::Range;

use bytes::{Bytes, BytesMut};

use super::scan::{Kind, ScanError, TopLevel, scan};

/// What to do with one member.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Replace the member's value, or add the member. The bytes must be one valid JSON value.
    Set(Bytes),
    /// Add the member only when it is absent.
    SetIfAbsent(Bytes),
    /// Remove the member.
    Remove,
    /// Patch members inside an object value. An absent or non-object value becomes an object
    /// holding the members this patch sets.
    Nested(Patch),
}

/// An ordered list of member operations. Members that are added appear in this order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    ops: Vec<(String, Op)>,
}

impl Patch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn ops(&self) -> &[(String, Op)] {
        &self.ops
    }

    /// Adds an operation. A later operation on the same key replaces the earlier one.
    pub fn push(&mut self, key: impl Into<String>, op: Op) -> &mut Self {
        let key = key.into();
        match self.ops.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = op,
            None => self.ops.push((key, op)),
        }
        self
    }

    pub fn set(&mut self, key: impl Into<String>, value: &serde_json::Value) -> &mut Self {
        self.push(key, Op::Set(render(value)))
    }

    pub fn set_if_absent(&mut self, key: impl Into<String>, value: &serde_json::Value) -> &mut Self {
        self.push(key, Op::SetIfAbsent(render(value)))
    }

    pub fn remove(&mut self, key: impl Into<String>) -> &mut Self {
        self.push(key, Op::Remove)
    }

    /// Merges an operation on a member of the object `key` into the patch.
    pub fn nested(&mut self, key: impl Into<String>, inner_key: impl Into<String>, op: Op) -> &mut Self {
        let key = key.into();
        if let Some((_, Op::Nested(p))) = self.ops.iter_mut().find(|(k, _)| *k == key) {
            p.push(inner_key, op);
            return self;
        }
        let mut p = Patch::new();
        p.push(inner_key, op);
        self.push(key, Op::Nested(p))
    }
}

fn render(value: &serde_json::Value) -> Bytes {
    Bytes::from(serde_json::to_vec(value).expect("a JSON value always serialises"))
}

/// The result of applying a patch.
#[derive(Debug, Clone)]
pub struct PatchedBody {
    segments: Vec<Bytes>,
    len: usize,
    changed: bool,
}

impl PatchedBody {
    /// False when the patch left the body exactly as it was.
    pub fn changed(&self) -> bool {
        self.changed
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn segments(&self) -> &[Bytes] {
        &self.segments
    }

    pub fn into_segments(self) -> Vec<Bytes> {
        self.segments
    }

    /// The body as one buffer. Copies only when there is more than one segment.
    pub fn to_bytes(&self) -> Bytes {
        match self.segments.as_slice() {
            [] => Bytes::new(),
            [one] => one.clone(),
            many => {
                let mut out = BytesMut::with_capacity(self.len);
                for s in many {
                    out.extend_from_slice(s);
                }
                out.freeze()
            }
        }
    }
}

enum Seg {
    /// A range of the original buffer.
    Orig(Range<usize>),
    New(Bytes),
}

struct Out {
    segs: Vec<Seg>,
}

impl Out {
    fn orig(&mut self, r: Range<usize>) {
        if r.is_empty() {
            return;
        }
        if let Some(Seg::Orig(last)) = self.segs.last_mut()
            && last.end == r.start
        {
            last.end = r.end;
            return;
        }
        self.segs.push(Seg::Orig(r));
    }

    fn new_bytes(&mut self, b: Bytes) {
        if !b.is_empty() {
            self.segs.push(Seg::New(b));
        }
    }

    fn lit(&mut self, s: &'static [u8]) {
        self.segs.push(Seg::New(Bytes::from_static(s)));
    }
}

/// Applies `patch` to `body`, whose top-level layout is `top`.
///
/// Fails only when an object the patch has to write into repeats a key.
pub fn apply(body: &Bytes, top: &TopLevel, patch: &Patch) -> Result<PatchedBody, ScanError> {
    if patch.is_empty() {
        return Ok(PatchedBody { segments: vec![body.clone()], len: body.len(), changed: false });
    }
    let mut out = Out { segs: Vec::new() };
    let changed = patch_object(body, 0, body.len(), top, patch, &mut out)?;
    if !changed {
        return Ok(PatchedBody { segments: vec![body.clone()], len: body.len(), changed: false });
    }
    let segments: Vec<Bytes> = out
        .segs
        .into_iter()
        .map(|s| match s {
            Seg::Orig(r) => body.slice(r),
            Seg::New(b) => b,
        })
        .collect();
    let len = segments.iter().map(Bytes::len).sum();
    Ok(PatchedBody { segments, len, changed: true })
}

/// What happens to one existing member.
enum Action {
    Keep,
    /// Replace the value with freshly rendered bytes.
    Replace(Bytes),
    /// Patch inside the object value.
    Recurse(TopLevel, Patch),
    Drop,
}

/// Emits the object occupying `body[base..end]` with `patch` applied. Offsets in `top` are
/// relative to `base`. Returns whether anything changed.
fn patch_object(
    body: &Bytes,
    base: usize,
    end: usize,
    top: &TopLevel,
    patch: &Patch,
    out: &mut Out,
) -> Result<bool, ScanError> {
    let entries = &top.entries;
    let mut used = vec![false; patch.ops.len()];
    let mut actions: Vec<Action> = Vec::with_capacity(entries.len());
    let mut changed = false;

    for e in entries {
        let op = patch.ops.iter().position(|(k, _)| *k == e.key);
        let action = match op.map(|i| {
            used[i] = true;
            &patch.ops[i].1
        }) {
            None | Some(Op::SetIfAbsent(_)) => Action::Keep,
            Some(Op::Set(v)) => {
                if body[base + e.value.start..base + e.value.end] == v[..] {
                    Action::Keep
                } else {
                    Action::Replace(v.clone())
                }
            }
            Some(Op::Remove) => Action::Drop,
            Some(Op::Nested(p)) => {
                if e.kind == Kind::Object {
                    let sub = scan(&body[base + e.value.start..base + e.value.end])?;
                    Action::Recurse(sub, p.clone())
                } else {
                    match build_object(p) {
                        Some(obj) => Action::Replace(obj),
                        None => Action::Keep,
                    }
                }
            }
        };
        if !matches!(action, Action::Keep | Action::Recurse(..)) {
            changed = true;
        }
        actions.push(action);
    }

    // Members to append: operations whose key was not present.
    let mut appended: Vec<Bytes> = Vec::new();
    for (i, (key, op)) in patch.ops.iter().enumerate() {
        if used[i] {
            continue;
        }
        let value = match op {
            Op::Set(v) | Op::SetIfAbsent(v) => Some(v.clone()),
            Op::Nested(p) => build_object(p),
            Op::Remove => None,
        };
        if let Some(v) = value {
            appended.push(member(key, &v));
        }
    }
    if !appended.is_empty() {
        changed = true;
    }

    // Bytes before the first member (the opening brace and any whitespace), and after the last.
    let head_end = match entries.first() {
        Some(first) => base + first.key_start,
        None => base + top.open + 1,
    };
    let tail_start = match entries.last() {
        Some(last) => base + last.value.end,
        None => base + top.open + 1,
    };

    // Remember the output state, so that an object that turns out unchanged can be emitted as
    // one original range. The first write below may extend the last segment instead of adding
    // one, so its range is saved as well.
    let mark = out.segs.len();
    let saved_last = match out.segs.last() {
        Some(Seg::Orig(r)) => Some(r.clone()),
        _ => None,
    };
    out.orig(base..head_end);
    let mut last_emitted: Option<usize> = None;
    let mut nested_changed = false;

    for (i, (e, action)) in entries.iter().zip(actions).enumerate() {
        if matches!(action, Action::Drop) {
            continue;
        }
        // Separator: the original one when the previous surviving member was the original
        // neighbour, a plain comma otherwise.
        match last_emitted {
            None => {}
            Some(prev) if prev + 1 == i => out.orig(base + entries[prev].value.end..base + e.key_start),
            Some(_) => out.lit(b","),
        }
        last_emitted = Some(i);
        let key_and_colon = base + e.key_start..base + e.value.start;
        match action {
            Action::Keep => out.orig(base + e.key_start..base + e.value.end),
            Action::Replace(v) => {
                out.orig(key_and_colon);
                out.new_bytes(v);
            }
            Action::Recurse(sub, p) => {
                out.orig(key_and_colon);
                let vb = base + e.value.start;
                nested_changed |= patch_object(body, vb, base + e.value.end, &sub, &p, out)?;
            }
            Action::Drop => unreachable!(),
        }
    }

    for m in appended {
        if last_emitted.is_some() {
            out.lit(b",");
        }
        // Any number larger than every index marks "something was emitted".
        last_emitted = Some(usize::MAX - 1);
        out.new_bytes(m);
    }

    out.orig(tail_start..end);

    let changed = changed || nested_changed;
    if !changed {
        // Nothing differs: collapse whatever was emitted back into one original range.
        out.segs.truncate(mark);
        if let (Some(saved), Some(Seg::Orig(last))) = (saved_last, out.segs.last_mut()) {
            *last = saved;
        }
        out.orig(base..end);
    }
    Ok(changed)
}

/// `"key":value`
fn member(key: &str, value: &[u8]) -> Bytes {
    let k = serde_json::to_vec(key).expect("a string always serialises");
    let mut b = BytesMut::with_capacity(k.len() + 1 + value.len());
    b.extend_from_slice(&k);
    b.extend_from_slice(b":");
    b.extend_from_slice(value);
    b.freeze()
}

/// An object holding the members `patch` sets, or `None` when it sets none.
fn build_object(patch: &Patch) -> Option<Bytes> {
    let mut b = BytesMut::new();
    b.extend_from_slice(b"{");
    let mut first = true;
    for (key, op) in &patch.ops {
        let value = match op {
            Op::Set(v) | Op::SetIfAbsent(v) => Some(v.clone()),
            Op::Nested(p) => build_object(p),
            Op::Remove => None,
        };
        if let Some(v) = value {
            if !first {
                b.extend_from_slice(b",");
            }
            first = false;
            b.extend_from_slice(&member(key, &v));
        }
    }
    if first {
        return None;
    }
    b.extend_from_slice(b"}");
    Some(b.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::{Value, json};

    fn run(body: &str, patch: &Patch) -> (String, bool) {
        let body = Bytes::copy_from_slice(body.as_bytes());
        let top = scan(&body).unwrap();
        let out = apply(&body, &top, patch).unwrap();
        (String::from_utf8(out.to_bytes().to_vec()).unwrap(), out.changed())
    }

    #[test]
    fn concept_planner_example() {
        // Concept appendix B: @planner = claude-haiku-4-5 with a ceiling of 400 output tokens.
        let body = r#"{"model":"@planner","max_tokens":8000,
       "messages":[{"role":"user","content":"Plan the next step."}]}"#;
        let mut p = Patch::new();
        p.set("model", &json!("claude-haiku-4-5")).set("max_tokens", &json!(400));
        let (out, changed) = run(body, &p);
        assert!(changed);
        assert_eq!(
            out,
            r#"{"model":"claude-haiku-4-5","max_tokens":400,
       "messages":[{"role":"user","content":"Plan the next step."}]}"#
        );
    }

    #[test]
    fn content_members_are_the_same_buffer() {
        // The content carries an escape and double spaces, which a re-serialiser would change.
        let body = Bytes::from_static(
            b"{\"model\":\"a\",\"messages\":[{\"role\":\"user\",\"content\":\"x  y\\u00e9\"}],\"system\":\"S\",\"tools\":[]}",
        );
        assert!(body.windows(6).any(|w| w == b"\\u00e9"));
        let top = scan(&body).unwrap();
        let mut p = Patch::new();
        p.set("model", &json!("b")).set("temperature", &json!(0.3));
        let out = apply(&body, &top, &p).unwrap();
        // The segment holding `messages` points into the original allocation.
        let messages = top.get("messages").unwrap().value.clone();
        let orig_ptr = body[messages.clone()].as_ptr();
        let hit = out.segments().iter().any(|s| {
            let r = s.as_ptr_range();
            r.start <= orig_ptr && orig_ptr < r.end
        });
        assert!(hit, "messages must be emitted as a slice of the input");
        let text = out.to_bytes();
        let t2 = scan(&text).unwrap();
        for k in ["messages", "system", "tools"] {
            assert_eq!(t2.raw(&text, k), top.raw(&body, k), "{k} must be byte-identical");
        }
    }

    #[test]
    fn empty_patch_and_no_op_patch_return_the_input() {
        let body = r#"{ "model" : "m" , "n": 1 }"#;
        let (out, changed) = run(body, &Patch::new());
        assert_eq!((out.as_str(), changed), (body, false));

        let mut p = Patch::new();
        p.set("model", &json!("m")).set_if_absent("n", &json!(5)).remove("absent");
        let (out, changed) = run(body, &p);
        assert_eq!((out.as_str(), changed), (body, false));
    }

    #[test]
    fn remove_first_last_only_and_adjacent() {
        let rm = |body: &str, keys: &[&str]| {
            let mut p = Patch::new();
            for k in keys {
                p.remove(*k);
            }
            run(body, &p).0
        };
        let body = r#"{"a":1,"b":2,"c":3,"d":4}"#;
        assert_eq!(rm(body, &["a"]), r#"{"b":2,"c":3,"d":4}"#);
        assert_eq!(rm(body, &["d"]), r#"{"a":1,"b":2,"c":3}"#);
        assert_eq!(rm(body, &["b", "c"]), r#"{"a":1,"d":4}"#);
        assert_eq!(rm(body, &["a", "b"]), r#"{"c":3,"d":4}"#);
        assert_eq!(rm(body, &["a", "c"]), r#"{"b":2,"d":4}"#);
        assert_eq!(rm(body, &["a", "b", "c", "d"]), r#"{}"#);
        assert_eq!(rm(r#"{"a":1}"#, &["a"]), r#"{}"#);
        assert_eq!(rm(r#"{ "a" : 1 }"#, &["a"]), r#"{  }"#);
    }

    #[test]
    fn append_to_empty_and_non_empty() {
        let mut p = Patch::new();
        p.set("model", &json!("m")).set_if_absent("max_tokens", &json!(10));
        assert_eq!(run("{}", &p).0, r#"{"model":"m","max_tokens":10}"#);
        assert_eq!(run("{ }", &p).0, r#"{"model":"m","max_tokens":10 }"#);
        assert_eq!(run(r#"{"a":1}"#, &p).0, r#"{"a":1,"model":"m","max_tokens":10}"#);
    }

    #[test]
    fn remove_all_then_append() {
        let mut p = Patch::new();
        p.remove("a").set("b", &json!(2));
        assert_eq!(run(r#"{"a":1}"#, &p).0, r#"{"b":2}"#);
    }

    #[test]
    fn nested_patch_keeps_untouched_members() {
        // A caller's `format` schema inside output_config must stay as sent.
        let body =
            r#"{"output_config":{"format":{"type":"json_schema","schema":{"a":  1}},"effort":"low"},"model":"m"}"#;
        let mut p = Patch::new();
        p.nested("output_config", "effort", Op::Set(Bytes::from_static(b"\"high\"")));
        assert_eq!(
            run(body, &p).0,
            r#"{"output_config":{"format":{"type":"json_schema","schema":{"a":  1}},"effort":"high"},"model":"m"}"#
        );
    }

    #[test]
    fn nested_patch_creates_the_object() {
        let mut p = Patch::new();
        p.nested("stream_options", "include_usage", Op::Set(Bytes::from_static(b"true")));
        assert_eq!(run(r#"{"stream":true}"#, &p).0, r#"{"stream":true,"stream_options":{"include_usage":true}}"#);
        // A null value is replaced by the object.
        assert_eq!(
            run(r#"{"stream_options":null,"stream":true}"#, &p).0,
            r#"{"stream_options":{"include_usage":true},"stream":true}"#
        );
        // Already as wanted: nothing changes.
        let same = r#"{"stream_options":{"include_usage":true}}"#;
        assert_eq!(run(same, &p), (same.to_owned(), false));
    }

    #[test]
    fn nested_default_does_not_override() {
        let mut p = Patch::new();
        p.nested("output_config", "effort", Op::SetIfAbsent(Bytes::from_static(b"\"high\"")));
        let body = r#"{"output_config":{"effort":"low"}}"#;
        assert_eq!(run(body, &p), (body.to_owned(), false));
        assert_eq!(run(r#"{"output_config":{}}"#, &p).0, r#"{"output_config":{"effort":"high"}}"#);
    }

    #[test]
    fn duplicate_key_inside_a_written_object_is_rejected() {
        let body = Bytes::from_static(br#"{"output_config":{"effort":"low","effort":"max"}}"#);
        let top = scan(&body).unwrap();
        let mut p = Patch::new();
        p.nested("output_config", "effort", Op::Set(Bytes::from_static(b"\"high\"")));
        assert_eq!(apply(&body, &top, &p).unwrap_err(), ScanError::DuplicateKey("effort".into()));
    }

    #[test]
    fn keys_needing_escapes_are_written_correctly() {
        let mut p = Patch::new();
        p.set("a\"b", &json!(1));
        let (out, _) = run("{}", &p);
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!({"a\"b": 1}));
    }

    #[test]
    fn patching_is_idempotent() {
        let body = r#"{"model":"x","temperature":1.0,"messages":[]}"#;
        let mut p = Patch::new();
        p.set("model", &json!("y")).remove("temperature").set_if_absent("max_tokens", &json!(64));
        let (once, changed) = run(body, &p);
        assert!(changed);
        let (twice, changed_again) = run(&once, &p);
        assert_eq!(once, twice);
        assert!(!changed_again);
    }

    // --- differential test against serde_json ---

    fn leaf() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            (-1000i64..1000).prop_map(|n| json!(n)),
            "[a-z \\\\\"{}\\[\\],:é]{0,8}".prop_map(Value::String),
        ]
    }

    fn value() -> impl Strategy<Value = Value> {
        leaf().prop_recursive(3, 16, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::vec((key(), inner), 0..4).prop_map(|kv| {
                    let mut m = serde_json::Map::new();
                    for (k, v) in kv {
                        m.insert(k, v);
                    }
                    Value::Object(m)
                }),
            ]
        })
    }

    fn key() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("model".to_owned()),
            Just("max_tokens".to_owned()),
            Just("cfg".to_owned()),
            Just("messages".to_owned()),
            Just("a b".to_owned()),
            Just("é\"".to_owned()),
        ]
    }

    #[derive(Debug, Clone)]
    enum TestOp {
        Set(Value),
        SetIfAbsent(Value),
        Remove,
        Nested(Vec<(String, TestOp)>),
    }

    fn flat_op() -> impl Strategy<Value = TestOp> {
        prop_oneof![value().prop_map(TestOp::Set), value().prop_map(TestOp::SetIfAbsent), Just(TestOp::Remove),]
    }

    fn test_op() -> impl Strategy<Value = TestOp> {
        prop_oneof![
            4 => flat_op(),
            1 => prop::collection::vec((key(), flat_op()), 1..3).prop_map(TestOp::Nested),
        ]
    }

    fn to_patch(ops: &[(String, TestOp)]) -> Patch {
        let mut p = Patch::new();
        for (k, op) in ops {
            let op = match op {
                TestOp::Set(v) => Op::Set(render(v)),
                TestOp::SetIfAbsent(v) => Op::SetIfAbsent(render(v)),
                TestOp::Remove => Op::Remove,
                TestOp::Nested(inner) => Op::Nested(to_patch(inner)),
            };
            p.push(k.clone(), op);
        }
        p
    }

    /// The same semantics on a parsed tree.
    fn apply_model(obj: &mut serde_json::Map<String, Value>, patch: &Patch) {
        for (k, op) in patch.ops() {
            match op {
                Op::Set(v) => {
                    obj.insert(k.clone(), serde_json::from_slice(v).unwrap());
                }
                Op::SetIfAbsent(v) => {
                    if !obj.contains_key(k) {
                        obj.insert(k.clone(), serde_json::from_slice(v).unwrap());
                    }
                }
                Op::Remove => {
                    obj.shift_remove(k);
                }
                Op::Nested(p) => match obj.get_mut(k) {
                    Some(Value::Object(inner)) => apply_model(inner, p),
                    _ => {
                        let mut fresh = serde_json::Map::new();
                        apply_model(&mut fresh, p);
                        if !fresh.is_empty() {
                            obj.insert(k.clone(), Value::Object(fresh));
                        }
                    }
                },
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(600))]

        #[test]
        fn matches_a_tree_based_implementation(
            members in prop::collection::vec((key(), value()), 0..6),
            ops in prop::collection::vec((key(), test_op()), 0..5),
            pretty in any::<bool>(),
        ) {
            let mut obj = serde_json::Map::new();
            for (k, v) in members {
                obj.insert(k, v);
            }
            let text = if pretty {
                serde_json::to_string_pretty(&Value::Object(obj.clone())).unwrap()
            } else {
                serde_json::to_string(&Value::Object(obj.clone())).unwrap()
            };
            let patch = to_patch(&ops);

            let body = Bytes::from(text.clone());
            let top = scan(&body).unwrap();
            let out = apply(&body, &top, &patch).unwrap();
            let out_bytes = out.to_bytes();
            prop_assert_eq!(out.len(), out_bytes.len());

            // The output is valid JSON for the strict scanner as well as for serde_json.
            prop_assert!(scan(&out_bytes).is_ok(), "output must scan: {}", String::from_utf8_lossy(&out_bytes));
            let got: Value = serde_json::from_slice(&out_bytes).unwrap();

            let mut want = obj.clone();
            apply_model(&mut want, &patch);
            prop_assert_eq!(&got, &Value::Object(want));

            // "changed" is exact: unchanged means byte-identical.
            if !out.changed() {
                prop_assert_eq!(&out_bytes[..], text.as_bytes());
            }
            // Applying the same patch again changes nothing more.
            let top2 = scan(&out_bytes).unwrap();
            let again = apply(&out_bytes, &top2, &patch).unwrap();
            prop_assert_eq!(&again.to_bytes()[..], &out_bytes[..]);
        }
    }
}
