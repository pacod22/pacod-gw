//! Where the configuration comes from: an env file and the process environment.
//!
//! In a container, Compose passes the `.env` file in as environment variables. On a host the
//! same file can be named with `--env-file`. Variables already set in the process win over the
//! file, as with Compose.
//!
//! The file is read as Compose reads an `env_file`, without interpolation:
//! - `NAME=value` per line; empty lines and lines starting with `#` are skipped;
//! - an optional `export ` in front;
//! - unquoted values are trimmed, and ` #` starts a comment;
//! - `'…'` is taken literally; `"…"` knows `\n`, `\t`, `\"` and `\\`;
//! - `$$` stands for `$` outside single quotes, because Compose would read a single `$` as the
//!   start of a variable.

use std::collections::BTreeMap;
use std::path::Path;

/// The configuration variables, by name.
#[derive(Debug, Clone, Default)]
pub struct Vars(BTreeMap<String, String>);

impl Vars {
    /// The process environment over the env file, if one is named.
    pub fn load(env_file: Option<&Path>) -> anyhow::Result<Self> {
        let mut vars = match env_file {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| anyhow::anyhow!("reading the env file {}: {e}", path.display()))?;
                parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?
            }
            None => Self::default(),
        };
        vars.0.extend(std::env::vars());
        Ok(vars)
    }

    pub fn from_pairs<K: Into<String>, V: Into<String>>(pairs: impl IntoIterator<Item = (K, V)>) -> Self {
        Self(pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Reads the text of an env file. Errors name the line.
pub fn parse(text: &str) -> Result<Vars, String> {
    let mut vars = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((name, raw)) = line.split_once('=') else {
            return Err(format!("line {n}: expected NAME=value"));
        };
        let name = name.trim();
        if !is_name(name) {
            return Err(format!("line {n}: \"{name}\" is not a variable name"));
        }
        let value = value(raw.trim_start()).map_err(|e| format!("line {n} ({name}): {e}"))?;
        vars.insert(name.to_owned(), value);
    }
    Ok(Vars(vars))
}

fn is_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn value(raw: &str) -> Result<String, String> {
    if let Some(rest) = raw.strip_prefix('\'') {
        let end = rest.find('\'').ok_or("the single quote is not closed")?;
        return after_quote(&rest[end + 1..]).map(|()| rest[..end].to_owned());
    }
    if let Some(rest) = raw.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    after_quote(&rest[i + 1..])?;
                    return Ok(out.replace("$$", "$"));
                }
                '\\' => match chars.next().map(|(_, c)| c) {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(c @ ('"' | '\\' | '$')) => out.push(c),
                    Some(c) => {
                        out.push('\\');
                        out.push(c);
                    }
                    None => return Err("the double quote is not closed".into()),
                },
                c => out.push(c),
            }
        }
        return Err("the double quote is not closed".into());
    }
    let unquoted = match raw.find(" #") {
        Some(i) => &raw[..i],
        None => raw,
    };
    Ok(unquoted.trim_end().replace("$$", "$"))
}

/// Only a comment may follow a closing quote.
fn after_quote(rest: &str) -> Result<(), String> {
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(format!("unexpected \"{rest}\" after the quote"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_forms_compose_reads() {
        let v = parse(
            "# a comment\n\
             \n\
             PLAIN=value\n\
             SPACED =  padded value   \n\
             export EXPORTED=yes\n\
             COMMENTED=abc # not part of it\n\
             HASH=abc#def\n\
             SINGLE='literal $$ \\n # kept'\n\
             DOUBLE=\"line\\nnext \\\"quoted\\\" $$5\" # comment\n\
             DOLLARS=pa$$word\n\
             EMPTY=\n",
        )
        .unwrap();
        assert_eq!(v.get("PLAIN"), Some("value"));
        assert_eq!(v.get("SPACED"), Some("padded value"));
        assert_eq!(v.get("EXPORTED"), Some("yes"));
        assert_eq!(v.get("COMMENTED"), Some("abc"));
        assert_eq!(v.get("HASH"), Some("abc#def"));
        assert_eq!(v.get("SINGLE"), Some("literal $$ \\n # kept"));
        assert_eq!(v.get("DOUBLE"), Some("line\nnext \"quoted\" $5"));
        assert_eq!(v.get("DOLLARS"), Some("pa$word"));
        assert_eq!(v.get("EMPTY"), Some(""));
    }

    #[test]
    fn says_which_line_is_wrong() {
        assert_eq!(parse("A=1\nnot a line\n").unwrap_err(), "line 2: expected NAME=value");
        assert!(parse("1A=x").unwrap_err().contains("not a variable name"));
        assert!(parse("A='open").unwrap_err().contains("line 1 (A)"));
        assert!(parse("A=\"x\" y").unwrap_err().contains("after the quote"));
    }

    #[test]
    fn the_process_environment_wins_over_the_file() {
        let dir = std::env::temp_dir().join(format!("pgw-lite-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.env");
        // PATH is set in every test process; the file's value must not replace it.
        std::fs::write(&file, "PATH=/from/the/file\nPGW_LITE_TEST_ONLY_IN_FILE=1\n").unwrap();
        let v = Vars::load(Some(&file)).unwrap();
        assert_ne!(v.get("PATH"), Some("/from/the/file"));
        assert_eq!(v.get("PGW_LITE_TEST_ONLY_IN_FILE"), Some("1"));
        assert!(Vars::load(Some(&dir.join("missing.env"))).unwrap_err().to_string().contains("missing.env"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
