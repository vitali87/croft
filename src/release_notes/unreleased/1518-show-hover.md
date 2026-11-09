feature: Show or Focus Hover (`Ctrl+K Ctrl+I`, `Cmd+K Cmd+I` on macOS) opens the hover for the symbol at the caret from the keyboard: its type and docs, and the full message of a problem there. It is in the Command Palette, and `croft import-vscode` carries over a chord bound to VS Code's `editor.action.showHover`.
fix: `Ctrl+K Ctrl+I` in the editor no longer toggles Broadcast Input; `Ctrl+K I` still does.
fix: Hovering a word with a problem in a file no language server has shows the problem's message.
