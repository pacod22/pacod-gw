//! Model ids as sources write them, made comparable.

/// A model id in the form ids are compared in: trimmed, lower case, without Gemini's `models/`.
pub fn normalise_model_id(id: &str) -> String {
    let id = id.trim().to_ascii_lowercase();
    id.strip_prefix("models/").map(str::to_owned).unwrap_or(id)
}

/// A model name as a slug, for matching a name a page writes (`GPT-5 mini`) to an id
/// (`gpt-5-mini`), and a dotted id (`claude-haiku-5.5`) to a hyphenated one
/// (`claude-haiku-5-5`): lower-case letters and digits, everything else one hyphen.
pub fn model_slug(name: &str) -> String {
    let mut out = String::new();
    for c in normalise_model_id(name).chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// A slug without the date of a snapshot (`claude-sonnet-4-5-20250929`, `gpt-4o-2024-08-06`)
/// or `-latest`, so that a snapshot finds its model and the other way round. A version number
/// (`gemini-2.0-flash-001`) is not a date and stays.
pub fn undated(slug: &str) -> &str {
    if let Some(rest) = slug.strip_suffix("-latest") {
        return rest;
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    // -YYYYMMDD
    if let Some((rest, last)) = slug.rsplit_once('-')
        && last.len() == 8
        && digits(last)
        && last.starts_with("20")
    {
        return rest;
    }
    // -YYYY-MM-DD
    let parts: Vec<&str> = slug.rsplitn(4, '-').collect();
    if parts.len() == 4
        && parts[0].len() == 2
        && parts[1].len() == 2
        && parts[2].len() == 4
        && parts.iter().take(3).all(|p| digits(p))
        && parts[2].starts_with("20")
    {
        return parts[3];
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_compare_by_their_slug_and_without_a_date() {
        assert_eq!(normalise_model_id(" models/Gemini-2.5-Pro "), "gemini-2.5-pro");
        assert_eq!(model_slug("claude-haiku-5.5"), "claude-haiku-5-5");
        assert_eq!(model_slug("GPT-5 mini"), "gpt-5-mini");
        assert_eq!(undated("claude-sonnet-4-5-20250929"), "claude-sonnet-4-5");
        assert_eq!(undated("gpt-4o-2024-08-06"), "gpt-4o");
        assert_eq!(undated("gpt-4o-latest"), "gpt-4o");
        assert_eq!(undated("gemini-2-0-flash-001"), "gemini-2-0-flash-001");
        assert_eq!(undated("kimi-k2-0905-preview"), "kimi-k2-0905-preview");
    }
}
