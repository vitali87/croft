# croft web: a browser client for the session host

Design RFC, now built. The WebSocket transport (#341), its token, TLS and
listener lifecycle (#343), and the page (#342) have shipped. What shipped is
described under [The transport](#the-transport), [The page](#the-page) and
[Threat model](#threat-model), at the end.

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

## The transport

`croft web [WORKSPACE] [--bind ADDR]` serves the running session of a
workspace (the one `croft attach` started) over WebSocket. It is a bridge, not
a second host: each WebSocket connection opens its own connection to the
session host's Unix socket, so a browser tab is an ordinary participant. It
has its own output queue, attaches read-only unless it is first or is granted
control in `Session: Participants`, and leaves the roster when the tab closes,
exactly as a terminal client that detaches.

**Framing.** RFC 6455 over a hand-rolled HTTP/1.1 upgrade (`src/web/ws.rs`),
no framework and no new crates. The host's frames map one to one:

| Session host frame | WebSocket message |
|---|---|
| `Frame::Bytes` (PTY output, keystrokes) | binary |
| `Frame::Control` (`hello`, `resize`, `presence`, ...) | text, the same `{"t": ...}` JSON |

A client opens with a text `{"t":"hello","name":...,"cols":...,"rows":...}`,
then sends keystrokes as binary messages and `resize` as text. A text message
the host would not accept is dropped, as the host drops malformed control
frames. Client frames must be masked; an unmasked frame, a message over 1 MiB,
or non-UTF-8 text closes the connection.

**Connecting.** Every connection presents the per-run token printed at
start-up, as a WebSocket subprotocol: the client offers two, `croft` and
`croft.token.<token>`, and the server selects `croft`. From a browser:

```js
new WebSocket("ws://127.0.0.1:7681/", ["croft", "croft.token." + token]);
```

A browser cannot set headers on a WebSocket, but subprotocols travel in the
`Sec-WebSocket-Protocol` header, so the token never sits in a URL. The reply
names only `croft`, never the token.

**Options.**

| | |
|---|---|
| `--bind ADDR` | Listen on `ADDR` instead of `127.0.0.1:7681`. A non-loopback address is served, with a warning on stderr naming the exposure (and, without `--tls`, that the token and the screen cross the network in plain text). |
| `--tls CERT KEY` | Serve TLS (`wss://`) with a PEM certificate chain and private key. croft does not generate certificates. |
| `--rotate-token` | Stop this workspace's listener and start one with a new token, on the same address unless `--bind` says otherwise. |
| `--off` | Stop this workspace's listener. The session keeps running. |

**Lifecycle.** A listener serves one workspace's session and exits when that
session ends. It records its address beside the session socket
(`<hash>.mux.sock.web`, mode 0600, never the token), so `croft ls` shows
`web: ws://...` for an exposed session, and `--off` and `--rotate-token` find
the listener to stop. A second `croft web` for the same workspace is refused
while one is running. A record whose listener was killed is cleared the next
time it is read.

**Remote access.** The recommended way to reach a session from another
machine is to leave `croft web` on loopback and put something in front of it:

- Tailscale Serve, which terminates TLS with a real certificate and only
  answers your tailnet: `tailscale serve --bg 7681`.
- Caddy as a reverse proxy: `reverse_proxy 127.0.0.1:7681` in a site block.
- A certificate from `mkcert` for `--tls`, when the browser is on the same
  machine or you install mkcert's root on the other.

## The page

`croft web` prints `http://127.0.0.1:7681/#token=...`. Opening it loads a page
the listener serves from the binary: `/`, `/term.js`, `/app.js` and
`/icons.woff2`, about 65 KB together, with a Content-Security-Policy that
allows the page's own scripts, `data:` images and a WebSocket back to the
host it came from, and nothing else. The token sits in the fragment, which
the browser never sends; the page moves it into memory and clears it from
the address bar before connecting.

**Rendering.** `term.js` is a terminal model written for what croft emits,
not a general emulator: cursor addressing, SGR colour (16, 256, truecolor,
and the colon forms) and attributes, erase and insert/delete, scroll
regions, the alternate screen, the private modes croft sets, kitty keyboard
flags (push, pop and the SET form croft re-asserts on every resize), and
wide and combining characters. `app.js` paints its grid on a canvas.

- **Icons.** `icons.woff2` holds the Nerd Font glyphs croft's source uses and
  nothing else (150 glyphs, 21 KB). `scripts/web_icon_font.py` rebuilds it
  from the Meslo Nerd Font; `scripts/tests/test_web_page.py` fails while an
  icon in `src/` is missing from it.
- **Images.** Kitty (`APC G`, chunked PNG, placements replaced by id, `a=d`
  deletes) and iTerm2 (`OSC 1337;File=`) images become `<img>` elements over
  the cells they cover, removed when those cells are written or the screen is
  cleared. Sixel is not decoded.
- **Queries.** DA1, cursor position, kitty flags, window and cell size, and
  default colours are answered, so croft's probes get a reply. Only the
  write-control holder's replies reach croft, as for any client.

**Input.**

- Keys: with kitty flags on, a key with Ctrl, Alt or Cmd is sent as
  `CSI code;mods u`, so `Cmd+K` reaches croft as `CSI 107;9u` exactly as
  from iTerm2 or Ghostty. Without them, the legacy encodings. Plain text goes
  through a hidden textarea, so IME composition, dead keys and phone
  keyboards send what they show. The chords a browser keeps for itself are
  the ones listed under [Input](#input); `Cmd+K` and the palette reach
  everything they would.
- Mouse: SGR reports for the modes croft enables, including drags (pane
  seams resize) and the wheel. Touch scrolling becomes wheel reports.
- Paste: bracketed when croft asked for it.
- Clipboard: croft's OSC 52 copies go to the system clipboard in a secure
  context (HTTPS, or `localhost`); elsewhere the page says the copy was not
  made.

**Size.** The page sizes the grid to the window and sends `resize`; the
host's smallest-window rule applies, so a small browser window shrinks the
session for everyone attached, as a small terminal would.

## Threat model

What croft web defends against:

- **Other local users.** The loopback port is open to every account on the
  machine. The token (128 bits from the system RNG, printed once) is the
  credential; the session socket behind it stays 0600.
- **Other web pages.** WebSockets are not bound by CORS, so any page the user
  has open can try to connect. A request with an `Origin` must match the
  scheme and `Host` it arrived on; on a loopback listener the `Host` must also
  be a loopback name, which closes DNS rebinding (a hostile name resolved to
  127.0.0.1 agrees with its own `Host`, but not with loopback). The page also
  lacks the token.
- **Token leakage.** The token is not in any URL, so it stays out of history,
  `Referer` and proxy logs, and it is not in the listener record. It is
  compared in constant time.
- **The network, with `--tls`.** Everything, the token included, is inside
  TLS.

What it does not do, deliberately:

- **No accounts or per-user tokens.** Anyone with the token is one more
  participant. Write control is still the host's, per participant: a new
  connection is read-only unless it is the first or is granted control in
  `Session: Participants`.
- **No certificate generation or ACME.** Bring a certificate, or front the
  listener with Tailscale or Caddy.
- **No protection on a routable `--bind` without `--tls`.** croft serves it,
  and warns that anyone on the path sees the token and the screen.
- **No rate limiting.** 128 bits of token make guessing pointless; a flood
  of connections is a denial of service croft does not try to stop.
- **Nothing against the local account itself.** Whoever can read the
  terminal croft web was started in has the token, as whoever owns the
  account already owns the session socket.
