# croft on Android (Termux)

Setup notes for running croft as a native Android binary inside [Termux](https://termux.dev). For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

croft builds and runs natively on Android with the same `cargo install` as every other platform. What differs, all handled in-process:

- No `Cmd` key.
- Mainline Termux supports no inline-image protocol.
- No usable soft keyboard.

There is **no required setup command**. The activity-bar font, the `Ctrl` modifier and the on-screen keyboard all come up on their own.

- Optional: `pkg install nodejs` (TypeScript / JavaScript) and `pkg install gopls` (Go).
- croft provisions Python's `ty`/`ruff`, Rust's `rust-analyzer` and Markdown's `marksman` itself on first use.

## Install

```bash
cargo install croft-software --locked
```

This builds croft into `~/.cargo/bin/croft` inside Termux. Re-run to upgrade.

## The command modifier

Android has no `Cmd` key, so **`Ctrl` is the command modifier** (VS Code's Linux convention).

- Every `Cmd` chord works as the same chord with `Ctrl`.
- Without a hardware keyboard, every chord is reachable through croft's on-screen keyboard (below).

## Dependencies via `pkg`

Stock Termux has no curl in the bootstrap, so croft's curl-based macOS/Linux installers cannot run. croft uses `pkg install` instead, and does it itself where it can:

- **zoxide**, which backs the `Ctrl`+`Z` directory-jump popup. When zoxide is missing the popup offers `pkg install -y zoxide` and runs it only after you press `y`.
- **ty** and **ruff** (the Python servers), because `uv` (croft's provisioning chain elsewhere) does not support Android.
- **rust-analyzer**: its cross-distro release binary is built against glibc and won't run on Android's bionic libc, so croft uses the Termux package, as for `ty`/`ruff`.
- **marksman** (the Markdown server): none of its release binaries runs on bionic, so croft installs the Termux package.

Install the remaining servers yourself:

```bash
pkg install nodejs     # lets croft set up the TypeScript / JavaScript server
pkg install gopls      # Go (picked up from PATH)
cargo install rumdl    # Markdown linting and formatting (Termux packages no rumdl; picked up from PATH)
```

`croft remote <host>` also needs **`rsync`** on the phone to sync the source tree to the box. Stock Termux has none, so the connect fails with `running rsync to remote: spawning streaming subprocess: No such file or directory`:

```bash
pkg install rsync                   # required by `croft remote <host>`
```

## Inline previews

- Mainline Termux supports no image protocol (the OSC 1337 support PR is unmerged).
- Image and PDF previews are therefore drawn with half-block characters under a metadata-header line.
- A Termux build that does support OSC 1337 can opt in with `CROFT_FORCE_INLINE_IMAGES=1`.

## Activity-bar icons (font auto-install)

Without inline images the activity bar draws codicon glyphs. Android's system fonts have none, so the bar would render blank.

- On first launch croft downloads MesloLGS Nerd Font Mono (the Meslo family `setup-terminal` configures on macOS) into `~/.termux/font.ttf`, in the background.
- It applies the font with `termux-reload-settings`. Icons appear within a few seconds.
- An existing `~/.termux/font.ttf` is never overwritten. Delete it to re-arm the install.
- A failed download is retried on the next launch.

## On-screen keyboard

Termux only raises the Android soft keyboard from its tap path. That path is skipped while an app has mouse tracking active, and croft always has it on for click routing. So a tap can never summon the native keyboard, and croft ships its own.

- Tapping the editor, a terminal pane, or the Search input docks a five-row keyboard above the status bar.
- `⌄` dismisses it.
- It takes roughly 40% of a portrait screen, so only the pane you are typing into stays visible: focusing the terminal folds the editor away, and vice versa.

**Layers.**

- Lowercase, Shift (one-shot uppercase), two symbol pages, and a real Caps Lock (letters only).
- One-shot `ctrl` / `alt` latches, so two taps produce chords like `Ctrl`+`C` or `Ctrl`+`P`.
- Keys synthesize real keystrokes, reaching the editor, terminal and every modal exactly like a hardware keyboard.

**Symbol pages.** Both mirror Gboard exactly.

- `?123` swaps the letters for digits and programming punctuation (`@ # $ _ & - + ( ) / * " ' : ; ! ? \`).
- On that page, the key where Shift sits on the letters (`=\<`) opens a second page with `~ \` |`, `{ } [ ] < >`, `^ = %`, and Gboard's math / currency glyphs (`• √ π ÷ × ¶ ∆ £ ¢ € ¥ ° © ® ™`).
- That key reads `?123` on the second page to switch back. `abc` returns to the letters.

**Geometry.**

- `ctrl` and `alt` sit beside the space bar.
- The left column staggers like a MacBook (`esc` < `tab` < `caps` < `shift`).
- On wide frames the structural keys stay key-sized and the letters and space bar absorb the extra width, so nothing looks stretched on an unfolded foldable.
- For thumb typing, `split` switches to a Gboard-style split layout: two clusters (`qwert` | `yuiop` and friends, a space bar on each side) with a center gap of about two-ninths of the keyboard's width.
- The choice persists in `~/.config/croft/config.json` as `osk_split`.
- Narrow screens fall back to the merged layout automatically.

### Voice input

Suppressing the native keyboard also removes its mic button. A mic key right of the left `alt` restores dictation: **tap the mic, speak, then pause** and the transcript is inserted automatically.

- It is a tap, not press-and-hold, because Termux turns any finger hold on the terminal into its own text-selection gesture, which a TUI cannot suppress.
- The status line shows `Listening, speak then pause to insert` while the system dialog is up.
- The insert happens when you stop talking. Android's recognizer only produces the final transcript at end-of-speech (silence), so killing it to "stop" would lose the result.
- A second tap therefore **cancels**.
- Under the hood it calls `termux-dialog speech`, the same system recognizer Gboard's mic uses, so the transcript lands wherever the cursor is.
- It deliberately does **not** use `termux-speech-to-text`: that closes its output the moment you pause, discarding the final result and returning empty dictation.

Voice input is the one croft feature with a setup step it cannot do for you. It needs three pieces, and it is easy to have only two:

1. The **Termux app** (you are running it).
2. The **`termux-api` package** (`pkg install termux-api`), a command-line client. croft installs this on the first mic tap.
3. The **Termux:API app**, a *separate APK* holding the Android permissions that actually talks to the recognizer. **You must install this yourself.**

Setting up Termux:API:

- Install it **from the same source as Termux** (both from [F-Droid](https://f-droid.org/packages/com.termux.api/), or both from the same GitHub build).
- A mismatch (say Termux from F-Droid, Termux:API from the Play Store) fails silently: the two are signed with different keys and cannot talk to each other.
- Grant **Microphone** under Android **Settings → Apps → Termux:API → Permissions**.
- Set battery usage to **Unrestricted** so Android does not kill its background helper.

Verify the bridge before expecting voice to work:

```bash
termux-battery-status     # must print JSON (percentage, status, ...)
```

If that (or any `termux-*` command) **hangs**, the Termux:API app is missing, disabled, or signed by a different key than Termux. The package alone cannot reach the recognizer. Fix that first, and voice input works with no further setup.

## Testing the keyboard on a desktop

- Desktop terminals can try the on-screen keyboard with `CROFT_FORCE_OSK=1`.
- croft's remote SSH launcher forwards it automatically, so a session opened from a phone gets the keyboard on the remote box too.
