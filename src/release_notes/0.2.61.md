fix: The Debug Console shows a line the program printed in pieces as one line, keeps the blank lines from an empty `print()`, and redraws a progress bar in place instead of stacking a copy per update.
fix: A Debug Console REPL result that spans several lines, such as a traceback, shows every line, so the `KeyError` at the end is no longer cut off.
