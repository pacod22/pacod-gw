//! What rendering a page gives the scraper, and the interface the browser implements. Tests use
//! a renderer of their own.

use std::future::Future;
use std::pin::Pin;

use pacod_gw_events::jobs::BrowserStatus;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A page as the browser rendered it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rendered {
    /// The status of the page's own response, where the browser reports it.
    pub status: Option<u16>,
    /// Where the page ended up after redirects.
    pub final_url: String,
    pub title: String,
    /// The page as text: one table row per line, its cells joined by ` | `.
    pub text: String,
    /// The addresses the page links to.
    pub links: Vec<String>,
    /// How many bytes the page and what it loaded took.
    pub bytes: u64,
}

pub trait Render: Send + Sync {
    /// Renders `url` and gives back its text.
    fn render<'a>(&'a self, url: &'a str) -> BoxFuture<'a, anyhow::Result<Rendered>>;
    /// Whether the browser answers, for the core's console and `/readyz`.
    fn status(&self) -> BoxFuture<'_, BrowserStatus>;
}
