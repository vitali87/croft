fix: `croft import-vscode | head` and `croft theme-import … | head` write the import before printing about it, and a reader that stops early ends croft quietly instead of with a "Broken pipe" panic.
fix: `croft theme-import --dry-run` prints only the manifest on stdout; its progress lines go to stderr.
