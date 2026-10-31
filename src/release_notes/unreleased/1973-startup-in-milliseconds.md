fix: croft opens from Croft.app or a Ghostty `command = croft` without waiting for your login shell: the PATH it read last time is used at once, and refreshed in the background for the next launch.
fix: In Ghostty, kitty, WezTerm and iTerm2 the first frame no longer waits for the activity-bar icons and the welcome logo to be drawn; they appear a moment later.
fix: Opening croft in a git repository no longer freezes input while it looks up the default branch.
