#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use serde::Deserialize;

const BUNDLED_LIST: &str = include_str!("plugins.toml");

#[derive(Debug, Deserialize)]
pub struct PluginEntry {
    pub owner: String,
    pub repo: String,
    pub shorthand: Option<String>,
    pub description: String,
}

#[derive(Debug, Deserialize)]
struct PluginListFile {
    plugins: Vec<PluginEntry>,
}

/// Parsed bundled curated list, lazy-initialized on first access. Panics on
/// parse failure; the unit tests catch malformed `plugins.toml` at build time.
pub fn entries() -> &'static [PluginEntry] {
    static CELL: OnceLock<Vec<PluginEntry>> = OnceLock::new();
    CELL.get_or_init(|| {
        toml::from_str::<PluginListFile>(BUNDLED_LIST)
            .expect("bundled discover/plugins.toml is malformed")
            .plugins
    })
}

/// Look up an entry by its declared `shorthand`, case-insensitively.
/// Entries without a shorthand never match. Empty input never matches —
/// otherwise an entry with `shorthand = ""` would catch every empty lookup.
pub fn lookup_shorthand(name: &str) -> Option<&'static PluginEntry> {
    if name.is_empty() {
        return None;
    }
    entries().iter().find(|e| {
        e.shorthand
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(name))
    })
}

/// Iterate curated entries. When `term` is `Some`, filter by case-insensitive
/// substring match against `repo`, `shorthand`, or `description`.
pub fn iter_filtered(term: Option<&str>) -> impl Iterator<Item = &'static PluginEntry> {
    let needle = term.map(str::to_ascii_lowercase);
    entries().iter().filter(move |e| match &needle {
        None => true,
        Some(n) => {
            e.repo.to_ascii_lowercase().contains(n)
                || e.shorthand
                    .as_deref()
                    .is_some_and(|s| s.to_ascii_lowercase().contains(n))
                || e.description.to_ascii_lowercase().contains(n)
        }
    })
}
