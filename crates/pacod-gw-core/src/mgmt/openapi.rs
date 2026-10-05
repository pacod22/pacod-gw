//! Assembles the OpenAPI 3.1 document of the management API.
//!
//! Schemas come from the same Rust types the handlers use (`schemars`), and each route is
//! registered next to its handler, so the document cannot describe something the code does not
//! do. The UI's TypeScript client is generated from this document.

use schemars::JsonSchema;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde_json::{Map, Value, json};

pub struct OpenApi {
    generator: SchemaGenerator,
    paths: Map<String, Value>,
}

/// One operation of the API.
pub struct Op {
    method: &'static str,
    path: String,
    id: String,
    summary: String,
    tag: String,
    params: Vec<Value>,
    request: Option<Value>,
    responses: Map<String, Value>,
    public: bool,
}

impl Default for OpenApi {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenApi {
    pub fn new() -> Self {
        let settings = SchemaSettings::draft2020_12().with(|s| {
            s.definitions_path = "#/components/schemas/".into();
        });
        Self { generator: settings.into_generator(), paths: Map::new() }
    }

    /// A schema reference for `T`, registering `T` in the components.
    pub fn schema<T: JsonSchema>(&mut self) -> Value {
        self.generator.subschema_for::<T>().to_value()
    }

    /// Starts an operation. `path` uses `{name}` for parameters; they are declared automatically.
    pub fn op(&self, method: &'static str, path: &str, id: &str, tag: &str, summary: &str) -> Op {
        let params = path
            .split('/')
            .filter_map(|seg| seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .map(|name| json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" } }))
            .collect();
        Op {
            method,
            path: path.to_owned(),
            id: id.to_owned(),
            summary: summary.to_owned(),
            tag: tag.to_owned(),
            params,
            request: None,
            responses: Map::new(),
            public: false,
        }
    }

    pub fn add(&mut self, op: Op) {
        let mut body = json!({
            "operationId": op.id,
            "summary": op.summary,
            "tags": [op.tag],
            "parameters": op.params,
            "responses": op.responses,
        });
        if let Some(r) = op.request {
            body["requestBody"] = r;
        }
        if op.public {
            body["security"] = json!([]);
        }
        // Every operation can fail the same way.
        let err = json!({
            "description": "Error",
            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
        });
        body["responses"]["default"] = err;
        let item = self.paths.entry(op.path).or_insert_with(|| json!({}));
        item[op.method] = body;
    }

    /// The finished document.
    pub fn finish(mut self, version: &str) -> Value {
        let mut schemas = self.generator.take_definitions(true);
        schemas.insert(
            "Error".into(),
            json!({
                "type": "object",
                "required": ["error"],
                "properties": { "error": {
                    "type": "object",
                    "required": ["type", "message"],
                    "properties": {
                        "type": { "type": "string" },
                        "message": { "type": "string" },
                        "details": {}
                    }
                } }
            }),
        );
        json!({
            "openapi": "3.1.0",
            "info": {
                "title": "pacod-gw management API",
                "version": version,
                "description": "Configuration, administration, monitoring and tracking of the gateway."
            },
            "servers": [{ "url": "/api/v1" }],
            "security": [{ "bearer": [] }],
            "paths": self.paths,
            "components": {
                "schemas": schemas,
                "securitySchemes": { "bearer": { "type": "http", "scheme": "bearer" } }
            }
        })
    }
}

impl Op {
    pub fn query(mut self, name: &str, description: &str) -> Self {
        self.params.push(json!({
            "name": name, "in": "query", "required": false,
            "description": description, "schema": { "type": "string" }
        }));
        self
    }

    /// A query parameter that takes a whole number.
    pub fn query_int(mut self, name: &str, description: &str) -> Self {
        self.params.push(json!({
            "name": name, "in": "query", "required": false,
            "description": description, "schema": { "type": "integer", "minimum": 0 }
        }));
        self
    }

    /// The body of a replace: the resource's input plus the version the caller last read.
    pub fn body_with_version(self, schema: Value) -> Self {
        self.body(json!({ "allOf": [schema, {
            "type": "object",
            "properties": { "version": {
                "type": "integer",
                "description": "The version the caller last read. When given, the replace is refused with 409 if the object has changed since."
            } }
        }] }))
    }

    pub fn body(mut self, schema: Value) -> Self {
        self.request = Some(json!({ "required": true, "content": { "application/json": { "schema": schema } } }));
        self
    }

    pub fn ok(self, schema: Value) -> Self {
        self.response("200", "OK", Some(schema))
    }

    pub fn created(self, schema: Value) -> Self {
        self.response("201", "Created", Some(schema))
    }

    pub fn no_content(self) -> Self {
        self.response("204", "No content", None)
    }

    pub fn response(mut self, status: &str, description: &str, schema: Option<Value>) -> Self {
        let mut r = json!({ "description": description });
        if let Some(s) = schema {
            r["content"] = json!({ "application/json": { "schema": s } });
        }
        self.responses.insert(status.to_owned(), r);
        self
    }

    /// Marks the operation as not needing a token.
    pub fn public(mut self) -> Self {
        self.public = true;
        self
    }
}

/// A page of a listing.
pub fn page_schema(item: Value) -> Value {
    json!({
        "type": "object",
        "required": ["items"],
        "properties": {
            "items": { "type": "array", "items": item },
            "next_cursor": { "type": ["string", "null"], "description": "Pass as `cursor` to get the next page." }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Thing {
        name: String,
        size: Option<u32>,
    }

    #[test]
    fn builds_a_document_with_paths_and_schemas() {
        let mut api = OpenApi::new();
        let thing = api.schema::<Thing>();
        api.add(api.op("get", "/things/{id}", "getThing", "things", "Get a thing").ok(thing.clone()));
        api.add(api.op("put", "/things/{id}", "putThing", "things", "Replace a thing").body(thing.clone()).ok(thing));
        let doc = api.finish("0.1.0");
        assert_eq!(doc["openapi"], "3.1.0");
        let get = &doc["paths"]["/things/{id}"]["get"];
        assert_eq!(get["operationId"], "getThing");
        assert_eq!(get["parameters"][0]["name"], "id");
        assert_eq!(
            get["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Thing"
        );
        assert!(doc["paths"]["/things/{id}"]["put"]["requestBody"].is_object());
        assert!(doc["components"]["schemas"]["Thing"]["properties"]["name"].is_object());
        assert!(doc["components"]["schemas"]["Error"].is_object());
    }
}
