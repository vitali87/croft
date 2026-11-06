//! Comma-separated glob lists, as VS Code's `files.exclude` style settings
//! and the Search view's include and exclude boxes spell them. A leaf, so
//! readers outside the UI (quickfix's rg seeding) need not reach into the
//! Search widget.

/// The entries of a comma-separated glob list. Commas inside `{...}`
/// belong to a brace alternate, so `*.{ts,tsx}, docs/**` is two entries
/// (#1251).
pub(crate) fn split_globs(raw: &str) -> impl Iterator<Item = &str> {
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in raw.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&raw[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&raw[start..]);
    parts.into_iter().map(str::trim).filter(|p| !p.is_empty())
}
