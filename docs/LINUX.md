# croft on Linux

Setup notes for running croft on Linux, both locally and as the remote target of `croft remote <host>`. For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

Linux is a first-class target: a session on a Linux box over SSH behaves identically to a local Mac one, with no second-class remote mode. The Linux-specific surface is mostly getting croft onto the box and the keyboard modifier.

## The command modifier

croft mirrors VS Code's Linux convention: the command modifier is `Ctrl`, and every chord works as the same chord with `Ctrl` (`Ctrl`+`P` for Quick Open, `Ctrl`+`Shift`+`E` for the Explorer) with zero setup. kitty, Ghostty, WezTerm, and Alacritty also deliver `Cmd`/`Super` over the kitty keyboard protocol natively, so the `Cmd` chords from the [keybindings reference](KEYBINDINGS.md) reach croft there too if you prefer them.

## Nerd Font

Explorer icons and the activity bar are Private Use Area Nerd Font glyphs (Codicons plus file-type icons); without a Nerd Font they render as `[?]` boxes. Install one and set it as your terminal font:

```bash
# Example: Meslo, the family croft uses elsewhere
mkdir -p ~/.local/share/fonts
cd ~/.local/share/fonts
curl -fLO https://github.com/ryanoasis/nerd-fonts/releases/latest/download/Meslo.zip
unzip -o Meslo.zip && fc-cache -f
```

Then select "MesloLGS Nerd Font Mono" (or any Nerd Font) as your terminal profile's font. kitty, Ghostty, WezTerm, and most modern terminals fall back to a Nerd Font for PUA glyphs automatically once one is installed.

## Inline previews

kitty and Ghostty render inline image, PDF, and spreadsheet previews via the Kitty graphics protocol; sixel-capable terminals (detected at startup via a DA1 probe) use DEC sixel. Other terminals fall back to a metadata header line.

Multi-page PDF preview needs `pdftoppm` from poppler-utils:

```bash
sudo apt install poppler-utils      # Debian / Ubuntu
sudo dnf install poppler-utils      # Fedora
sudo pacman -S poppler              # Arch
```

## Language servers

croft auto-provisions `rust-analyzer` on first use, downloading the official release binary into `~/.croft/servers`; a copy on your `PATH` or in `~/.cargo/bin` wins, so it matches your toolchain. It picks up `gopls` from `PATH`, auto-installs `vtsls` for TypeScript / JavaScript on first use where `node` + `npm` are present, and provisions the Python servers (`ty`, `ruff`) via `uv`. You can still install distro packages — a PATH copy takes precedence:

```bash
sudo apt install rust-analyzer gopls nodejs npm   # adjust per distro; rust-analyzer optional
```

## Remote: `croft remote <host>`

`croft remote <host>` launches croft over SSH on a Linux server, installing itself on first connect with no manual prep. `<host>` comes from your `~/.ssh/config`. The install takes one of two paths:

1. It cross-compiles a static musl binary on your Mac and copies it over (fastest path; needs the musl target installed locally).
2. Failing that, it compiles on the host, provisioning a C toolchain and `pkg-config` across whichever package manager the box has: `apt`, `dnf`/`yum`, `apk`, `pacman`, or `zypper`.

A stock cloud image works as-is, and behaviour, keybindings, latency, and the filesystem-sync invariants are identical to a local session.

The **launching** machine needs `rsync` on its `PATH` to sync the source tree to the host. macOS and most Linux installs ship it; a stock Termux does not (`pkg install rsync`). Without it the connect fails with `running rsync to remote: spawning streaming subprocess: No such file or directory`.

Only the first connect waits for the install. Later connects attach immediately; if your local source is newer, the cross-build and ship run in the background while you work, and `F9` reloads into the new binary once it lands. Self-updates use a dedicated throttled SSH lane so install bytes never queue ahead of live keystrokes, keeping input latency at zero while a newer binary streams in.

### "Background croft update failed; staying on current version"

This is a refusal, not a crash. It happens when you are already attached to the remote croft *and* the fast cross-build path is unavailable. The only route left would be compiling on the box you are typing into, so croft declines rather than spend its cores on an unrequested build. You stay on the running binary; nothing is half-installed.

The reason is logged on the **launching** machine — the one you connected *from*, not the server in the message. Read it **before** reconnecting: each connect truncates the file.

```bash
tail ~/.cache/croft/install.log
```

Look for the missing piece. Every line carries a Unix timestamp:

```
[1788730108] Local cross-build skipped: rustup target `x86_64-unknown-linux-musl` missing (run `rustup target add x86_64-unknown-linux-musl` once to enable the fast path)
[1788730108] Update NOT installed: cross-build unavailable (rustup target `x86_64-unknown-linux-musl` missing (…)). Run `croft setup-cross` on this machine, then reconnect — updates then ship a prebuilt binary in seconds.
```

The prerequisites are `zig`, `cargo-zigbuild`, and both musl rustup targets (`x86_64-` and `aarch64-unknown-linux-musl`). Install them on the launching machine:

```bash
croft setup-cross          # prints a plan, then asks to confirm
croft setup-cross --yes    # skip the prompt
```

It is idempotent and reports what it skips. Answering anything but `y` prints `Aborted.` and exits 0, so read the output rather than the exit code. Then **reconnect** — a declined update is not retried on the running session.

Two things catch people out. Rustup targets are per-toolchain, and this repo pins its channel in `rust-toolchain.toml`, so `rustup target list --installed` only answers for the pinned toolchain when run from a croft checkout; a channel bump orphans every target you added. And connecting with the dialog still up — no session attached — asks whether to compile on the host instead, since there is no live session to disturb.

### Surviving sleep and network drops

A remote session runs under [`dtach`](https://github.com/crigler/dtach), so closing your laptop or changing networks no longer kills it. When the SSH transport dies (its keepalive gives up after ~30s of no response), croft keeps running on the host inside its dtach session; croft auto-reconnects (showing `Reconnecting to <host>…`, Ctrl+C to stop) and reattaches with your tabs, layout, and terminals intact.

dtach is used rather than tmux because it is transparent to the byte stream: croft's inline images (iTerm2 OSC-1337 and the Kitty graphics protocol used by Ghostty/Kitty/WezTerm) pass through untouched, whereas tmux corrupts the Kitty protocol. The session is launched with `dtach -A -E -z -r winch`, so dtach never steals croft's `Ctrl` chords and fires a redraw on reattach.

The session name is keyed to the workspace path, so reconnecting to the same directory resumes the same session. The from-source install path provisions dtach automatically; on a host installed via the fast cross-build path, install it once (`sudo apt install dtach`, or your package manager's equivalent). Without dtach, croft shows an orange `⚠ Persistence off: install dtach` badge on its status line for the whole session, so you know a transport drop will end it.

Because the session name is keyed to the workspace path and not to the SSH connection, a second connection to the **same host and same directory** does not start a fresh croft: it attaches to the running one. dtach allows several clients on one socket, so both mirror a single process. Connect from your phone, raise the on-screen keyboard, then connect from your laptop to the same directory, and the laptop sees that process with the keyboard still up — the same persistence machinery, with two clients attached at once rather than one reconnecting after a drop. The most recently attached client drives the terminal dimensions (dtach sizes the PTY to the latest attacher and fires the `winch` redraw). For an independent session on the same host, open a different workspace path — it hashes to a different socket. To take sole control, disconnect the other client.
