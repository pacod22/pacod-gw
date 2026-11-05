//! `robots.txt` and sitemaps, as far as the scan needs them: which addresses a site lists, and
//! which it asks crawlers to leave alone. Not a general parser: a sitemap's `<loc>` elements are
//! read wherever they are, and compressed sitemaps are skipped.

/// What a site's `robots.txt` says to every crawler (`User-agent: *`) and to this one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Robots {
    pub sitemaps: Vec<String>,
    allow: Vec<String>,
    disallow: Vec<String>,
}

impl Robots {
    pub fn parse(text: &str) -> Self {
        let mut out = Robots::default();
        // Whether the group being read applies here, and whether the line before was an agent
        // line (agents of one group come in a row).
        let (mut applies, mut in_agents) = (false, false);
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once(':') else { continue };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "sitemap" => out.sitemaps.push(value.to_owned()),
                "user-agent" => {
                    let ours = value == "*" || value.eq_ignore_ascii_case("pacod-gw-scraper");
                    applies = if in_agents { applies || ours } else { ours };
                    in_agents = true;
                    continue;
                }
                "allow" if applies && !value.is_empty() => out.allow.push(value.to_owned()),
                // An empty Disallow allows everything.
                "disallow" if applies && !value.is_empty() => out.disallow.push(value.to_owned()),
                _ => {}
            }
            in_agents = false;
        }
        out
    }

    /// Whether the site asks crawlers to leave `path` alone. The longest matching rule wins;
    /// `*` matches anything, `$` ends a rule.
    pub fn disallows(&self, path: &str) -> bool {
        let longest = |rules: &[String]| rules.iter().filter(|r| matches(r, path)).map(String::len).max();
        match (longest(&self.disallow), longest(&self.allow)) {
            (Some(d), Some(a)) => d > a,
            (Some(_), None) => true,
            _ => false,
        }
    }
}

fn matches(rule: &str, path: &str) -> bool {
    let (rule, anchored) = match rule.strip_suffix('$') {
        Some(r) => (r, true),
        None => (rule, false),
    };
    let parts: Vec<&str> = rule.split('*').collect();
    let Some(mut rest) = path.strip_prefix(parts[0]) else { return false };
    for (i, part) in parts.iter().enumerate().skip(1) {
        if anchored && i == parts.len() - 1 {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    !anchored || rest.is_empty()
}

/// What a sitemap lists: the addresses of pages, and other sitemaps (a sitemap index).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sitemap {
    pub pages: Vec<String>,
    pub sitemaps: Vec<String>,
}

/// Reads a sitemap or a sitemap index.
pub fn parse_sitemap(xml: &str) -> Sitemap {
    let index = xml.contains("<sitemapindex");
    let mut out = Sitemap::default();
    let mut rest = xml;
    while let Some(start) = rest.find("<loc>") {
        let after = &rest[start + 5..];
        let Some(end) = after.find("</loc>") else { break };
        let loc = after[..end].trim().replace("&amp;", "&");
        let loc = loc.trim_start_matches("<![CDATA[").trim_end_matches("]]>").trim().to_owned();
        if !loc.is_empty() {
            if index { out.sitemaps.push(loc) } else { out.pages.push(loc) }
        }
        rest = &after[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_rules_for_every_crawler_apply() {
        let r = Robots::parse(
            "User-agent: Googlebot\nDisallow: /\n\nUser-agent: *\nUser-agent: other\nDisallow: /private/\nDisallow: /*.json$\n\
             Allow: /private/pricing\nDisallow:\n# a note\nSitemap: https://docs.example.com/sitemap.xml\n",
        );
        assert_eq!(r.sitemaps, ["https://docs.example.com/sitemap.xml"]);
        assert!(r.disallows("/private/keys"));
        assert!(!r.disallows("/private/pricing"), "the longer Allow wins");
        assert!(r.disallows("/data/prices.json") && !r.disallows("/data/prices.json.html"));
        assert!(!r.disallows("/docs/pricing"), "Googlebot's rule is not ours");
        assert!(!Robots::parse("").disallows("/anything"));
    }

    #[test]
    fn a_sitemap_and_an_index_are_read() {
        let urls = parse_sitemap(
            "<?xml version=\"1.0\"?><urlset><url><loc> https://docs.example.com/pricing </loc></url>\
             <url><loc>https://docs.example.com/models?a=1&amp;b=2</loc></url></urlset>",
        );
        assert_eq!(urls.pages, ["https://docs.example.com/pricing", "https://docs.example.com/models?a=1&b=2"]);
        let index =
            parse_sitemap("<sitemapindex><sitemap><loc>https://docs.example.com/s1.xml</loc></sitemap></sitemapindex>");
        assert_eq!((index.sitemaps.len(), index.pages.len()), (1, 0));
        assert_eq!(parse_sitemap("<html>not a sitemap</html>"), Sitemap::default());
    }
}
