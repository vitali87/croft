fix: Before compiling croft on a remote, `croft remote` says that it may install Rust with rustup (about 1 GB) and a C toolchain and dtach with the package manager.
fix: A remote compile no longer edits the remote user's ~/.bashrc and ~/.profile, and never waits on a sudo password prompt.
