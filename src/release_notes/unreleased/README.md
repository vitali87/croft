# Unreleased release notes

A pull request that changes what ships adds one file here, named for the
change and starting with its issue number, for example `862-hot-exit.md`. It
holds the change's highlights, one per line, each prefixed `feature:` or
`fix:`:

```text
feature: Unsaved edits survive a kill and come back on the next launch.
fix: Ctrl+Q asks before throwing away unsaved edits.
```

Pull requests never change `version` in `Cargo.toml` and never write
`src/release_notes/<version>.md`. After each merge, the version-bump workflow
(`.github/workflows/version-bump.yml`, running `scripts/release.py cut`) folds
every file here into the next version's notes, removes them, and bumps the
version. A build made while notes are pending shows them on its welcome card
under "IN THIS BUILD".

This README is never a note. See CONTRIBUTING.md, "Every shipped change carries
release notes".
