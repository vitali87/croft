fix: Scrub History, the TIMELINE diff and the merge editor read a Git LFS file (or any file with a clean/smudge filter) through its filter, so past versions show the file instead of the LFS pointer.
