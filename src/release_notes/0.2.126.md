fix: Emmet understands the numbering modifiers `$@-` (count down), `$@N` (start at N) and `$@-N`, so `ol>li{Step $@-}*3` gives Step 3, 2, 1 and `li.item$@-*3` expands instead of doing nothing.
