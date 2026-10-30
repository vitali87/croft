fix: Local history no longer copies files over 256 KB on every save, as VS Code's does not: saving a 20 MB dump used to add another 20 MB to the history each time, up to about 1 GB per file.
