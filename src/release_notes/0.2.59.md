feature: Go files get gopls without a manual install: when `go` is on PATH and gopls is not, croft builds a pinned gopls with your own `go install`.
fix: A file whose language has no language server on PATH (Lua without lua-language-server, Go without gopls or go) now says so once in the status bar and OUTPUT › Server Provisioning instead of silently getting no diagnostics or completion.
