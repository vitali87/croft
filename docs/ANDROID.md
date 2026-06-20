# croft on Android (Termux)

Setup notes for running croft as a native Android binary inside [Termux](https://termux.dev). For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

croft compiles and runs natively on Android via the same `cargo install` command as every other platform. Two things differ on Android: there is no `Cmd` key, and mainline Termux supports no inline-image protocol. croft handles both, plus the lack of a usable soft keyboard, in-process.

There is **no required setup command** on Android. After `cargo install`, croft works on launch: the activity-bar font, the `Ctrl` modifier, and the on-screen keyboard all come up on their own. The only manual steps are optional `pkg install`s for the language servers croft cannot provision itself (Node for TypeScript, `rust-analyzer` / `gopls` for Rust / Go), and only if you want LSP for those languages.

## Install

```bash
cargo install --git https://codeberg.org/vitali87/croft.git
```

This builds croft into `~/.cargo/bin/croft` inside Termux. Re-run to upgrade.

## The command modifier

Android has no `Cmd` key, so **`Ctrl` is the command modifier** (VS Code's Linux convention): every `Cmd` chord works as the same chord with `Ctrl`. Touch users without a hardware keyboard get every chord through croft's on-screen keyboard (below).

## Dependencies via `pkg`

The curl-based installers croft uses on macOS and Linux cannot run on a stock Termux (there is no curl in the bootstrap), so croft installs its dependencies from the Termux repo with `pkg install` instead. It does this itself where it can:

- **zoxide** (backs the `Ctrl`+`Z` directory-jump popup) is installed via `pkg`.
- **ty** and **ruff** (the Python language servers) are installed via `pkg`, because `uv` (croft's provisioning chain elsewhere) does not support Android.

For the other languages, install the servers yourself:

```bash
pkg install nodejs                  # lets croft set up the TypeScript / JavaScript server
pkg install rust-analyzer gopls     # Rust and Go (picked up from PATH on every platform)
```

## Inline previews

Mainline Termux supports no image protocol (the OSC 1337 support PR is unmerged), so inline image / PDF / spreadsheet previews fall back to a metadata-header line. A Termux build that does support OSC 1337 can opt in with `CROFT_FORCE_INLINE_IMAGES=1`.

## Activity-bar icons (font auto-install)

Without inline images the activity bar draws codicon glyphs, and Android's system fonts contain none of them, so out of the box the bar would render blank. On first launch inside Termux, croft downloads MesloLGS Nerd Font Mono (the same Meslo family `setup-terminal` configures on macOS) into `~/.termux/font.ttf` in the background and applies it with `termux-reload-settings`; the icons appear within a few seconds with no manual step.

An existing `~/.termux/font.ttf` is never overwritten (delete it to re-arm the install), and a failed download is retried on the next launch.

## On-screen keyboard

Termux only raises the Android soft keyboard from its tap path, and that path is skipped entirely while an app has mouse tracking active, which croft always does for click routing. A tap therefore can never summon the native keyboard, so croft ships its own.

**Raising and dismissing it.** Tapping the editor, a terminal pane, or the Search input docks a five-row keyboard above the status bar. The `⌄` key dismisses it. It is thumb-sized: it scales to roughly 40% of the screen on portrait frames, and while it is up only the pane you are typing into stays visible. Focusing the terminal folds the editor away so the terminal rides directly above the keys, and vice versa.

**Layers and modifiers.** It has lowercase, Shift (one-shot uppercase), and symbol layers, plus a real Caps Lock (uppercases letters only; digits and punctuation are untouched) and one-shot `ctrl` / `alt` latches. Two taps produce chords like `Ctrl`+`C` or `Ctrl`+`P`. Keys synthesize real keystrokes, so they reach the editor, terminal, and every modal identically to a hardware keyboard.

**Physical-keyboard geometry.** `ctrl` and `alt` sit on the bottom row beside the space bar, the left column staggers like a MacBook (`esc` < `tab` < `caps` < `shift`), and on wide frames the structural keys stay key-sized while the letters and space bar absorb the extra width, so nothing looks stretched on an unfolded foldable.

**Split layout for foldables.** For thumb typing on foldables the `split` key switches to a Gboard-style split layout: two clusters (`qwert` | `yuiop` and friends, with a space bar on each side) separated by a center gap of about two-ninths of the keyboard's width. The choice is remembered across launches in `~/.config/croft/config.json` as `osk_split`. Narrow screens (the folded front display) automatically fall back to the merged layout.

**Voice input.** A mic key sits immediately right of the left `alt`, taking its width from the space bar (the left space half in the split layout). Suppressing the native keyboard also removes its mic button, so this restores dictation: **tap the mic, speak, then pause** and the transcript is inserted automatically. A tap rather than press-and-hold because Termux turns any finger hold on the terminal into its own text-selection gesture, which a TUI cannot suppress. The system speech dialog appears while listening and the status line shows `Listening…`. The insert happens when you stop talking, not on a second tap: Android's speech recognizer only produces the final transcript at end-of-speech (silence), so killing it to "stop" would throw the result away. A second tap therefore **cancels** (the dialog also has its own Cancel button). Under the hood it calls `termux-dialog speech`, which drives Android's system recognizer (the same engine Gboard's mic uses) and returns the final transcript, so it lands wherever the cursor is, the editor, a terminal, or a modal. It deliberately does **not** use `termux-speech-to-text`: that service closes its output the moment you pause and so discards the final result, which is why dictation through it came back empty. The first tap installs the `termux-api` package automatically (background `pkg install`); you also need the **Termux:API app** from F-Droid and to grant it microphone permission, since the package alone cannot reach the recognizer.

## Testing the keyboard on a desktop

Desktop terminals can try the on-screen keyboard with `CROFT_FORCE_OSK=1`, and croft's remote SSH launcher forwards that flag automatically, so a session opened from a phone gets the keyboard on the remote box too.
