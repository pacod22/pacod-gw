//! Collecting validation problems. A provider learns of every problem in what it announced at
//! once, each with the path of the field, rather than one at a time.

use std::fmt;

/// The problems found in one document, each `path: message`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Problems(Vec<String>);

impl Problems {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, path: impl fmt::Display, message: impl fmt::Display) {
        // A flood of problems helps nobody and costs memory; the first hundred say enough.
        if self.0.len() < 100 {
            self.0.push(format!("{path}: {message}"));
        }
    }

    pub fn check(&mut self, ok: bool, path: impl fmt::Display, message: impl fmt::Display) {
        if !ok {
            self.add(path, message);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn list(&self) -> &[String] {
        &self.0
    }

    pub fn into_result(self) -> Result<(), Problems> {
        if self.0.is_empty() { Ok(()) } else { Err(self) }
    }

    pub fn extend(&mut self, other: Problems) {
        for p in other.0 {
            if self.0.len() < 100 {
                self.0.push(p);
            }
        }
    }
}

impl fmt::Display for Problems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("; "))
    }
}

impl std::error::Error for Problems {}

/// Checks the length of a plain-text field.
pub(crate) fn text_len(p: &mut Problems, path: &str, value: &str, max: usize) {
    if value.chars().count() > max {
        p.add(path, format_args!("at most {max} characters"));
    }
    if value.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        p.add(path, "control characters are not allowed");
    }
}

/// Checks a URL: absolute http or https, bounded.
pub(crate) fn url(p: &mut Problems, path: &str, value: &str) {
    if value.len() > crate::limits::URL_CHARS {
        p.add(path, "URL too long");
    } else if !(value.starts_with("https://") || value.starts_with("http://")) || value.contains(char::is_whitespace) {
        p.add(path, "an absolute http or https URL");
    }
}
