fix: croft opens from Croft.app or a Ghostty `command = croft` without waiting for your login shell: the PATH it read last time is used at once, and refreshed in the background for the next launch.
fix: In terminals without inline images (Terminal.app, Alacritty, tmux and others) croft no longer pauses a quarter of a second at every launch, and a sixel terminal is now recognised.
