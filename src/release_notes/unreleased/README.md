# Unreleased release notes

A pull request that changes what ships adds one file here, named for the
change and starting with its issue number, for example `862-hot-exit.md`. It
holds the change's highlights, one per line, each prefixed `feature:`, `fix:`
or `refactor:`:

```text
feature: Unsaved edits survive a kill and come back on the next launch.
fix: Ctrl+Q asks before throwing away unsaved edits.
refactor: Moved the file tree's code into its own file (#2062). Nothing changes in use.
```

Use `refactor:` only when the change ships but nothing changes in use: code
moved between files, renamed, or restructured with the same behaviour.
Reviewers check that. A merge whose notes are all `refactor:` makes no version
of its own and never counts toward the release cadence: its notes wait here
for the next version that has a feature, a fix or a dependency update.

Pull requests never change `version` in `Cargo.toml` and never write
`src/release_notes/<version>.md`. After each merge, the version-bump workflow
(`.github/workflows/version-bump.yml`, running `scripts/release.py cut`) folds
every file here into the next version's notes, removes them, and bumps the
version. A change that only moves dependency versions needs no file here: the
cut adds a line naming the crates that moved. A build made while a feature
or a fix is pending shows the pending notes on its welcome card under "IN THIS
BUILD".

This README is never a note. See CONTRIBUTING.md, "Every shipped change carries
release notes".
