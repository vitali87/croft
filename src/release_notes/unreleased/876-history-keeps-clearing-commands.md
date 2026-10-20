fix: Commands that clear the screen, such as `clear; make` or `reset; cargo test`, are now kept in the command history (Ctrl+Shift+H), and their build errors reach PROBLEMS.
fix: A command that clears the screen is no longer recorded in the history as the command run before it.
