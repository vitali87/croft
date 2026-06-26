// The welcome panel used to bake `git log` output (`CROFT_RELEASE_COMMITS`)
// and the repository remote (`CROFT_REPOSITORY_REMOTE`) into the binary at
// build time. Both are gone: the panel now renders hand-curated release
// highlights from `src/release_notes.rs` (compiled-in data, zero network,
// never derived from a forge), so there is nothing to do at build time.
fn main() {}
