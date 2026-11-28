# croft on macOS

Setup notes for running croft on macOS. For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

- macOS is croft's primary local platform. The clipboard, inline previews and "Reveal in Finder" all use native AppKit paths.
- It is also the one platform needing terminal setup, because macOS reserves `Cmd` for application menus.

## Nerd Font

Explorer icons and the activity bar are Private Use Area Nerd Font glyphs (Codicons plus file-type icons). Without a Nerd Font they render as `[?]` boxes.

```bash
brew install --cask font-meslo-lg-nerd-font
croft setup-terminal   # sets Terminal.app's default profile font to MesloLGS NF 13pt
```

Quit Terminal.app entirely (`Cmd`+`Q`) and reopen for the font to take effect.

- **Terminal.app** does **not** fall back to a Nerd Font for Private Use Area glyphs the way iTerm2 does, so its *primary* font must be a Nerd Font.
- To set it by hand: Terminal.app → Settings → Profiles → Text → Font → MesloLGS Nerd Font Mono Regular 13pt.
- **iTerm2, kitty, WezTerm and Ghostty** fall back automatically. Icons render whatever your primary font, as long as a Nerd Font is installed.

## The `Cmd` modifier and your terminal

macOS reserves `Cmd` for application menus, so `Cmd` chords need one extra step that depends on your terminal.

- Optional: every `Cmd` chord also has a zero-setup `Ctrl` equivalent (see [KEYBINDINGS.md](KEYBINDINGS.md)).
- Recommended if you want the VS Code muscle memory.

### iTerm2

Run the setup command once after installing:

```bash
croft setup-iterm2
```

- It installs croft's `Cmd` chords as CSI-u key forwarders.
- It relocates conflicting iTerm2 / macOS menu shortcuts to unused alternates, so their original actions stay reachable.

Then enable right-click forwarding so croft's context menu works:

1. iTerm2 → Settings (`⌘,`) → search **"right click"**.
2. Tick **"Right click reported to apps, does not open menu"**.
3. Fully quit iTerm2 (`⌘Q`) and reopen.

- The chord-by-chord mapping, and the hand-mapping recipe if you skip the setup command, is in [KEYBINDINGS.md → iTerm2 key mappings](KEYBINDINGS.md#iterm2-key-mappings).
- `⌘V` is deliberately left on iTerm2's native Paste. croft reads the system clipboard and routes it by focus, so paste works identically over SSH.

### Ghostty

Ghostty resolves its own keybinds (`new_tab`, `goto_tab`, ...) before handing a key to croft, so by default `⌘T` opens a Ghostty tab instead. Run:

```bash
croft setup-ghostty
```

- It adds a managed `keybind` block to your Ghostty config that re-emits every croft chord as the same CSI-u sequence iTerm2 forwards.
- Reload the config (`⌘⇧,`) or restart Ghostty afterwards.
- See [KEYBINDINGS.md → Ghostty key mappings](KEYBINDINGS.md#ghostty-key-mappings).

### kitty and WezTerm

Both deliver `Cmd` over the kitty keyboard protocol natively, so nothing is needed there.

## One-click launcher (Croft.app)

To open croft without typing anything, create a clickable launcher:

```bash
croft install-launcher                 # opens croft at your home folder
croft install-launcher --path ~/Documents   # opens croft at a specific folder
croft install-launcher --user          # install to ~/Applications (no admin rights)
```

- It builds a `Croft.app` bundle (croft's logo as its icon) in `/Applications`.
- Reach it from Spotlight (`⌘Space`, type "Croft"), Launchpad, or the Dock.
- Clicking it opens a fresh Ghostty window with croft running in the chosen folder.
- Re-run it any time to point the launcher at a different folder.

How it launches: `open -na Ghostty.app --args --initial-command="croft <dir>"`.

- `--initial-command` sets the command for only that launch's first window. Your normal Ghostty windows stay a plain shell and your config is untouched.
- Unlike Ghostty's `-e` flag, it does not trigger the macOS "Allow Ghostty to Execute" prompt.

### Opening files with it

`Croft.app` also accepts documents. Drop a file on the app, or right-click it and pick Croft under **Open With**.

- The workspace roots itself at the file's folder and the file opens in the editor, the same as `croft <file>` on the command line.
- An opened document fills the window, with the Explorer and terminal hidden: double-clicking a file means "show me this file", not "give me an IDE".
- Bring either back with `Cmd+B` and `Cmd+J`.
- Clicking the launcher itself keeps the normal layout.

To make Croft the permanent handler for a file type: select a file in Finder, press `⌘I`, then under **Open With** choose Croft and click **Change All**.

- Do this in Finder, not with `duti`.
- File types no installed app declares (`.tex` is a common one) get a *dynamic* type identifier, and LaunchServices rejects `duti -s` for those with `error -50` whatever app you name.

The launcher is an AppleScript applet, not a shell script. macOS hands a double-clicked document to an app as an Apple Event, which a `#!/bin/sh` bundle executable cannot receive: it would launch with empty arguments and silently open the default folder instead of the file.

## Inline previews

| Terminal | Image, PDF and spreadsheet previews |
|----------|-------------------------------------|
| iTerm2 | OSC 1337 |
| kitty, Ghostty | Kitty graphics protocol |
| Sixel-capable terminals (detected at startup via a DA1 probe) | DEC sixel |
| Others | Image and PDF previews drawn with half-block characters, two pixels a cell, under a metadata header line |

Multi-page PDF preview (with clickable links, via `pdftohtml`) needs poppler:

```bash
brew install poppler
```

Without it, croft falls back to macOS `sips`, which renders page 1 only.

## Native integrations

- **Clipboard.** Copy and paste go through the native macOS pasteboard (`NSPasteboard`). OSC 52 is only a remote-host fallback, so copy/paste behaves identically local and over SSH.
- **Reveal in Finder.** In the Explorer, `Cmd`+`Opt`+`R` (or the right-click menu) reveals the selected entry in Finder. macOS-only.

## Spotlight indexing and the build directory

Building any Rust project on macOS gives Spotlight (`mds_stores`) a lot of work.

- Cargo writes thousands of small files into the build directory on every build, mostly under `target/debug/.fingerprint/` and `target/debug/incremental/`.
- Spotlight treats each write as a filesystem event and re-indexes continuously, pinning a CPU core and spinning your fans up.
- This is a general Rust-on-macOS issue ([rust-lang/cargo#8684](https://github.com/rust-lang/cargo/issues/8684)), not croft-specific, but you will meet it whenever you build from source.

The reliable fix: send Cargo's output to a directory whose name ends in `.noindex`. Spotlight skips any folder with that suffix and everything inside it. Add this to your shell profile (`~/.zshrc`):

```bash
export CARGO_TARGET_DIR=target.noindex
```

- The value is relative, so Cargo resolves it per project. Each builds into its own `target.noindex/` beside its `Cargo.toml`, and projects stay isolated.
- Keep that directory out of Git with your global ignore file:

```bash
echo 'target.noindex/' >> ~/.config/git/ignore
```

Open a new terminal (or run the `export` in your current one) and rebuild.

- The installed binary still lands in `~/.cargo/bin/croft`. Only the intermediate build artifacts move, so `croft` stays a global command.
- If you already built into a plain `target/`, its files are still in the Spotlight index. Clear them by rebuilding the index once:

```bash
sudo mdutil -E /
```

Approaches that do **not** work on current macOS, so you can skip them:

* A `.metadata_never_index` file inside the build directory. That marker only takes effect at a volume root, not a subdirectory, so it is ignored for `target/`.
* `mdutil -i off <dir>`. `mdutil` acts per volume, not per directory, so this turns Spotlight off for your whole disk.
* The System Settings Spotlight Privacy list does work, but it has no clean command line interface (the backing file is protected by SIP). The `.noindex` directory above is the simplest dependable option.
