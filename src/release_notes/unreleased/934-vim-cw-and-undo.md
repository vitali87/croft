fix: In vim mode, `cw` changes a word up to its end and keeps the space after it, as in Vim, instead of deleting the space too.
fix: In vim mode, one `u` undoes a whole change with the text typed after it (`cw`, `ciw`, `cc`, `o`, `i` up to `Esc`), where it used to take back only the typed text.
fix: In vim mode, `.` and `>>` / `<<` say in the status bar that they are not supported, instead of doing nothing.
