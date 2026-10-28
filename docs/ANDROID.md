# croft on Android (Termux)

Setup notes for running croft as a native Android binary inside [Termux](https://termux.dev). For what croft does and the full keyboard surface see the [README](../README.md) and [KEYBINDINGS.md](KEYBINDINGS.md).

croft runs natively on Android: releases carry an Android build, and it also compiles from source with the same `cargo install` as every other platform, given the memory the build needs (see [Install](#install)). Two things differ: there is no `Cmd` key, and mainline Termux supports no inline-image protocol. croft handles both, plus the lack of a usable soft keyboard, in-process.

There is **no required setup command**: the activity-bar font, the `Ctrl` modifier, and the on-screen keyboard all come up on their own. The only manual steps are optional — `pkg install nodejs` for TypeScript / JavaScript and `pkg install gopls` for Go. croft provisions Python's `ty`/`ruff`, Rust's `rust-analyzer` and Markdown's `marksman` itself on first use.

## Install

Install the prebuilt Android binary. Compiling croft on the phone needs more memory than most phones leave Termux ([below](#building-from-source)).

```bash
cargo binstall croft-software
```

This downloads the release's `aarch64-linux-android` build into `~/.cargo/bin/croft` in seconds. `cargo binstall` itself needs installing once (`cargo install cargo-binstall`). Re-run to upgrade, or let croft's updater (`Ctrl+Shift+F9`) do it: a croft installed this way updates by downloading the next release's Android build.

Without cargo, `curl` and `tar` are enough:

```bash
pkg install curl
curl -fsSL https://github.com/vitali87/croft/releases/latest/download/croft-aarch64-linux-android.tar.gz | tar xz
mv croft-aarch64-linux-android/croft "$PREFIX/bin/"
```

Releases build for 64-bit ARM only, which covers current phones. On a 32-bit ARM or x86 device, build from source.

### Building from source

```bash
cargo install croft-software --locked
```

This builds croft into `~/.cargo/bin/croft` inside Termux, given the memory the build needs. Its last step compiles croft's own crate in a single `rustc` process, which needs about 4 GB of memory at its peak (4.1 GiB measured on Linux for the release profile). A phone often has less than that free. When it runs out, every dependency compiles fine and then croft's own `rustc` command dies with `(signal: 6, SIGABRT: process abort signal)`.

A different build profile won't get under that: the release profile already peaks lowest of the optimised builds. Splitting croft into more codegen units, as the `remote-fast` profile does, pushes the peak to about 4.5 GiB at `opt-level` 1 or 2, and even `opt-level = 0` needs 3.2 GiB. So either install the prebuilt binary above, or free memory and retry: close other apps until `grep MemAvailable /proc/meminfo` shows at least 5 GB (5000000 kB). The dependencies don't need compiling again: cargo's error names the directory holding them (`intermediate artifacts can be found at ...`), so run the same install command with that directory as the build directory:

```bash
CARGO_BUILD_BUILD_DIR=$PREFIX/tmp/cargo-installXXXXXX cargo install croft-software --locked
```

A croft built from a git checkout or with `cargo install --git` updates by building from source again, so it needs that memory on every update. One installed from crates.io updates by downloading the release's Android build, as a binstalled one does.

## The command modifier

Android has no `Cmd` key, so **`Ctrl` is the command modifier** (VS Code's Linux convention): every `Cmd` chord works as the same chord with `Ctrl`. Without a hardware keyboard, every chord is reachable through croft's on-screen keyboard (below).

## Dependencies via `pkg`

The curl-based installers croft uses on macOS and Linux cannot run on a stock Termux, which has no curl in the bootstrap, so croft installs dependencies with `pkg install` instead. It does this itself where it can:

- **zoxide**, which backs the `Ctrl`+`Z` directory-jump popup. The popup offers `pkg install -y zoxide` when zoxide is missing and runs it only after you press `y`.
- **ty** and **ruff** (the Python servers), because `uv` — croft's provisioning chain elsewhere — does not support Android.
- **rust-analyzer**, whose cross-distro release binary is built against glibc and won't run on Android's bionic libc, so croft reroutes to the Termux package as it does for `ty`/`ruff`.
- **marksman** (the Markdown server), for the same reason: none of its release binaries runs on bionic, so croft installs the Termux package instead.

Install the remaining servers yourself:

```bash
pkg install nodejs     # lets croft set up the TypeScript / JavaScript server
pkg install gopls      # Go (picked up from PATH)
cargo install rumdl    # Markdown linting and formatting (Termux packages no rumdl; picked up from PATH)
```

`croft remote <host>` also needs **`rsync`** on the phone to sync the source tree to the box. A stock Termux has none, so the connect fails with `running rsync to remote: spawning streaming subprocess: No such file or directory`:

```bash
pkg install rsync                   # required by `croft remote <host>`
```

## Inline previews

Mainline Termux supports no image protocol (the OSC 1337 support PR is unmerged), so image and PDF previews are drawn with half-block characters under a metadata-header line. A Termux build that does support OSC 1337 can opt in with `CROFT_FORCE_INLINE_IMAGES=1`.

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
