//! CSV output (RFC 4180) for the reports that offer `format=csv`.
//!
//! A row is any value that serialises to a flat JSON object. The keys of a sample row are the
//! header, so the header is present even when there are no rows. Lists and maps inside a row
//! are written as JSON text.

use axum::http::StatusCode;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;

use crate::error::ApiError;

/// `rows` as a CSV download named `<name>.csv`. `name` is fixed by the report; `sample` gives
/// the header.
pub fn download<T: Serialize>(name: &'static str, sample: &T, rows: &[T]) -> Result<Response, ApiError> {
    let body = render(sample, rows).map_err(|_| {
        tracing::error!(report = name, "CSV rendering failed");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "encoding_failed", "the report could not be written as CSV")
    })?;
    Ok((
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (CONTENT_DISPOSITION, format!("attachment; filename=\"{name}.csv\"")),
        ],
        body,
    )
        .into_response())
}

/// Renders `rows` under the header given by the keys of `sample`.
fn render<T: Serialize>(sample: &T, rows: &[T]) -> serde_json::Result<String> {
    let header: Vec<String> = match serde_json::to_value(sample)? {
        Value::Object(fields) => fields.into_iter().map(|(name, _)| name).collect(),
        _ => Vec::new(),
    };

    let mut out = String::new();
    write_line(&mut out, header.iter().map(String::as_str));
    for row in rows {
        let value = serde_json::to_value(row)?;
        let cells: Vec<String> = header.iter().map(|name| value.get(name).map(cell).unwrap_or_default()).collect();
        write_line(&mut out, cells.iter().map(String::as_str));
    }
    Ok(out)
}

fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn write_line<'a>(out: &mut String, cells: impl Iterator<Item = &'a str>) {
    for (index, cell) in cells.enumerate() {
        if index > 0 {
            out.push(',');
        }
        if cell.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&cell.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(cell);
        }
    }
    out.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_header_comes_from_the_sample_even_without_rows() {
        let sample = json!({ "group": "", "requests": 0, "cost": 0.0 });
        assert_eq!(render(&sample, &[]).expect("csv"), "group,requests,cost\r\n");
    }

    #[test]
    fn cells_are_quoted_only_when_needed() {
        let sample = json!({ "a": "", "b": "", "c": "" });
        let rows = [
            json!({ "a": "plain", "b": "with, comma", "c": "with \"quotes\"" }),
            json!({ "a": "line\nbreak", "b": 1.5, "c": null }),
        ];
        assert_eq!(
            render(&sample, &rows).expect("csv"),
            "a,b,c\r\n\
             plain,\"with, comma\",\"with \"\"quotes\"\"\"\r\n\
             \"line\nbreak\",1.5,\r\n"
        );
    }

    #[test]
    fn nested_values_are_written_as_json_and_booleans_as_words() {
        let sample = json!({ "tags": {}, "list": [], "flag": false });
        let rows = [json!({ "tags": { "purpose": "plan" }, "list": ["a", "b"], "flag": true })];
        assert_eq!(
            render(&sample, &rows).expect("csv"),
            "tags,list,flag\r\n\
             \"{\"\"purpose\"\":\"\"plan\"\"}\",\"[\"\"a\"\",\"\"b\"\"]\",true\r\n"
        );
    }
}
