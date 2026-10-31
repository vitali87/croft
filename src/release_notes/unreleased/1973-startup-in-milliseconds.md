fix: croft opens from Croft.app or a Ghostty `command = croft` without waiting for your login shell: the PATH it read last time is used at once, and refreshed in the background for the next launch.
