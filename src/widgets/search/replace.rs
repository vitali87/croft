//! The Search panel's Replace All on disk, over the files its hits came from.

use super::*;

impl SearchPanel {
    /// Replace every match in every distinct file among the current hits with
    /// `self.replace`, honouring the active `SearchOpts`. Returns the total
    /// number of matches replaced across all files. The caller is responsible
    /// for re-running the query afterwards so the (now stale) hit list updates.
    #[cfg(test)]
    pub fn replace_all_on_disk(&self) -> usize {
        let needle = as_typed(&self.query);
        if needle.is_empty() {
            return 0;
        }
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut total = 0usize;
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if let Some(n) = replace_in_file(&hit.path, needle, &self.replace, self.opts) {
                total += n;
            }
        }
        total
    }

    /// Count what Replace All WOULD do — `(occurrences, files)` — without
    /// writing anything. Feeds the confirmation modal (#123).
    pub fn count_replacements(&self, skip: &HashSet<PathBuf>) -> (usize, usize) {
        let needle = as_typed(&self.query);
        if needle.is_empty() {
            return (0, 0);
        }
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut occurrences = 0usize;
        let mut files = 0usize;
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if skip.contains(&hit.path) {
                // The confirmed action will not touch these; counting them
                // made the modal overpromise (#124 review).
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&hit.path) else {
                continue;
            };
            if let Some((_, n)) = replace_in_text(&content, needle, &self.replace, self.opts)
                && n > 0
            {
                occurrences += n;
                files += 1;
            }
        }
        (occurrences, files)
    }

    /// Replace All, skipping `skip` (files open with unsaved edits — a disk
    /// write under a dirty buffer forces the tab into conflict divergence).
    /// Returns `(occurrences replaced, the skipped hit files, the files that
    /// could not be rewritten)`. The last are named rather than dropped: a
    /// file that is not UTF-8 still shows hits (the search reads bytes) but
    /// is never replaced, and "replaced N" alone hid that.
    pub fn replace_all_skipping(
        &self,
        skip: &HashSet<PathBuf>,
    ) -> (usize, Vec<PathBuf>, Vec<PathBuf>) {
        let needle = as_typed(&self.query);
        if needle.is_empty() {
            return (0, Vec::new(), Vec::new());
        }
        let mut failed = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut total = 0usize;
        let mut skipped = Vec::new();
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if skip.contains(&hit.path) {
                skipped.push(hit.path.clone());
                continue;
            }
            match replace_in_file(&hit.path, needle, &self.replace, self.opts) {
                Some(n) => total += n,
                None => failed.push(hit.path.clone()),
            }
        }
        (total, skipped, failed)
    }
}
