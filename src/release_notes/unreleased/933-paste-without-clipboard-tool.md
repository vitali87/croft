fix: On a Linux host with no clipboard tool (wl-paste, xclip or xsel), such as a server over SSH, Ctrl+V, vim's `p` and the terminal pane's Paste now paste what you last copied, yanked or deleted in croft instead of nothing.
fix: Vim's `p` says so in the status bar when there is nothing to paste, instead of doing nothing.
