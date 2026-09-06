# croft on Android (Termux)

Setup notes for running croft as a native Android binary inside [Termux](https://termux.dev). For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

croft compiles and runs natively on Android via the same `cargo install` as every other platform. Two things differ: there is no `Cmd` key, and mainline Termux supports no inline-image protocol. croft handles both, plus the lack of a usable soft keyboard, in-process.

There is **no required setup command**: the activity-bar font, the `Ctrl` modifier, and the on-screen keyboard all come up on their own. The only manual steps are optional — `pkg install nodejs` for TypeScript / JavaScript and `pkg install gopls` for Go. croft provisions Python's `ty`/`ruff` and Rust's `rust-analyzer` itself on first use.

## Install

```bash
cargo install croft-software --locked
```

This builds croft into `~/.cargo/bin/croft` inside Termux. Re-run to upgrade.

## The command modifier

Android has no `Cmd` key, so **`Ctrl` is the command modifier** (VS Code's Linux convention): every `Cmd` chord works as the same chord with `Ctrl`. Without a hardware keyboard, every chord is reachable through croft's on-screen keyboard (below).

## Dependencies via `pkg`

The curl-based installers croft uses on macOS and Linux cannot run on a stock Termux, which has no curl in the bootstrap, so croft installs dependencies with `pkg install` instead. It does this itself where it can:

- **zoxide**, which backs the `Ctrl`+`Z` directory-jump popup.
- **ty** and **ruff** (the Python servers), because `uv` — croft's provisioning chain elsewhere — does not support Android.
- **rust-analyzer**, whose cross-distro release binary is built against glibc and won't run on Android's bionic libc, so croft reroutes to the Termux package as it does for `ty`/`ruff`.

Install the remaining servers yourself:

```bash
pkg install nodejs     # lets croft set up the TypeScript / JavaScript server
pkg install gopls      # Go (picked up from PATH)
```

`croft remote <host>` also needs **`rsync`** on the phone to sync the source tree to the box. A stock Termux has none, so the connect fails with `running rsync to remote: spawning streaming subprocess: No such file or directory`:

```bash
pkg install rsync                   # required by `croft remote <host>`
```

## Inline previews

Mainline Termux supports no image protocol (the OSC 1337 support PR is unmerged), so inline image / PDF / spreadsheet previews fall back to a metadata-header line. A Termux build that does support OSC 1337 can opt in with `CROFT_FORCE_INLINE_IMAGES=1`.

## Activity-bar icons (font auto-install)

Without inline images the activity bar draws codicon glyphs, and Android's system fonts have none of them, so the bar would render blank. On first launch croft downloads MesloLGS Nerd Font Mono (the Meslo family `setup-terminal` configures on macOS) into `~/.termux/font.ttf` in the background and applies it with `termux-reload-settings`. The icons appear within a few seconds.

An existing `~/.termux/font.ttf` is never overwritten (delete it to re-arm the install), and a failed download is retried on the next launch.

## On-screen keyboard

Termux only raises the Android soft keyboard from its tap path, and that path is skipped while an app has mouse tracking active — which croft always does, for click routing. A tap can therefore never summon the native keyboard, so croft ships its own.

Tapping the editor, a terminal pane, or the Search input docks a five-row keyboard above the status bar; `⌄` dismisses it. It takes roughly 40% of a portrait screen, so while it is up only the pane you are typing into stays visible: focusing the terminal folds the editor away, and vice versa.

**Layers.** Lowercase, Shift (one-shot uppercase), two symbol pages, a real Caps Lock (letters only), and one-shot `ctrl` / `alt` latches, so two taps produce chords like `Ctrl`+`C` or `Ctrl`+`P`. Keys synthesize real keystrokes, reaching the editor, terminal, and every modal identically to a hardware keyboard.

**Symbol pages.** Both mirror Gboard exactly. `?123` swaps the letters for digits and programming punctuation (`@ # $ _ & - + ( ) / * " ' : ; ! ? \`); where Shift sits on the letters, `=\<` opens a second page with `~ \` |`, `{ } [ ] < >`, `^ = %`, and Gboard's math / currency glyphs (`• √ π ÷ × ¶ ∆ £ ¢ € ¥ ° © ® ™`). That key reads `?123` on the second page to switch back; `abc` returns to the letters.

**Geometry.** `ctrl` and `alt` sit beside the space bar and the left column staggers like a MacBook (`esc` < `tab` < `caps` < `shift`). On wide frames the structural keys stay key-sized while the letters and space bar absorb the extra width, so nothing looks stretched on an unfolded foldable. For thumb typing, `split` switches to a Gboard-style split layout: two clusters (`qwert` | `yuiop` and friends, with a space bar on each side) separated by a center gap of about two-ninths of the keyboard's width. The choice persists in `~/.config/croft/config.json` as `osk_split`, and narrow screens fall back to the merged layout automatically.

### Voice input

Suppressing the native keyboard also removes its mic button, so a mic key right of the left `alt` restores dictation: **tap the mic, speak, then pause** and the transcript is inserted automatically. It is a tap rather than press-and-hold because Termux turns any finger hold on the terminal into its own text-selection gesture, which a TUI cannot suppress. The status line shows `Listening, speak then pause to insert` while the system dialog is up.

The insert happens when you stop talking, not on a second tap: Android's recognizer only produces the final transcript at end-of-speech (silence), so killing it to "stop" would throw the result away. A second tap therefore **cancels**. Under the hood it calls `termux-dialog speech`, driving the same system recognizer Gboard's mic uses, so the transcript lands wherever the cursor is. It deliberately does **not** use `termux-speech-to-text`, which closes its output the moment you pause and so discards the final result, returning empty dictation.

Voice input is the one croft feature with a setup step it cannot do for you. It needs three pieces, and it is easy to think you have them all when you have only two:

1. The **Termux app** (you are running it).
2. The **`termux-api` package** (`pkg install termux-api`), a command-line client. croft installs this on the first mic tap.
3. The **Termux:API app**, a *separate APK* holding the Android permissions that actually talks to the recognizer. **You must install this yourself.**

Install Termux:API **from the same source as Termux** (both from [F-Droid](https://f-droid.org/packages/com.termux.api/), or both from the same GitHub build). A mismatch — Termux from F-Droid and Termux:API from the Play Store, say — fails silently, because the two are signed with different keys and cannot talk to each other. Then grant **Microphone** under Android **Settings → Apps → Termux:API → Permissions**, and set battery usage to **Unrestricted** so Android does not kill its background helper.

Verify the bridge before expecting voice to work:

```bash
termux-battery-status     # must print JSON (percentage, status, ...)
```

If that (or any `termux-*` command) **hangs**, the Termux:API app is missing, disabled, or signed by a different key than Termux: the package alone cannot reach the recognizer. Fix that first, and voice input works with no further setup.

## Testing the keyboard on a desktop

Desktop terminals can try the on-screen keyboard with `CROFT_FORCE_OSK=1`. croft's remote SSH launcher forwards it automatically, so a session opened from a phone gets the keyboard on the remote box too.
