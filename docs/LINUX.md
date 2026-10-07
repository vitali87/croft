# croft on Linux

Setup notes for running croft on Linux, both locally and as the remote target of `croft remote <host>`. For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

Linux is a first-class target: a session on a Linux box over SSH behaves identically to a local Mac one, with no second-class remote mode. The Linux-specific surface is mostly getting croft onto the box and the keyboard modifier.

## The command modifier

croft mirrors VS Code's Linux convention: the command modifier is `Ctrl`, and a `Cmd` chord from the [keybindings reference](KEYBINDINGS.md) works as the same chord with `Ctrl` (`Ctrl`+`P` for Quick Open, `Ctrl`+`Shift`+`E` for the Explorer) with zero setup. The Command Palette and the Keyboard Shortcuts view spell each chord that way (`Ctrl+/`, not `Cmd+/`), and `Super` for the few chords below that have no `Ctrl` form.

That includes the `Cmd+K` chords: `Ctrl`+`K` is the leader (`Ctrl`+`K` `B` opens Testing, `Ctrl`+`K` `Ctrl`+`S` the Keyboard Shortcuts editor) everywhere except two places that keep `Ctrl`+`K` for themselves:

* the terminal pane, where it goes to the shell (readline's kill-line), and
* the editor while vim mode is on, where it kills to the end of the line.

Outside vim mode the editor's kill-to-end-of-line is the Command Palette's "Kill to End of Line"; bind `ctrl+k` to `kill_to_end_of_line` in `keybindings.json` to give the key back to it.

A handful of chords have no `Ctrl` form, or one a legacy terminal cannot send, because in a terminal it is taken or cannot be told apart from another key:

| Chord | Why not `Ctrl` | Without `Super` |
|-------|----------------|-----------------|
| `Cmd`+`\` split editor | `Ctrl`+`\` is the shell's quit signal | Command Palette "View: Split Editor" |
| `Cmd`+`Shift`+`\` / `Cmd`+`Opt`+`\` go to / select to bracket | a legacy terminal cannot send `Shift` or `Alt` with `Ctrl`+`\` | Command Palette "Go to Bracket" / "Select to Bracket" |
| `Cmd`+`Opt`+`←` / `→` focus the left / right editor group | `Ctrl`+`Alt`+arrows switch desktop workspaces | Command Palette "View: Focus Left Editor Group" / "View: Focus Right Editor Group" |
| `Cmd`+`]` / `Cmd`+`[` next / previous terminal | `Ctrl`+`[` is `Esc` | Command Palette "Terminal: Focus Next Terminal" / "Terminal: Focus Previous Terminal" |
| `Cmd`+`Shift`+`T` focus the terminal | `Ctrl`+`Shift`+`T` opens another terminal | `Ctrl`+`J` shows and focuses a hidden terminal (on a showing one it hides it: press it twice) |
| `Cmd`+`C` / `Cmd`+`W` copy the selection / close the active terminal, in the terminal pane | `Ctrl`+`C` and `Ctrl`+`W` go to the shell (interrupt, delete a word) | `Ctrl`+`Shift`+`C` / `Ctrl`+`Shift`+`W` |
| `Cmd`+`T` split terminal | `Ctrl`+`T` belongs to the shell and the editor | `Ctrl`+`Shift`+`T` |
| `Cmd`+`F12` go to implementations | `Ctrl`+`F12` is Go to Type Definition | Command Palette "Go to Implementations" |
| `Cmd`+`Z` jump to a directory with zoxide, in the Explorer | `Ctrl`+`Z` is the terminal's suspend key, kept out of the Explorer | Command Palette "Explorer: Jump to Directory (zoxide)" |
| `Cmd`+`Enter` run the Markdown code block under the caret | `Ctrl`+`Enter` runs it too where the terminal reports it (the kitty keyboard protocol, tmux `extended-keys`), but a legacy terminal sends it as a bare `Enter` | Command Palette "Markdown: Run Code Block at Cursor" |
| `Cmd`+`A` select all, in the editor | `Ctrl`+`A` is line start there, as in the shell | Command Palette "Select All" |
| `Cmd`+`E` toggle vim mode | `Ctrl`+`E` is end of line, in the editor as in the shell | Command Palette "Toggle Vim Mode" |
| `Cmd+K` chords typed in the terminal pane | `Ctrl`+`K` goes to the shell | the ones that act on the active terminal (`R`, `K`, `M`, `D`) work as `Ctrl`+`K` chords from any other pane |

`Cmd` is `Super`, and it reaches croft only over the kitty keyboard protocol: kitty, Ghostty, WezTerm, and Alacritty deliver it natively, so these chords work there. In GNOME Terminal, Konsole, xterm, or tmux, use the right-hand column, or give its palette command a chord of your own: pick the command in the Keyboard Shortcuts view (`Ctrl`+`K` `Ctrl`+`S`) and press the chord, which croft writes to `keybindings.json`.

The same protocol is what tells `Ctrl`+`Shift`+a letter from `Ctrl`+that letter: without it both send one control byte, so `Ctrl`+`Shift`+`S` arrives as `Ctrl`+`S` and saves instead of jumping to Source Control (the activity bar icon, or the palette's "View: Show Source Control", gets there).

tmux does not speak the kitty protocol, so inside it the `Shift` reaches croft only through tmux's own extended keys, and only in one setup: tmux 3.2 to 3.4 with `set -g extended-keys always`, which reports the chord as `CSI 83;6u` and so jumps to Source Control. With `extended-keys on` tmux sends extended keys only to a program that asks for xterm's modifyOtherKeys, which croft does not, and tmux 3.2 to 3.4 then drop the chord; tmux 3.5 and later send it as the plain `Ctrl`+`S` byte whatever `extended-keys` says, so there it saves.

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

`croft remote <host>` launches croft over SSH on a Linux server, installing itself on first connect with no manual prep. `<host>` comes from your `~/.ssh/config`. The install takes the first of these that works:

1. **The release binary.** If your croft came from crates.io (`cargo install croft-software`), your machine downloads that version's `croft-<target>.tar.gz` and `SHA256SUMS` from the GitHub release. If [`cosign`](https://docs.sigstore.dev/cosign/system_config/installation/) is installed, it first checks that `SHA256SUMS` was signed by croft's release workflow for that exact tag, and refuses the release if not. Without `cosign` the file is trusted over HTTPS, and the install log says the signature was not checked. It then checks the digest, caches the binary under `~/.cache/croft/prebuilt/`, and copies it over. The server needs no internet access and no toolchain. A version with no release assets skips this step.
2. **A cross-compile.** croft builds a static musl binary on your machine and copies it over. This needs the musl target installed locally (`croft setup-cross`).
3. **A build on the host.** croft compiles on the server, first installing a C toolchain and `pkg-config` with whichever package manager the box has: `apt`, `dnf`/`yum`, `apk`, `pacman`, or `zypper`.

`croft remote <host> --build` skips the release binary and builds from source. Each connect logs the path it took, and why it skipped the others, to `~/.cache/croft/install.log` on your machine.

A stock cloud image works as-is, and behaviour, keybindings, latency, and the filesystem-sync invariants are identical to a local session.

The **launching** machine needs `rsync` on its `PATH` to sync the source tree to the host. macOS and most Linux installs ship it; a stock Termux does not (`pkg install rsync`). Without it the connect fails with `running rsync to remote: spawning streaming subprocess: No such file or directory`.

Only the first connect waits for the install. Later connects attach immediately; if your local source is newer, the cross-build and ship run in the background while you work, and `Ctrl+Shift+F9` (or a click on the status bar's "Update ready" pill) relaunches into the new binary once it lands. Self-updates use a dedicated throttled SSH lane so install bytes never queue ahead of live keystrokes, keeping input latency at zero while a newer binary streams in.

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
