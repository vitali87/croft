fix: On Linux, Ctrl+K starts the Cmd+K chords (Ctrl+K B opens Testing) everywhere except the terminal pane and vim mode, and "Kill to End of Line" is in the Command Palette (#843).
fix: On Linux, Ctrl+Enter runs the Markdown code block under the caret, as Cmd+Enter does, and the Command Palette's "Markdown: Run Code Block at Cursor" runs it on any terminal (#843).
fix: The Command Palette has "Select All", the Linux route to Cmd+A in the editor, where Ctrl+A is line start (#843).
fix: The Command Palette has "View: Focus Left Editor Group", "View: Focus Right Editor Group", "Terminal: Focus Next Terminal" and "Terminal: Focus Previous Terminal", the Linux routes to Cmd+Opt+Left / Right and Cmd+] / Cmd+[ (#843).
fix: The Command Palette has "Go to Implementations" and "Explorer: Jump to Directory (zoxide)", the Linux routes to Cmd+F12 and the Explorer's Cmd+Z, and LINUX.md lists every chord that needs Super there with its route (#843).
fix: Off macOS the Command Palette and the Keyboard Shortcuts view show chords as Ctrl+… (Ctrl+/, not Cmd+/), and as Super+… for the few with no Ctrl form, such as Super+E for Toggle Vim Mode (#843).
