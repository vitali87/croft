# croft web: a browser client for the session host

Design RFC. Nothing here has shipped; this document exists to settle the
protocol and the trust boundary before any listener code lands.

The session host already fans one inner croft out to N clients, enforces write
control server-side, sizes everyone to the smallest window, and survives
detach. A browser is another client. What follows is what that costs.

## The decision, up front

**Ship the byte stream (option a), with a negotiated image sidechannel.** The
cell-diff protocol (option b) is the more elegant idea and it is the wrong
first move, for a reason that only shows up once you look at where images are
written.

## What crosses the wire

### Option (a): the existing byte stream

The host relays raw PTY bytes today, and the wire format is already there:

```
[type: u8][len: u32 be][payload]      session_host.rs:14-17
FRAME_BYTES = 0, FRAME_CONTROL = 1     session_host.rs:31-33
```

`encode_frame` is a 5-byte header and the payload (`session_host.rs:140-158`).
`FrameReader` (`:160-196`) resynchronises on framing, skips a malformed control
payload rather than poisoning the stream, and — the load-bearing part —
**ignores unknown frame types**:

```rust
// Unknown frame type from a newer peer: ignore the payload.
_ => {}
```

So a new frame type can be added without breaking existing hosts or clients.
`Claim`, `HostSwap` and `ServerHello` already rely on that tolerance.

The browser needs a terminal emulator in JS. That is the cost of option (a),
and it is a real one: a vendored emulator is a second implementation of
something croft already has, and it will disagree with the Rust one at the
edges.

### Option (b): ship changed cells

The host runs croft's own emulator and sends a cell grid. The page becomes a
dumb painter with no JS emulator to vendor, and rendering is identical to the
terminal by construction.

**The headless half is already solved**, which is better than the issue assumed.
`PtyTerminal` owns `Arc<FairMutex<Term<VoidListener>>>` (`terminal.rs:442`), and
`VoidListener` (`:163-199`) handles only `Bell`, `PtyWrite` and
`TextAreaSizeRequest`, answering the last with `cell_width: 1, cell_height: 1`.
There is no display, no window handle, no pixel geometry. Bytes are fed with a
plain `Processor::advance` call (`:1762`). `grid_lines_ansi` (`:1404-1461`)
already exports the grid as text plus SGR, touches no ratatui type, and has
seven non-render callers. ratatui appears 9 times in a 9242-line file, all
inside `impl Widget`.

So option (b) is not blocked on the emulator. It is blocked on four things:

1. **No damage tracking exists.** The only dirty signal in croft is one
   whole-terminal `AtomicBool` (`terminal.rs:443-445`). alacritty's own
   `Term::damage()` is not used anywhere. Option (b) would have to keep a
   previous-frame grid and diff it — new code, on the hot path.
2. **`PtyTerminal` bundles the PTY.** `spawn_with_preamble` always opens a pty
   and spawns a child (`:1714+`); there is no constructor taking an existing
   byte stream. A headless owner needs a new entry point, or to use
   `Term::new` + `Processor::advance` directly as the tests do.
3. **A cell is bigger than it looks.** `alacritty_terminal 0.26`'s `Cell` is
   `{c: char, fg: Color, bg: Color, flags: Flags, extra: Option<Arc<CellExtra>>}`
   (`cell.rs:131-140`). `flags` is a 17-bit bitfield; `extra` carries combining
   marks, per-cell underline colour, and OSC 8 hyperlinks — which croft uses.
   Naively 8-12 bytes per cell before `extra`.
4. **It cannot see the images.** This is the one that decides it.

### Why images decide it

Inline images never enter the ratatui buffer and never enter any client's
alacritty grid. They are written straight to stdout, after the frame, with
absolute cursor positioning (`app/mod.rs:5911-5943`):

```rust
let _ = write!(out, "\x1b[?25l\x1b[s");
for ((x, y), seq) in &overlays {
    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
    let _ = out.write_all(seq.as_bytes());
}
let _ = write!(out, "\x1b[u");
```

On the byte path they pass through verbatim, which is exactly what the host
promises (`session_host.rs:5-7`: byte-transparent, "Kitty graphics and every
escape sequence pass through untouched"). **On a cell-grid path they would be
invisible** — the grid has nothing where the picture is. Option (b) therefore
needs an image sidechannel anyway, so it does not avoid the second protocol; it
adds one on top of losing byte-transparency.

Option (a) needs that sidechannel too, but only to *improve* on a path that
already works, rather than to repair one that does not.

### Measurement, outstanding

The issue asks for measured byte rates for (a) and (b) on a scrolling
`cargo build`. **Neither is measured, and no number should be inferred from
this document.** There is no benchmark, no frame-size counter, and for (b) no
diffing code to measure. What the tree does record:

- `pty_pending_bytes` (`terminal.rs:446-455`) exists precisely to distinguish
  interactive echo from a bulk stream, so that "large ones stay capped so they
  can't saturate the ssh pipe and starve input."
- `is_remote_session()` (`app/mod.rs:46819-46826`) throttles PTY redraws further
  over SSH for the same reason.

Byte-rate saturation over a network hop is a known, already-mitigated problem
here. That is an argument for measuring before committing to (b), not for
assuming (a) is cheap.

## Fonts and glyphs

Explorer icons and the activity bar are Private Use Area Nerd Font glyphs. A
viewer must not have to install anything, so the page embeds a **subset woff2**
of the glyphs croft actually uses. The set is enumerable: it is what the icon
tables reference, not an open-ended range.

## Input

**Keyboard is where a browser client loses the most, and the loss is
structural.** croft receives Cmd chords at all only because it installs
forwarders into the terminal emulator — `Chord::iterm2_forwarder`
(`keymap.rs:82-116`) and `ghostty_trigger` (`:120-145`) re-emit them as CSI-u,
and `iterm2.rs` relocates conflicting menu items. A browser has no equivalent
escape hatch: `preventDefault()` cannot reach a chord the OS or the browser
claims at window level.

Unreachable, with what croft binds them to:

| Chord | Browser takes it for | croft uses it for |
| --- | --- | --- |
| `Cmd/Ctrl+W` | Close tab | Close the active editor tab; close the terminal |
| `Cmd/Ctrl+T` | New tab | Open another terminal |
| `Cmd+Q` | Quit (macOS) | — (`Ctrl+Q` is Quit) |
| `Cmd+Shift+W` | Close window | `Cmd+K Shift+W`, Reopen Closed Editor |
| `Cmd+Shift+T` | Reopen closed tab | Open a terminal; restore a closed pane |
| `Cmd+Shift+R` | Hard reload | Jump to Remote (SSH) |
| `Cmd+0` / `Cmd+8` / `Cmd+9` | Zoom reset, jump to tab N | Bound |

Contested but generally preventable: `Cmd+P` (Quick Open, croft's most-used
chord), `Cmd+Shift+P` (Command Palette; Firefox takes it for a private window),
`Cmd+S`, `Cmd+F`, `Cmd+L` (unpreventable in Firefox), `Cmd+Shift+D`,
`Cmd+Shift+E`, `Cmd+Shift+B`.

**The mitigation is already in the design.** `Cmd+K` is croft's chord prefix,
with 55 bindings under it, and everything after the prefix is a *second*
keystroke inside croft's own state machine — so the entire `Cmd+K` family
survives intact. Together with the Command Palette, which lists every named
command with its keybinding, the unreachable set is single-keystroke chords
whose function is reachable another way. The RFC's position: **do not remap
anything for the web client.** Document the unreachable set, and let `Cmd+K`
and the palette carry it.

The browser must also reproduce croft's VT translation for unbound keys —
arrows, most `Ctrl+letter`, `Alt+x`, function keys (`KEYBINDINGS.md:507`),
currently done by croft's crossterm router — and SGR mouse encoding, matching
the terminal path.

## Images

On the iTerm2 and Kitty paths the payload is **base64-encoded PNG**
(`iterm2_inline.rs:738-750`, `:762-804`), which a browser can drop into a
`data:image/png;base64,...` URL with no transcoding. Kitty needs de-chunking
across `m=1` continuations; iTerm2 is one blob. **Sixel is different** — it is
NeuQuant-quantised and DCS-wrapped (`:828-947`), so a web gateway should
negotiate iTerm2 or Kitty and never Sixel.

Sizing is in **cells**, not pixels (`width={width_cells};height={height_cells}`),
so the client needs croft's cell metrics to place a picture. A browser can draw
these natively, which plausibly makes it the best image host croft has — but
that is an outcome, not a reason to choose a protocol.

## Trust boundary

**This would be the first listening socket in croft's history.** That is not a
rhetorical point; it is measured. Every `TcpListener::bind` in the tree today
is either a test, or a bind-to-probe-a-free-port (`remote.rs:1565-1569`,
`dap/transport.rs:342`). `tokio::net` appears zero times; `tokio`'s `net`
feature is deliberately absent. No HTTP server or websocket crate is in
`Cargo.lock`, not even transitively. What exists is Unix sockets at mode 0600
(`session.rs:464`) and SSH.

So the current boundary is **filesystem permissions plus SSH**, and the host
says so itself (`session_host.rs:836-839`): the random token "is a
discriminator (inner croft vs regular participant), not the trust boundary;
account possession remains that."

A browser client replaces that with a network boundary. In scope for the
follow-ups (#341, #342, #343), and to be signed off *before* any listener
lands:

- **LAN exposure.** Default bind must be loopback. A LAN bind is an explicit,
  separate opt-in, never a convenience default.
- **Token leakage via URL.** A token in a query string lands in history, in
  `Referer`, and in any logging proxy. Prefer a one-time exchange for a
  cookie or header credential.
- **CSRF against a local listener.** A page on any origin can issue requests to
  `localhost`. Requires origin checking and a non-cookie-only credential.
- **The write-control model must not weaken.** Server-side enforcement
  (`session_host.rs:1354-1370`) is what makes read-only real rather than
  advisory; a web client is another client under the same rule, and must not
  get a bypass for convenience.

One interaction worth stating: **a browser client joining at a different size
shrinks the PTY for everyone**, because `min_winsize` (`:207-212`) takes the
minimum across clients. A phone-sized viewport would reflow the session for
every attached terminal. Whether the web client is an observer (zero size,
already filtered out) or a full participant is a product decision this RFC does
not make.

## Phased plan

1. **A gateway process, no listener.** Re-implement the client pump against the
   Unix socket, exercised by a test harness rather than a browser.
   `attach_client` cannot be reused: it enables raw mode and sizes from
   `crossterm::terminal::size()` (`session_host.rs:1731-1738`, `:1783`).
2. **Loopback listener plus token** (#343). The trust boundary decisions above
   must be signed off first.
3. **The page** (#342): grid painter, subset woff2, VT translation, SGR mouse.
4. **Image sidechannel.** A new frame type, using the unknown-type tolerance so
   older clients ignore it.
5. **Measure.** Only once there is a working (a) implementation is a real
   comparison against (b) possible.

## What this RFC does not settle

- The byte-rate comparison the issue asks for. Unmeasured, deliberately not
  estimated.
- Whether a web client is an observer or a full participant in `min_winsize`.
- IME, which needs a browser-side prototype rather than a reading of this tree.
- Per-browser preventability of the contested chords above, which is platform
  behaviour and should be verified per browser rather than asserted here.
