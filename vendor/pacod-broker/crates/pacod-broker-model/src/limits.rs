//! Size limits of what a provider may announce. They keep the broker's memory bounded whatever
//! providers send; the binary enforces the totals (resources, leases), the model the sizes of one
//! record.

/// A one-line summary, plain text.
pub const SUMMARY_CHARS: usize = 200;
/// A title, plain text.
pub const TITLE_CHARS: usize = 120;
/// A key, name or tag: a slug.
pub const SLUG_CHARS: usize = 96;
/// Tags on one resource.
pub const TAGS: usize = 32;
/// Interfaces on one resource.
pub const INTERFACES: usize = 16;
/// Usage examples on one resource.
pub const EXAMPLES: usize = 8;
/// Bytes of one usage example.
pub const EXAMPLE_BYTES: usize = 4096;
/// Translations of one text.
pub const LANGUAGES: usize = 8;
/// Items of an inventory, hardware and software each.
pub const INVENTORY_ITEMS: usize = 256;
/// Price periods of one resource.
pub const PRICE_PERIODS: usize = 32;
/// Windows, overrides, tiers or commitments in one price period, each.
pub const PRICE_PARTS: usize = 32;
/// Rules, maintenance windows or slots of one schedule, each.
pub const SCHEDULE_PARTS: usize = 256;
/// MCP tools listed on one resource.
pub const TOOLS: usize = 256;
/// Entries of `capabilities.extra`.
pub const EXTRA: usize = 64;
/// Bytes of the JSON of `capabilities.extra`.
pub const EXTRA_BYTES: usize = 8192;
/// Longest span a time query may ask about: recurring rules are expanded day by day for it.
pub const SPAN_DAYS: i64 = 62;
/// URLs.
pub const URL_CHARS: usize = 2048;
/// Free notes on inventory items and similar.
pub const NOTE_CHARS: usize = 500;
