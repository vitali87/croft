# croft on Linux

Setup notes for running croft on Linux, both locally and as the remote target of `croft remote <host>`. For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

Linux is a first-class target: behaviour on a Linux box over SSH is identical to the local Mac, with no second-class remote mode. Most of croft's Linux-specific surface is about getting it onto the box (the remote launcher) and the keyboard modifier.

## The command modifier

croft mirrors VS Code's Linux convention, where the command modifier is `Ctrl`. Every chord works as the same chord with `Ctrl` (e.g. `Ctrl`+`P` for Quick Open, `Ctrl`+`Shift`+`E` for the Explorer) with zero setup. kitty, Ghostty, WezTerm, and Alacritty additionally deliver `Cmd`/`Super` over the kitty keyboard protocol natively, so the `Cmd` chords from the [keybindings reference](KEYBINDINGS.md) also reach croft under those terminals if you prefer them.

## Nerd Font

Explorer icons and the activity bar are Private Use Area Nerd Font glyphs (Codicons plus file-type icons). Without a Nerd Font they render as `[?]` boxes. Install one and set it as your terminal font:

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

croft auto-provisions `rust-analyzer` on first use (it downloads the official release binary into `~/.croft/servers`), but a copy already on your `PATH` or in `~/.cargo/bin` wins so it matches your toolchain. It picks up `gopls` from your `PATH`. For the TypeScript / JavaScript server it auto-installs `vtsls` on first use as long as `node` + `npm` are present, and it provisions the Python servers (`ty`, `ruff`) itself via `uv`. You can still install distro packages for the languages you use (a PATH copy takes precedence):

```bash
sudo apt install rust-analyzer gopls nodejs npm   # adjust per distro; rust-analyzer optional
```

## Remote: `croft remote <host>`

This is the most common way Linux comes into play, even from a Mac. `croft remote <host>` (with `<host>` from your `~/.ssh/config`) launches croft over SSH on a Linux server, and installs itself on the box on first connect with no manual prep:

1. It cross-compiles a static musl binary on your Mac and copies it over (fastest path; needs the musl target installed locally).
2. Failing that, it falls back to compiling on the host, provisioning a C toolchain and `pkg-config` across whichever package manager the box has: `apt`, `dnf`/`yum`, `apk`, `pacman`, or `zypper`.

A stock cloud image works out of the box. Behaviour, keybindings, latency, and the filesystem-sync invariants are all identical to the local session.

The **launching** machine needs `rsync` on its `PATH` — croft uses it to sync the source tree to the host. macOS and most Linux installs ship it already; a stock Termux does not (`pkg install rsync`). Without it the connect fails with `running rsync to remote: spawning streaming subprocess: No such file or directory`.

Background self-updates use a dedicated throttled SSH lane so install bytes never queue ahead of live keystrokes, keeping input latency at zero even while a newer binary streams in.

### Surviving sleep and network drops

A remote session is launched under [`dtach`](https://github.com/crigler/dtach), so closing your laptop or changing networks no longer kills it. When the SSH transport dies (its keepalive gives up after ~30s of no response), the croft process keeps running on the host inside its dtach session; croft auto-reconnects (showing `Reconnecting to <host>…`, Ctrl+C to stop) and reattaches with your tabs, layout, and terminals intact.

dtach is used rather than tmux because it is transparent to the byte stream: croft's inline images (iTerm2 OSC-1337 and the Kitty graphics protocol used by Ghostty/Kitty/WezTerm) pass through untouched, whereas tmux corrupts the Kitty protocol. The session is launched with `dtach -A -E -z -r winch`, so dtach never steals croft's `Ctrl` chords and fires a redraw on reattach.

The session name is keyed to the workspace path, so reconnecting to the same directory resumes the same session. The from-source install path provisions dtach automatically; on a host that installed via the fast cross-build path, install it once (`sudo apt install dtach`, or your package manager's equivalent) to enable persistence. When a remote session is running without dtach, croft shows an orange `⚠ Persistence off: install dtach` badge on its bottom status line for the whole session, so you know a transport drop will end it as before.

Because the session name is keyed to the workspace path and not to the SSH connection, a second connection to the **same host and same directory** opened while the first is still live does not start a fresh croft: it attaches to the running one. dtach allows several clients on one socket at once, so both connections mirror a single croft process. If you connect from your phone, raise the on-screen keyboard, and then connect from your laptop to the same directory, the laptop sees that same process with the on-screen keyboard still up. This is the same persistence machinery, just with two clients attached simultaneously rather than one reconnecting after a drop. The most recently attached client drives the terminal dimensions (dtach sizes the PTY to the latest attacher and fires the `winch` redraw). To run a genuinely independent session on the same host, open a different workspace path (it hashes to a different socket); to take sole control, disconnect the other client.
