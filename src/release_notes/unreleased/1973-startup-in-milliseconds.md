fix: croft opens from Croft.app or a Ghostty `command = croft` without waiting for your login shell: the PATH it read last time is used at once, and refreshed in the background for the next launch.
fix: In Ghostty, kitty, WezTerm and iTerm2 the first frame no longer waits for the activity-bar icons and the welcome logo to be drawn; they appear a moment later.
fix: In terminals without inline images (Terminal.app, Alacritty, tmux and others) croft no longer pauses a quarter of a second at every launch, and a sixel terminal is now recognised.
