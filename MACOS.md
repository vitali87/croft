# croft on macOS

Setup notes for running croft on macOS. For what croft does and the full keyboard surface see the [README](README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

macOS is croft's primary local platform: the clipboard, inline previews, and "Reveal in Finder" all use native AppKit paths here. The one thing macOS needs that other platforms don't is a small amount of terminal setup, because macOS reserves the `Cmd` modifier for application menus.

## Nerd Font

Explorer icons and the activity bar are Private Use Area glyphs (Codicons, Devicons, Seti). Without a Nerd Font they render as `[?]` boxes.

```bash
brew install --cask font-meslo-lg-nerd-font
croft setup-terminal   # sets Terminal.app's default profile font to MesloLGS NF 13pt
```

Quit Terminal.app entirely (`Cmd`+`Q`) and reopen for the font to take effect.

macOS Terminal.app does **not** fall back to a Nerd Font for Private Use Area glyphs the way iTerm2 does, so the *primary* font must be a Nerd Font. To set it by hand: Terminal.app → Settings → Profiles → Text → Font → MesloLGS Nerd Font Mono Regular 13pt.

iTerm2, kitty, WezTerm, and Ghostty all fall back to a Nerd Font for PUA glyphs automatically, so if your primary font is not a Nerd Font the icons still render correctly there as long as one is installed on the system.

## The `Cmd` modifier and your terminal

macOS reserves `Cmd` for application menus, so `Cmd` chords need one extra step depending on which terminal you use. Every `Cmd` chord also has a zero-setup `Ctrl` equivalent (see [KEYBINDINGS.md](KEYBINDINGS.md)), so this step is optional but recommended if you want the VS Code muscle memory.

### iTerm2

Run the setup command once after installing:

```bash
croft setup-iterm2
```

This installs croft's `Cmd` chords as CSI-u key forwarders and relocates the conflicting iTerm2 / macOS menu shortcuts to unused alternates so their original actions stay reachable. Then enable right-click forwarding so croft's context menu works:

1. iTerm2 → Settings (`⌘,`) → search **"right click"**.
2. Tick **"Right click reported to apps, does not open menu"**.
3. Fully quit iTerm2 (`⌘Q`) and reopen.

The full chord-by-chord mapping (and the hand-mapping recipe if you skip the setup command) is in [KEYBINDINGS.md → iTerm2 key mappings](KEYBINDINGS.md#iterm2-key-mappings). `⌘V` is deliberately left on iTerm2's native Paste; croft reads the system clipboard and routes it by focus, so paste works identically over SSH.

### Ghostty

Ghostty resolves its own keybinds (`new_tab`, `goto_tab`, ...) before handing a key to croft, so by default `⌘T` opens a Ghostty tab instead of reaching croft. Run:

```bash
croft setup-ghostty
```

This adds a managed `keybind` block to your Ghostty config that re-emits every croft chord as the same CSI-u sequence iTerm2 forwards. Reload the config (`⌘⇧,`) or restart Ghostty afterwards. See [KEYBINDINGS.md → Ghostty key mappings](KEYBINDINGS.md#ghostty-key-mappings).

### kitty and WezTerm

Both deliver `Cmd` over the kitty keyboard protocol natively, so nothing is needed there.

## Inline previews

iTerm2 renders inline image, PDF, and spreadsheet previews via OSC 1337; kitty and Ghostty use the Kitty graphics protocol; sixel-capable terminals (detected at startup via a DA1 probe) use DEC sixel. Other terminals fall back to a metadata header line.

Multi-page PDF preview needs `pdftoppm` from poppler:

```bash
brew install poppler
```

Without it, croft falls back to macOS `sips`, which renders page 1 only.

## Native integrations

- **Clipboard.** Copy and paste route through the native macOS pasteboard (`NSPasteboard`), with OSC 52 used only as a remote-host fallback, so copy/paste behaves identically local and over SSH.
- **Reveal in Finder.** In the Explorer, `Cmd`+`Opt`+`R` (or the right-click menu) reveals the selected entry in Finder. This is macOS-only.
