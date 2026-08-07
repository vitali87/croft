# croft on Windows

Short version: **run croft inside WSL2, hosted in a graphics-capable terminal (WezTerm).** Native Windows (PowerShell / `conhost`) is not supported and is unlikely to be worth supporting, for the reasons below. For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

## WSL2 works today (it is the Linux build)

croft has no separate "Windows" target. Inside WSL2 you are running real Linux, so croft's normal `x86_64-unknown-linux-gnu` build compiles and runs **unmodified**, with the full feature set: PTYs, file watching, language servers, the `Ctrl` command modifier, and the `croft remote <host>` flow. Everything in [LINUX.md](LINUX.md) applies verbatim.

```bash
# inside your WSL2 distro (Ubuntu, Debian, etc.)
cargo install --git https://github.com/vitali87/croft.git --locked
```

The one thing WSL does **not** change is the terminal. A WSL shell renders into whatever Windows terminal emulator is hosting it, and that choice decides whether you get croft's full icon/image UI or the degraded text fallback:

| Host terminal for the WSL shell | Result |
|---------------------------------|--------|
| **WezTerm** (Windows build, pointed at the WSL shell) | Full UI. WezTerm implements the iTerm2 OSC-1337 and Kitty graphics protocols croft draws with, so activity-bar icons, file icons, the hero image, and PDF / image / spreadsheet previews all render. **Recommended.** |
| **Windows Terminal** (the default WSL host) | croft runs, but image-less: icons fall back to Nerd Font glyphs and previews to a metadata-header line. croft shows its one-time "switch terminal" nudge on launch. |
| Legacy `conhost` console | Same image-less fallback, and a worse Nerd Font / truecolor story. |

So on Windows the practical setup is: WSL2 for the OS, WezTerm for the terminal, and a Nerd Font configured in WezTerm (see [LINUX.md](LINUX.md) for the font and dependency notes, which all apply inside WSL).

## Why native Windows (PowerShell / conhost) is not supported

It is tempting to read "Windows is not supported" as "nobody ported the PTY yet," but the PTY is the *least* of it. croft uses [`portable-pty`](https://crates.io/crates/portable-pty) (`Cargo.toml`), the WezTerm crate that already abstracts Windows ConPTY, so the pseudo-terminal layer is not the blocker. Three layers are, in increasing order of difficulty.

### 1. Direct POSIX syscalls (mechanical, but everywhere)

`libc` is a hard dependency and croft calls it directly on several paths, none of which exist on Windows and none of which are currently behind a `cfg(windows)` branch, so the crate does not even compile for `*-pc-windows-msvc` today:

- `src/iterm2_inline.rs` — `libc::poll` / `pollfd` / `POLLIN` for the sixel DA1 capability probe (a timed raw read of stdin).
- `src/remote.rs` — `getrlimit` / `setrlimit` on `RLIMIT_NOFILE` to raise the open-file limit before spawning PTYs, watchers, LSPs, and the cross-link.
- `src/dap/transport.rs`, `src/lsp/install.rs` — `setsid` / `getsid` to detach spawned adapters and servers into their own process group.
- `src/widgets/file_tree.rs` — `localtime_r` for mtime formatting and the `EXDEV` cross-device-rename fallback.

Porting these is busywork rather than research: each needs a Win32 equivalent (`WaitForMultipleObjects` / overlapped I/O, `SetHandleInformation` or job objects, `CreateProcess` flags, `GetDiskFreeSpaceEx`, `localtime_s`) behind a `cfg(windows)` arm. Doable, but it touches many modules.

### 2. macOS / Linux-only glue

Several integrations are written for the two supported OSes and silently no-op elsewhere:

- **Clipboard** (`src/clipboard.rs`) goes through `NSPasteboard` on macOS and `wl-copy` / `xclip` / `xsel` on Linux; the `cfg(not(any(macos, linux)))` arm returns nothing, so copy/paste would be dead on native Windows until a Win32 clipboard backend is added.
- **Cmd-chord delivery, the drag-out helper, and the `setup-iterm2` / `setup-ghostty` commands** are macOS-and-iTerm2 specific and have no Windows analogue.

### 3. The graphics model is the real wall, and it is terminal-deep, not OS-deep

This is the decisive one. croft's entire visual identity is painted with the **iTerm2 OSC-1337** inline-image protocol and the **Kitty graphics protocol** (with sixel as a runtime-probed fallback): the activity-bar icons, file-type icons, the no-repo hero illustration, the editor/welcome background, and every PDF / image / spreadsheet preview. croft assumes a terminal that speaks at least one of these.

The stock Windows console stack (`conhost`, and the Windows Terminal that fronts PowerShell) implements **none** of them. So even if every syscall above were ported and the binary ran cleanly under PowerShell, the result would be the image-less fallback: Nerd Font glyphs for icons and a metadata-header line for previews. That is exactly the degraded mode croft already warns about at startup, regardless of OS.

The only way to get the real UI on Windows is a terminal emulator that implements the protocols, which in practice means **WezTerm** (cross-platform, speaks both). And once you are running WezTerm on Windows, the natural way to give it a POSIX environment to talk to is WSL, which puts you right back at the supported Linux build. That is why the recommendation is "WSL2 + WezTerm" rather than a native Windows port: the port's best case still requires WezTerm, and WSL gets you there without rewriting the syscall and clipboard layers.

## Summary

- **Use croft on Windows via WSL2**, hosted in WezTerm, with a Nerd Font configured. This is fully supported as the Linux build.
- **Native PowerShell / conhost is not supported** and is not on the roadmap: it would require Windows branches for every `libc` call plus a Win32 clipboard backend, and even then the stock Windows console cannot render croft's graphics, so the payoff is capped at the image-less fallback unless you run WezTerm anyway.
