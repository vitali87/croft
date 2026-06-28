# croft on Android (Termux)

Setup notes for running croft as a native Android binary inside [Termux](https://termux.dev). For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

croft compiles and runs natively on Android via the same `cargo install` command as every other platform. Two things differ on Android: there is no `Cmd` key, and mainline Termux supports no inline-image protocol. croft handles both, plus the lack of a usable soft keyboard, in-process.

There is **no required setup command** on Android. After `cargo install`, croft works on launch: the activity-bar font, the `Ctrl` modifier, and the on-screen keyboard all come up on their own. The only manual step is an optional `pkg install nodejs` if you want the TypeScript / JavaScript server, plus `pkg install gopls` for Go — croft provisions the rest (Python's `ty`/`ruff` and Rust's `rust-analyzer`) itself on first use via `pkg`.

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
- **rust-analyzer** is installed via `pkg` too: its cross-distro release binary is built against glibc and won't run on Android's bionic libc, so croft reroutes to the Termux package (`rust-analyzer`) the same way it does for `ty`/`ruff`.

For the remaining languages, install the servers yourself:

```bash
pkg install nodejs     # lets croft set up the TypeScript / JavaScript server
pkg install gopls      # Go (picked up from PATH)
```

To launch croft on a server with `croft remote <host>`, you also need **`rsync`** on the phone — croft shells out to it to sync the source tree to the box, and a stock Termux has no `rsync`, so the connect fails with `running rsync to remote: spawning streaming subprocess: No such file or directory`:

```bash
pkg install rsync                   # required by `croft remote <host>`
```

## Inline previews

Mainline Termux supports no image protocol (the OSC 1337 support PR is unmerged), so inline image / PDF / spreadsheet previews fall back to a metadata-header line. A Termux build that does support OSC 1337 can opt in with `CROFT_FORCE_INLINE_IMAGES=1`.

## Activity-bar icons (font auto-install)

Without inline images the activity bar draws codicon glyphs, and Android's system fonts contain none of them, so out of the box the bar would render blank. On first launch inside Termux, croft downloads MesloLGS Nerd Font Mono (the same Meslo family `setup-terminal` configures on macOS) into `~/.termux/font.ttf` in the background and applies it with `termux-reload-settings`; the icons appear within a few seconds with no manual step.

An existing `~/.termux/font.ttf` is never overwritten (delete it to re-arm the install), and a failed download is retried on the next launch.

## On-screen keyboard

Termux only raises the Android soft keyboard from its tap path, and that path is skipped entirely while an app has mouse tracking active, which croft always does for click routing. A tap therefore can never summon the native keyboard, so croft ships its own.

**Raising and dismissing it.** Tapping the editor, a terminal pane, or the Search input docks a five-row keyboard above the status bar. The `⌄` key dismisses it. It is thumb-sized: it scales to roughly 40% of the screen on portrait frames, and while it is up only the pane you are typing into stays visible. Focusing the terminal folds the editor away so the terminal rides directly above the keys, and vice versa.

**Layers and modifiers.** It has lowercase, Shift (one-shot uppercase), and two symbol pages, plus a real Caps Lock (uppercases letters only; digits and punctuation are untouched) and one-shot `ctrl` / `alt` latches. Two taps produce chords like `Ctrl`+`C` or `Ctrl`+`P`. Keys synthesize real keystrokes, so they reach the editor, terminal, and every modal identically to a hardware keyboard.

**Symbol pages.** The `?123` key swaps the letters for the first symbol page (digits and the common programming punctuation: `@ # $ _ & - + ( ) / * " ' : ; ! ? \`). That page mirrors Gboard exactly: where Shift sits on the letters, a `=\<` key opens a second "more symbols" page carrying the tilde, backtick and pipe (`~ \` |`), the full bracket family (`{ } [ ] < >`), the operators `^ = %`, and Gboard's math / typographic / currency glyphs (`• √ π ÷ × ¶ ∆ £ ¢ € ¥ ° © ® ™`). On the second page that same key reads `?123` and switches back; the `abc` key returns to the letters from either page. Both symbol pages keep the comma and period beside the space bar and split into thumb clusters for foldables just like the letters.

**Physical-keyboard geometry.** `ctrl` and `alt` sit on the bottom row beside the space bar, the left column staggers like a MacBook (`esc` < `tab` < `caps` < `shift`), and on wide frames the structural keys stay key-sized while the letters and space bar absorb the extra width, so nothing looks stretched on an unfolded foldable.

**Split layout for foldables.** For thumb typing on foldables the `split` key switches to a Gboard-style split layout: two clusters (`qwert` | `yuiop` and friends, with a space bar on each side) separated by a center gap of about two-ninths of the keyboard's width. The choice is remembered across launches in `~/.config/croft/config.json` as `osk_split`. Narrow screens (the folded front display) automatically fall back to the merged layout.

**Voice input.** A mic key sits immediately right of the left `alt`, taking its width from the space bar (the left space half in the split layout). Suppressing the native keyboard also removes its mic button, so this restores dictation: **tap the mic, speak, then pause** and the transcript is inserted automatically. A tap rather than press-and-hold because Termux turns any finger hold on the terminal into its own text-selection gesture, which a TUI cannot suppress. The system speech dialog appears while listening and the status line shows `Listening, speak then pause to insert`. The insert happens when you stop talking, not on a second tap: Android's speech recognizer only produces the final transcript at end-of-speech (silence), so killing it to "stop" would throw the result away. A second tap therefore **cancels** (the dialog also has its own Cancel button). Under the hood it calls `termux-dialog speech`, which drives Android's system recognizer (the same engine Gboard's mic uses) and returns the final transcript, so it lands wherever the cursor is, the editor, a terminal, or a modal. It deliberately does **not** use `termux-speech-to-text`: that service closes its output the moment you pause and so discards the final result, which is why dictation through it came back empty.

**Required: the Termux:API app.** Voice input is the one croft feature with a setup step it cannot do for you. It depends on three separate pieces, and it is easy to think you have them all when you have only two:

1. The **Termux app** itself (you are running it).
2. The **`termux-api` package** (`pkg install termux-api`), a thin command-line client. croft installs this for you on the first mic tap.
3. The **Termux:API app**, a *separate APK* that holds the Android permissions and actually talks to the speech recognizer. **You must install this yourself.**

Install the Termux:API app **from the same source as Termux** (both from [F-Droid](https://f-droid.org/packages/com.termux.api/), or both from the same GitHub build). A mismatch (for example Termux from F-Droid and Termux:API from the Play Store) fails silently because the two apps are signed with different keys and cannot talk to each other. Then open Android **Settings → Apps → Termux:API → Permissions** and grant **Microphone**, and set its battery usage to **Unrestricted** so Android does not kill its background helper.

Verify the bridge is alive before expecting voice to work:

```bash
termux-battery-status     # must print JSON (percentage, status, ...)
```

If that command (or any other `termux-*` command) **hangs**, the Termux:API app is missing, disabled, or signed by a different key than Termux: the package alone cannot reach the recognizer. Fix that first, and voice input then works with no further setup.

## Testing the keyboard on a desktop

Desktop terminals can try the on-screen keyboard with `CROFT_FORCE_OSK=1`, and croft's remote SSH launcher forwards that flag automatically, so a session opened from a phone gets the keyboard on the remote box too.
