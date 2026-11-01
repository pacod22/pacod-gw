//! A JSON Schema check for answers the gateway asked for in JSON mode (structured output it
//! emulates) and for the parameter schemas it generates.
//!
//! It covers the subset that strict structured output allows and that callers send: types,
//! objects, arrays, enumerations, combinations, local references and numeric and length
//! bounds. `format` and `pattern` are not checked: a pattern needs a regular-expression engine,
//! and the providers that hold an answer to a schema do not check them either. Everything else
//! it does not know is ignored, as JSON Schema treats unknown keywords.

use serde_json::Value;

/// The most problems one check reports.
const MAX_PROBLEMS: usize = 10;
/// How deep references and nesting are followed. A schema deeper than this is malformed or
/// recursive without end for the instance at hand.
const MAX_DEPTH: usize = 64;

/// The ways `instance` does not conform to `schema`, each with the JSON pointer of the value
/// at fault. Empty when it conforms.
pub fn validate(schema: &Value, instance: &Value) -> Vec<String> {
    let mut problems = Check { root: schema }.problems(schema, instance, "", 0);
    problems.truncate(MAX_PROBLEMS);
    problems
}

struct Check<'a> {
    root: &'a Value,
}

fn at(path: &str) -> &str {
    if path.is_empty() { "/" } else { path }
}

/// A JSON pointer token: `~` and `/` escaped.
fn token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_type(v: &Value, name: &str) -> bool {
    match name {
        "null" => v.is_null(),
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "number" => v.is_number(),
        // 1.0 is an integer in JSON Schema.
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0),
        _ => true,
    }
}

/// Equality as JSON Schema means it: numbers by value, so that 1 equals 1.0.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y || x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

impl Check<'_> {
    fn resolve(&self, reference: &str) -> Option<&Value> {
        let pointer = reference.strip_prefix('#')?;
        if pointer.is_empty() {
            return Some(self.root);
        }
        self.root.pointer(pointer)
    }

    fn problems(&self, schema: &Value, v: &Value, path: &str, depth: usize) -> Vec<String> {
        let obj = match schema {
            Value::Bool(true) => return Vec::new(),
            Value::Bool(false) => return vec![format!("{}: no value is allowed here", at(path))],
            Value::Object(o) => o,
            // Not a schema: nothing to hold the value to.
            _ => return Vec::new(),
        };
        if depth > MAX_DEPTH {
            return vec![format!("{}: the schema nests deeper than {MAX_DEPTH} levels", at(path))];
        }
        let mut out = Vec::new();
        let num = |key: &str| obj.get(key).and_then(Value::as_f64);
        let count = |key: &str| obj.get(key).and_then(Value::as_u64);

        if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
            match self.resolve(r) {
                Some(target) => out.extend(self.problems(target, v, path, depth + 1)),
                None => out.push(format!("{}: the reference {r} does not resolve", at(path))),
            }
        }

        // OpenAPI's `nullable` lets a null through whatever the type says.
        let nullable = obj.get("nullable").and_then(Value::as_bool) == Some(true) && v.is_null();
        match obj.get("type") {
            Some(Value::String(t)) if !nullable && !is_type(v, t) => {
                out.push(format!("{}: expected {t}, found {}", at(path), type_name(v)));
                return out;
            }
            Some(Value::Array(ts)) if !nullable && !ts.iter().filter_map(Value::as_str).any(|t| is_type(v, t)) => {
                let names: Vec<&str> = ts.iter().filter_map(Value::as_str).collect();
                out.push(format!("{}: expected {}, found {}", at(path), names.join(" or "), type_name(v)));
                return out;
            }
            _ => {}
        }
        if let Some(values) = obj.get("enum").and_then(Value::as_array)
            && !values.iter().any(|e| same(e, v))
        {
            out.push(format!("{}: {v} is not one of {}", at(path), Value::Array(values.clone())));
        }
        if let Some(c) = obj.get("const")
            && !same(c, v)
        {
            out.push(format!("{}: must be {c}", at(path)));
        }

        match v {
            Value::Object(map) => {
                let properties = obj.get("properties").and_then(Value::as_object);
                for name in
                    obj.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str)
                {
                    if !map.contains_key(name) {
                        out.push(format!("{}: the property \"{name}\" is required", at(path)));
                    }
                }
                for (key, value) in map {
                    let here = format!("{path}/{}", token(key));
                    match properties.and_then(|p| p.get(key)) {
                        Some(s) => out.extend(self.problems(s, value, &here, depth + 1)),
                        // Pattern properties would need a regular-expression engine: with them,
                        // other properties are not judged.
                        None if obj.contains_key("patternProperties") => {}
                        None => match obj.get("additionalProperties") {
                            Some(Value::Bool(false)) => {
                                out.push(format!("{}: the property \"{key}\" is not allowed", at(path)));
                            }
                            Some(s @ Value::Object(_)) => out.extend(self.problems(s, value, &here, depth + 1)),
                            _ => {}
                        },
                    }
                }
                if count("minProperties").is_some_and(|n| (map.len() as u64) < n) {
                    out.push(format!("{}: too few properties", at(path)));
                }
                if count("maxProperties").is_some_and(|n| (map.len() as u64) > n) {
                    out.push(format!("{}: too many properties", at(path)));
                }
            }
            Value::Array(items) => {
                // `prefixItems` (2020-12) or an array under `items` (draft 7) describe the first
                // items one by one; `items` as a schema describes the rest.
                let (prefix, rest) = match (obj.get("prefixItems"), obj.get("items")) {
                    (Some(Value::Array(p)), rest) => (p.as_slice(), rest),
                    (_, Some(Value::Array(p))) => (p.as_slice(), obj.get("additionalItems")),
                    (_, rest) => (&[][..], rest),
                };
                for (i, item) in items.iter().enumerate() {
                    let here = format!("{path}/{i}");
                    if let Some(s) = prefix.get(i).or(rest) {
                        out.extend(self.problems(s, item, &here, depth + 1));
                    }
                }
                if count("minItems").is_some_and(|n| (items.len() as u64) < n) {
                    out.push(format!("{}: fewer than {} items", at(path), count("minItems").unwrap_or_default()));
                }
                if count("maxItems").is_some_and(|n| (items.len() as u64) > n) {
                    out.push(format!("{}: more than {} items", at(path), count("maxItems").unwrap_or_default()));
                }
                if obj.get("uniqueItems").and_then(Value::as_bool) == Some(true)
                    && items.iter().enumerate().any(|(i, a)| items[i + 1..].iter().any(|b| same(a, b)))
                {
                    out.push(format!("{}: the items are not unique", at(path)));
                }
            }
            Value::String(s) => {
                let chars = s.chars().count() as u64;
                if count("minLength").is_some_and(|n| chars < n) {
                    out.push(format!(
                        "{}: shorter than {} characters",
                        at(path),
                        count("minLength").unwrap_or_default()
                    ));
                }
                if count("maxLength").is_some_and(|n| chars > n) {
                    out.push(format!(
                        "{}: longer than {} characters",
                        at(path),
                        count("maxLength").unwrap_or_default()
                    ));
                }
            }
            Value::Number(n) => {
                let x = n.as_f64().unwrap_or_default();
                // Draft 4 spells an exclusive bound as a flag beside `minimum` and `maximum`.
                let flag = |key: &str| obj.get(key).and_then(Value::as_bool) == Some(true);
                if let Some(m) = num("minimum")
                    && (x < m || (flag("exclusiveMinimum") && x <= m))
                {
                    out.push(format!("{}: {x} is below the minimum {m}", at(path)));
                }
                if let Some(m) = num("maximum")
                    && (x > m || (flag("exclusiveMaximum") && x >= m))
                {
                    out.push(format!("{}: {x} is above the maximum {m}", at(path)));
                }
                if let Some(m) = num("exclusiveMinimum")
                    && x <= m
                {
                    out.push(format!("{}: {x} is not above {m}", at(path)));
                }
                if let Some(m) = num("exclusiveMaximum")
                    && x >= m
                {
                    out.push(format!("{}: {x} is not below {m}", at(path)));
                }
                if let Some(m) = num("multipleOf").filter(|m| *m > 0.0) {
                    let q = x / m;
                    if (q - q.round()).abs() > 1e-9 {
                        out.push(format!("{}: {x} is not a multiple of {m}", at(path)));
                    }
                }
            }
            _ => {}
        }

        if let Some(all) = obj.get("allOf").and_then(Value::as_array) {
            for s in all {
                out.extend(self.problems(s, v, path, depth + 1));
            }
        }
        if let Some(any) = obj.get("anyOf").and_then(Value::as_array)
            && !any.iter().any(|s| self.problems(s, v, path, depth + 1).is_empty())
        {
            out.push(format!("{}: matches none of the schemas in anyOf", at(path)));
        }
        if let Some(one) = obj.get("oneOf").and_then(Value::as_array) {
            let matching = one.iter().filter(|s| self.problems(s, v, path, depth + 1).is_empty()).count();
            if matching != 1 {
                out.push(format!("{}: matches {matching} of the schemas in oneOf, not exactly one", at(path)));
            }
        }
        if let Some(not) = obj.get("not")
            && self.problems(not, v, path, depth + 1).is_empty()
        {
            out.push(format!("{}: matches the schema it must not", at(path)));
        }
        out.truncate(MAX_PROBLEMS);
        out
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate;

    fn ok(schema: serde_json::Value, v: serde_json::Value) {
        let p = validate(&schema, &v);
        assert!(p.is_empty(), "{v} against {schema}: {p:?}");
    }

    fn bad(schema: serde_json::Value, v: serde_json::Value, needle: &str) {
        let p = validate(&schema, &v);
        assert!(p.iter().any(|m| m.contains(needle)), "{v} against {schema}: {p:?} lacks {needle:?}");
    }

    #[test]
    fn types() {
        ok(json!({ "type": "integer" }), json!(3));
        ok(json!({ "type": "integer" }), json!(3.0));
        bad(json!({ "type": "integer" }), json!(3.5), "expected integer, found number");
        ok(json!({ "type": ["string", "null"] }), json!(null));
        bad(json!({ "type": ["string", "null"] }), json!(1), "expected string or null");
        ok(json!({ "type": "string", "nullable": true }), json!(null));
        ok(json!(true), json!({ "anything": 1 }));
        bad(json!(false), json!(1), "no value is allowed");
    }

    #[test]
    fn objects() {
        let schema = json!({
            "type": "object",
            "properties": { "word": { "type": "string" }, "n": { "type": "number" } },
            "required": ["word"],
            "additionalProperties": false
        });
        ok(schema.clone(), json!({ "word": "pong" }));
        bad(schema.clone(), json!({}), "the property \"word\" is required");
        bad(schema.clone(), json!({ "word": "x", "extra": 1 }), "\"extra\" is not allowed");
        bad(schema.clone(), json!({ "word": 1 }), "/word: expected string");
        bad(schema, json!([]), "expected object, found array");
        // A schema for the others.
        ok(json!({ "additionalProperties": { "type": "integer" } }), json!({ "a": 1 }));
        bad(json!({ "additionalProperties": { "type": "integer" } }), json!({ "a/b": "x" }), "/a~1b: expected integer");
    }

    #[test]
    fn arrays_enums_and_bounds() {
        let schema = json!({ "type": "array", "items": { "enum": ["a", "b"] }, "minItems": 1, "maxItems": 2 });
        ok(schema.clone(), json!(["a", "b"]));
        bad(schema.clone(), json!([]), "fewer than 1 items");
        bad(schema.clone(), json!(["a", "b", "a"]), "more than 2 items");
        bad(schema, json!(["c"]), "/0: \"c\" is not one of");
        ok(json!({ "prefixItems": [{ "type": "string" }], "items": { "type": "number" } }), json!(["a", 1, 2]));
        bad(json!({ "prefixItems": [{ "type": "string" }], "items": false }), json!(["a", 1]), "/1: no value");
        bad(json!({ "uniqueItems": true }), json!([1, 1.0]), "not unique");
        ok(json!({ "const": 1 }), json!(1.0));
        bad(json!({ "minimum": 0, "maximum": 1 }), json!(1.5), "above the maximum 1");
        bad(json!({ "exclusiveMinimum": 0 }), json!(0), "is not above 0");
        bad(json!({ "minimum": 0, "exclusiveMinimum": true }), json!(0), "below the minimum");
        bad(json!({ "multipleOf": 0.5 }), json!(0.7), "not a multiple");
        bad(json!({ "maxLength": 2 }), json!("äöü"), "longer than 2 characters");
    }

    #[test]
    fn combinations_and_references() {
        let schema = json!({
            "$defs": { "node": { "type": "object", "properties": { "next": { "$ref": "#/$defs/node" } } } },
            "$ref": "#/$defs/node"
        });
        ok(schema.clone(), json!({ "next": { "next": {} } }));
        bad(schema, json!({ "next": { "next": 1 } }), "/next/next: expected object");
        bad(json!({ "$ref": "#/definitions/none" }), json!(1), "does not resolve");
        ok(json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }), json!(null));
        bad(json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }), json!(1), "none of the schemas in anyOf");
        bad(json!({ "oneOf": [{ "type": "number" }, { "type": "integer" }] }), json!(1), "matches 2 of the schemas");
        bad(json!({ "allOf": [{ "minimum": 2 }] }), json!(1), "below the minimum");
        bad(json!({ "not": { "type": "null" } }), json!(null), "must not");
        // A reference to itself with nothing else ends at the depth bound.
        bad(json!({ "$ref": "#" }), json!(1), "deeper than");
    }

    #[test]
    fn problems_are_bounded() {
        let schema = json!({ "type": "array", "items": { "type": "string" } });
        assert_eq!(validate(&schema, &json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])).len(), 10);
    }

    /// A schema of the shape the intent plugin asks with (pacod-gw-plugin-intent, `llm.rs`):
    /// nested objects, every property required, nothing else allowed.
    #[test]
    fn a_strict_caller_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "intent": { "type": "string", "enum": ["billing", "unknown"] },
                "confidence": { "type": "number" },
                "flags": { "type": "object", "properties": { "pii": { "type": "number" } }, "required": ["pii"], "additionalProperties": false }
            },
            "required": ["intent", "confidence", "flags"],
            "additionalProperties": false
        });
        ok(schema.clone(), json!({ "intent": "billing", "confidence": 0.9, "flags": { "pii": 0.05 } }));
        bad(schema, json!({ "intent": "other", "confidence": 0.9, "flags": {} }), "the property \"pii\" is required");
    }
}
