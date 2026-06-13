# croft on Linux

Setup notes for running croft on Linux, both locally and as the remote target of `croft remote <host>`. For what croft does and the full keyboard surface see the [README](README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

Linux is a first-class target: behaviour on a Linux box over SSH is identical to the local Mac, with no second-class remote mode. Most of croft's Linux-specific surface is about getting it onto the box (the remote launcher) and the keyboard modifier.

## The command modifier

croft mirrors VS Code's Linux convention, where the command modifier is `Ctrl`. Every chord works as the same chord with `Ctrl` (e.g. `Ctrl`+`P` for Quick Open, `Ctrl`+`Shift`+`E` for the Explorer) with zero setup. kitty, Ghostty, WezTerm, and Alacritty additionally deliver `Cmd`/`Super` over the kitty keyboard protocol natively, so the `Cmd` chords from the [keybindings reference](KEYBINDINGS.md) also reach croft under those terminals if you prefer them.

## Nerd Font

Explorer icons and the activity bar are Private Use Area glyphs (Codicons, Devicons, Seti). Without a Nerd Font they render as `[?]` boxes. Install one and set it as your terminal font:

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

croft picks up `rust-analyzer` and `gopls` from your `PATH`. For the TypeScript / JavaScript server it auto-installs `vtsls` on first use as long as `node` + `npm` are present, and it provisions the Python servers (`ty`, `ruff`) itself via `uv`. Install whatever your distro provides for the languages you use:

```bash
sudo apt install rust-analyzer gopls nodejs npm   # adjust per distro
```

## Remote: `croft remote <host>`

This is the most common way Linux comes into play, even from a Mac. `croft remote <host>` (with `<host>` from your `~/.ssh/config`) launches croft over SSH on a Linux server, and installs itself on the box on first connect with no manual prep:

1. It cross-compiles a static musl binary on your Mac and copies it over (fastest path; needs the musl target installed locally).
2. Failing that, it falls back to compiling on the host, provisioning a C toolchain and `pkg-config` across whichever package manager the box has: `apt`, `dnf`/`yum`, `apk`, `pacman`, or `zypper`.

A stock cloud image works out of the box. Behaviour, keybindings, latency, and the filesystem-sync invariants are all identical to the local session.

Background self-updates use a dedicated throttled SSH lane so install bytes never queue ahead of live keystrokes, keeping input latency at zero even while a newer binary streams in.
