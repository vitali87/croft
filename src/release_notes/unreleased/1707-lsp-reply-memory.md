fix: croft reads each reply from a language server once instead of three times. Opening a big file no longer spikes tens of MB (a 1.9 MB Rust file with rust-analyzer: peak 146 MB, now 102 MB), and on Linux hosts (`croft remote`) memory afterwards is about half what it was.
fix: On Ghostty and kitty, croft no longer copies the whole screen after every frame and keeps that copy for nothing.
