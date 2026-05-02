# terminal-vscode

A VS Code style three pane workspace that runs entirely inside your terminal.

* **Left pane:** file explorer (Nerd Font icons, click to open)
* **Top right pane:** code editor with syntax highlighting
* **Bottom right pane:** a real interactive shell (your `$SHELL` running on a PTY)

Built on [Textual](https://textual.textualize.io/) and [pyte](https://github.com/selectel/pyte).

## Requirements

| Requirement | Why |
|-------------|-----|
| macOS or Linux | Uses POSIX `pty.fork` for the embedded terminal. Windows is not supported. |
| Python 3.12+ | Required by the project. |
| [uv](https://docs.astral.sh/uv/) | Package manager and runner. |
| A [Nerd Font](https://www.nerdfonts.com/) in your terminal | Needed for file icons in the explorer. Without one, icons appear as boxes or question marks. |
| A 256 color or truecolor terminal | iTerm2, Alacritty, kitty, WezTerm, Ghostty, modern Terminal.app are all fine. |

### Install uv

```bash
curl -LsSf https://astral.sh/uv/install.sh | sh
```

### Install a Nerd Font (macOS)

**Use this exact recipe for macOS Terminal.app.** It is the only one that reliably renders all file type icons in the native Terminal.app on recent macOS:

```bash
brew install --cask font-symbols-only-nerd-font
brew install --cask font-meslo-lg-nerd-font
```

Then in Terminal.app: `Settings → Profiles → your profile → Text → Font → Change → MesloLGS Nerd Font (Regular, 13pt)`. Quit Terminal.app entirely and reopen it so the font cache picks up the new fonts.

**Why both fonts:**

* `font-meslo-lg-nerd-font` (a.k.a. MesloLGS NF) is the Nerd Font with the most reliable glyph coverage in Terminal.app. JetBrainsMono Nerd Font has a known bug where its Devicons (file type icons in the U+E700 range) do not render in Terminal.app even when correctly selected (see [JetBrains/JetBrainsMono#732](https://github.com/JetBrains/JetBrainsMono/issues/732)).
* `font-symbols-only-nerd-font` is a glyphs only font that macOS CoreText uses as a system wide fallback. If any glyph is missing in your primary font, macOS will pick it up from this one automatically. This makes the icons robust even if you switch your terminal font later.

If you prefer a different look, any other Nerd Font from [nerdfonts.com](https://www.nerdfonts.com/font-downloads) works as long as you also keep `font-symbols-only-nerd-font` installed as the fallback.

## Install the app

```bash
git clone <your fork or this repo> terminal-vscode
cd terminal-vscode
uv sync
```

That single `uv sync` reads `pyproject.toml`, creates a `.venv`, and installs every dependency (Textual, pyte, the tree sitter language packs for syntax highlighting, etc.).

## Run the app

```bash
uv run tcode               # opens the current directory as the workspace
uv run tcode ~/projects    # opens a specific folder
uv run tcode --help        # show CLI options
```

The first time you run it, you should see the file explorer on the left, an empty editor on the top right, and your shell prompt at the bottom right.

## Keybindings

| Keys | Action |
|------|--------|
| `enter` on a file in the tree | Open the file in the editor |
| `ctrl+s` | Save the open file |
| `ctrl+q` | Quit the app |
| `f6` | Cycle focus across the three panes (tree, editor, terminal) |
| `ctrl+b` | Toggle the file tree on or off |
| Arrow keys, page up or down, etc. | Standard editor navigation when the editor is focused; forwarded to the shell when the terminal is focused |
| `ctrl+letter` | When the terminal is focused, sent through to the shell as a control character (e.g. `ctrl+c` interrupts) |

## Supported file types for syntax highlighting

Bash, CSS, Go, HTML, Java, JavaScript, JSON, Markdown, Python, regex, Rust, SQL, TOML, XML, YAML.

Files in other languages still open and are editable; they just render in plain text.

## Project layout

```
src/terminal_vscode/
├── app.py              # Textual app, three pane composition, key bindings
├── cli.py              # typer entry point exposed as `tcode`
├── constants.py        # IDs, file extension to language map, status messages
├── icons.py            # Nerd Font glyphs and colors per file type
├── styles.tcss         # Textual CSS for the three pane layout
└── widgets/
    ├── editor.py       # TextArea subclass with file load and save
    ├── file_tree.py    # DirectoryTree subclass that renders icons
    └── terminal.py     # Custom widget: pty.fork, pyte screen, key forwarding
```

## How the embedded terminal works

The terminal pane is not a fake or read only log. On mount it calls `pty.fork()`, which spawns your shell in a child process whose stdin, stdout, and stderr are wired to a pseudoterminal. The parent side keeps the master file descriptor.

* Reads from the master fd are registered with asyncio via `add_reader`. Whenever the shell produces output, the bytes are fed to a `pyte.ByteStream`, which updates a `pyte.Screen` in memory.
* The widget's `render` method walks that screen, builds a Rich `Text` with the right foreground, background, bold, italic, underline, and reverse styles per cell, and Textual paints it.
* Key events from Textual are translated back to the byte sequences that real terminals send (arrow keys to `\x1b[A` etc., control letters to their `\x01` to `\x1a` codes, alt prefixed keys to `\x1b<char>`) and written to the master fd.
* Resizes call `ioctl(fd, TIOCSWINSZ, ...)` so programs like `htop`, `vim`, or your shell prompt can react to the new size.

The result is that anything you can do in a normal terminal works here too: run `vim`, `htop`, `ssh`, interactive Python, `git`, etc.

## Troubleshooting

**Icons in the file explorer are boxes, question marks, or wrong characters.**
The single most common cause on macOS Terminal.app is using JetBrainsMono Nerd Font, which has a known bug where Devicons do not render. Use the recipe in the Requirements section above: install both `font-meslo-lg-nerd-font` and `font-symbols-only-nerd-font` via Homebrew, set MesloLGS Nerd Font as the Terminal.app font, and fully quit and reopen Terminal.app.

**Colors look wrong in the editor or terminal.**
Make sure your terminal is set to truecolor or 256 colors. In iTerm2: Profiles, Terminal, Report Terminal Type set to `xterm-256color`. The embedded shell exports `TERM=xterm-256color` and `COLORTERM=truecolor` automatically.

**The shell prompt looks weird in the bottom pane.**
Your shell prompt may rely on glyphs that need a Nerd Font. Same fix as above.

**It does not run on Windows.**
Correct. The PTY layer relies on POSIX. Use WSL2 if you are on Windows.

**`uv: command not found`.**
Install uv first. See Requirements above.

## Limitations

This is a minimal three pane shell, not a full IDE. There is no LSP, no debugger, no plugin system, no command palette, no multi tab editor, no git integration. If you want any of those, use VS Code, Neovim, or Emacs. This project's goal is to demonstrate that the three pane experience can fit into a single TUI process and to serve as a small embeddable building block for larger Textual apps.

## License

MIT.
