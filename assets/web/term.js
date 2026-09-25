// croft web's terminal emulator (#342): enough of VT/xterm to show croft
// itself. croft draws its whole UI with absolute cursor moves, SGR colours
// (256 and truecolor), line clears and the alternate screen, so that is what
// this implements, plus scroll regions and the insert/delete edits a shell
// in croft's terminal panel uses. Anything else is parsed and ignored, never
// printed. The page (index.html) owns input and drawing; this file only
// turns bytes into a grid, so it runs the same in a test under node.

const WIDE = [
  [0x1100, 0x115f], [0x2e80, 0x303e], [0x3041, 0x33ff], [0x3400, 0x4dbf],
  [0x4e00, 0x9fff], [0xa000, 0xa4cf], [0xac00, 0xd7a3], [0xf900, 0xfaff],
  [0xfe30, 0xfe4f], [0xff00, 0xff60], [0xffe0, 0xffe6], [0x1f300, 0x1f64f],
  [0x1f900, 0x1f9ff], [0x20000, 0x3fffd],
];

/** Display width of a code point: 0, 1 or 2 cells. */
export function charWidth(cp) {
  if (cp === 0 || (cp >= 0x300 && cp <= 0x36f) || cp === 0x200d || (cp >= 0xfe00 && cp <= 0xfe0f)) return 0;
  for (const [a, b] of WIDE) if (cp >= a && cp <= b) return 2;
  return 1;
}

/** The xterm 256-colour palette as [r, g, b]. */
export function palette256(n) {
  const base = [
    [0, 0, 0], [205, 0, 0], [0, 205, 0], [205, 205, 0], [0, 0, 238], [205, 0, 205], [0, 205, 205], [229, 229, 229],
    [127, 127, 127], [255, 0, 0], [0, 255, 0], [255, 255, 0], [92, 92, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255],
  ];
  if (n < 16) return base[n];
  if (n < 232) {
    const i = n - 16;
    const step = (v) => (v === 0 ? 0 : 55 + v * 40);
    return [step(Math.floor(i / 36)), step(Math.floor(i / 6) % 6), step(i % 6)];
  }
  const g = 8 + (n - 232) * 10;
  return [g, g, g];
}

const BOLD = 1, ITALIC = 2, UNDERLINE = 4, INVERSE = 8, DIM = 16, STRIKE = 32;
export const ATTR = { BOLD, ITALIC, UNDERLINE, INVERSE, DIM, STRIKE };

function blank(fg = null, bg = null) {
  return { ch: " ", w: 1, fg, bg, attrs: 0 };
}

export class Term {
  constructor(cols, rows) {
    this.cols = cols;
    this.rows = rows;
    this.decoder = new TextDecoder("utf-8");
    this.state = "ground";
    this.params = "";
    this.osc = "";
    this.apc = "";
    this.pen = { fg: null, bg: null, attrs: 0 };
    this.cx = 0;
    this.cy = 0;
    this.saved = { cx: 0, cy: 0 };
    this.top = 0;
    this.bottom = rows - 1;
    this.cursorVisible = true;
    this.autowrap = true;
    this.modes = new Set();
    this.title = "";
    this.images = [];
    this.replies = [];
    this.primary = this.makeGrid();
    this.alt = this.makeGrid();
    this.grid = this.primary;
    this.dirty = new Set();
    this.markAll();
  }

  makeGrid() {
    return Array.from({ length: this.rows }, () => Array.from({ length: this.cols }, () => blank()));
  }

  markAll() {
    for (let y = 0; y < this.rows; y++) this.dirty.add(y);
  }

  /** Bytes from the session, in any chunking. */
  write(bytes) {
    const text = typeof bytes === "string" ? bytes : this.decoder.decode(bytes, { stream: true });
    for (const c of text) this.feed(c);
  }

  /** Replies owed to the application (DSR, DA): the page sends these back. */
  takeReplies() {
    const r = this.replies;
    this.replies = [];
    return r;
  }

  feed(c) {
    const cp = c.codePointAt(0);
    switch (this.state) {
      case "ground":
        if (cp === 0x1b) this.state = "esc";
        else if (cp < 0x20 || cp === 0x7f) this.control(cp);
        else this.print(c, cp);
        return;
      case "esc":
        this.state = "ground";
        if (c === "[") { this.state = "csi"; this.params = ""; }
        else if (c === "]") { this.state = "osc"; this.osc = ""; }
        else if (c === "_") { this.state = "apc"; this.apc = ""; }
        else if (c === "P") { this.state = "dcs"; }
        else if (c === "7") this.saved = { cx: this.cx, cy: this.cy, pen: { ...this.pen } };
        else if (c === "8") { this.cx = this.saved.cx; this.cy = this.saved.cy; if (this.saved.pen) this.pen = { ...this.saved.pen }; }
        else if (c === "M") this.reverseIndex();
        else if (c === "D") this.lineFeed();
        else if (c === "E") { this.cx = 0; this.lineFeed(); }
        else if (c === "c") this.reset();
        else if (c === "(" || c === ")") this.state = "charset";
        return;
      case "charset":
        this.state = "ground";
        return;
      case "csi":
        if (cp >= 0x40 && cp <= 0x7e) { this.state = "ground"; this.csi(c, this.params); }
        else this.params += c;
        return;
      case "osc":
        if (cp === 0x07) { this.state = "ground"; this.oscDone(); }
        else if (cp === 0x1b) this.state = "osc_esc";
        else this.osc += c;
        return;
      case "osc_esc":
        this.state = c === "\\" ? "ground" : "osc";
        if (this.state === "ground") this.oscDone();
        return;
      case "apc":
        if (cp === 0x1b) this.state = "apc_esc";
        else this.apc += c;
        return;
      case "apc_esc":
        if (c === "\\") { this.state = "ground"; this.apcDone(); }
        else { this.apc += "\x1b" + c; this.state = "apc"; }
        return;
      case "dcs":
        if (cp === 0x1b) this.state = "dcs_esc";
        return;
      case "dcs_esc":
        this.state = c === "\\" ? "ground" : "dcs";
        return;
    }
  }

  control(cp) {
    if (cp === 0x0d) this.cx = 0;
    else if (cp === 0x0a || cp === 0x0b || cp === 0x0c) this.lineFeed();
    else if (cp === 0x08) this.cx = Math.max(0, this.cx - 1);
    else if (cp === 0x09) this.cx = Math.min(this.cols - 1, (Math.floor(this.cx / 8) + 1) * 8);
  }

  print(c, cp) {
    const w = charWidth(cp);
    if (w === 0) return;
    if (this.cx + w > this.cols) {
      if (this.autowrap) { this.cx = 0; this.lineFeed(); }
      else this.cx = this.cols - w;
    }
    const row = this.grid[this.cy];
    row[this.cx] = { ch: c, w, fg: this.pen.fg, bg: this.pen.bg, attrs: this.pen.attrs };
    if (w === 2 && this.cx + 1 < this.cols) row[this.cx + 1] = { ch: "", w: 0, fg: this.pen.fg, bg: this.pen.bg, attrs: this.pen.attrs };
    this.dirty.add(this.cy);
    this.cx += w;
    if (this.cx >= this.cols) this.cx = this.cols; // pending wrap
  }

  lineFeed() {
    if (this.cy === this.bottom) this.scrollUp(1);
    else if (this.cy < this.rows - 1) this.cy++;
  }

  reverseIndex() {
    if (this.cy === this.top) this.scrollDown(1);
    else if (this.cy > 0) this.cy--;
  }

  scrollUp(n) {
    for (let i = 0; i < n; i++) {
      this.grid.splice(this.top, 1);
      this.grid.splice(this.bottom, 0, this.blankRow());
    }
    for (let y = this.top; y <= this.bottom; y++) this.dirty.add(y);
  }

  scrollDown(n) {
    for (let i = 0; i < n; i++) {
      this.grid.splice(this.bottom, 1);
      this.grid.splice(this.top, 0, this.blankRow());
    }
    for (let y = this.top; y <= this.bottom; y++) this.dirty.add(y);
  }

  blankRow() {
    return Array.from({ length: this.cols }, () => blank(null, this.pen.bg));
  }

  eraseCells(y, from, to) {
    const row = this.grid[y];
    for (let x = Math.max(0, from); x < Math.min(this.cols, to); x++) row[x] = blank(null, this.pen.bg);
    this.dirty.add(y);
  }

  csi(final, raw) {
    const priv = raw.startsWith("?") ? "?" : raw.startsWith(">") ? ">" : raw.startsWith("<") ? "<" : "";
    const body = priv ? raw.slice(1) : raw;
    const ps = body === "" ? [] : body.split(";").map((p) => p);
    const n = (i, d = 1) => {
      const v = parseInt(ps[i], 10);
      return Number.isNaN(v) || v === 0 ? d : v;
    };
    const cx = Math.min(this.cx, this.cols - 1);
    switch (final) {
      case "H": case "f":
        this.cy = Math.min(this.rows - 1, n(0) - 1);
        this.cx = Math.min(this.cols - 1, n(1) - 1);
        return;
      case "A": this.cy = Math.max(0, this.cy - n(0)); return;
      case "B": case "e": this.cy = Math.min(this.rows - 1, this.cy + n(0)); return;
      case "C": case "a": this.cx = Math.min(this.cols - 1, cx + n(0)); return;
      case "D": this.cx = Math.max(0, cx - n(0)); return;
      case "E": this.cx = 0; this.cy = Math.min(this.rows - 1, this.cy + n(0)); return;
      case "F": this.cx = 0; this.cy = Math.max(0, this.cy - n(0)); return;
      case "G": case "`": this.cx = Math.min(this.cols - 1, n(0) - 1); return;
      case "d": this.cy = Math.min(this.rows - 1, n(0) - 1); return;
      case "J": {
        const mode = parseInt(ps[0] || "0", 10);
        if (mode === 0) { this.eraseCells(this.cy, cx, this.cols); for (let y = this.cy + 1; y < this.rows; y++) this.eraseCells(y, 0, this.cols); }
        else if (mode === 1) { this.eraseCells(this.cy, 0, cx + 1); for (let y = 0; y < this.cy; y++) this.eraseCells(y, 0, this.cols); }
        else { for (let y = 0; y < this.rows; y++) this.eraseCells(y, 0, this.cols); if (mode === 2) this.images = []; }
        return;
      }
      case "K": {
        const mode = parseInt(ps[0] || "0", 10);
        if (mode === 0) this.eraseCells(this.cy, cx, this.cols);
        else if (mode === 1) this.eraseCells(this.cy, 0, cx + 1);
        else this.eraseCells(this.cy, 0, this.cols);
        return;
      }
      case "X": this.eraseCells(this.cy, cx, cx + n(0)); return;
      case "@": { const row = this.grid[this.cy]; for (let i = 0; i < n(0); i++) { row.splice(cx, 0, blank(null, this.pen.bg)); row.pop(); } this.dirty.add(this.cy); return; }
      case "P": { const row = this.grid[this.cy]; for (let i = 0; i < n(0); i++) { row.splice(cx, 1); row.push(blank(null, this.pen.bg)); } this.dirty.add(this.cy); return; }
      case "L": if (this.cy >= this.top && this.cy <= this.bottom) { const t = this.top; this.top = this.cy; this.scrollDown(n(0)); this.top = t; } return;
      case "M": if (this.cy >= this.top && this.cy <= this.bottom) { const t = this.top; this.top = this.cy; this.scrollUp(n(0)); this.top = t; } return;
      case "S": if (!priv) this.scrollUp(n(0)); return;
      case "T": this.scrollDown(n(0)); return;
      case "r": {
        this.top = Math.max(0, n(0) - 1);
        this.bottom = Math.min(this.rows - 1, (ps[1] ? n(1) : this.rows) - 1);
        this.cx = 0; this.cy = 0;
        return;
      }
      case "s": this.saved = { cx: this.cx, cy: this.cy, pen: { ...this.pen } }; return;
      case "u": if (!priv) { this.cx = this.saved.cx; this.cy = this.saved.cy; } else if (priv === "?") this.replies.push("\x1b[?0u"); return;
      case "m": this.sgr(ps); return;
      case "h": case "l": {
        const on = final === "h";
        for (const p of ps) {
          const m = priv === "?" ? parseInt(p, 10) : `ansi${p}`;
          if (priv === "?" && m === 25) this.cursorVisible = on;
          if (priv === "?" && m === 7) this.autowrap = on;
          if (priv === "?" && (m === 1049 || m === 47 || m === 1047)) this.altScreen(on);
          if (on) this.modes.add(m); else this.modes.delete(m);
        }
        return;
      }
      case "n":
        if (ps[0] === "6") this.replies.push(`\x1b[${this.cy + 1};${Math.min(this.cx, this.cols - 1) + 1}R`);
        else if (ps[0] === "5") this.replies.push("\x1b[0n");
        return;
      case "c":
        if (priv === "") this.replies.push("\x1b[?62;22c");
        return;
      case "t": return;
      default: return;
    }
  }

  altScreen(on) {
    if (on && this.grid !== this.alt) {
      this.saved = { cx: this.cx, cy: this.cy, pen: { ...this.pen } };
      this.alt = this.makeGrid();
      this.grid = this.alt;
    } else if (!on && this.grid !== this.primary) {
      this.grid = this.primary;
      this.cx = this.saved.cx; this.cy = this.saved.cy;
    }
    this.images = [];
    this.markAll();
  }

  sgr(ps) {
    if (ps.length === 0) ps = ["0"];
    for (let i = 0; i < ps.length; i++) {
      const p = ps[i];
      // Colon sub-parameters (38:2::r:g:b, 4:3) arrive inside one field.
      if (p.includes(":")) {
        const sub = p.split(":").map((s) => parseInt(s || "0", 10));
        if (sub[0] === 38 || sub[0] === 48 || sub[0] === 58) {
          const rgb = sub[1] === 2 ? sub.slice(-3) : sub[1] === 5 ? palette256(sub[2]) : null;
          if (sub[0] === 38) this.pen.fg = rgb; else if (sub[0] === 48) this.pen.bg = rgb;
        } else if (sub[0] === 4) {
          if (sub[1] === 0) this.pen.attrs &= ~UNDERLINE; else this.pen.attrs |= UNDERLINE;
        }
        continue;
      }
      const v = parseInt(p || "0", 10);
      if (v === 0) this.pen = { fg: null, bg: null, attrs: 0 };
      else if (v === 1) this.pen.attrs |= BOLD;
      else if (v === 2) this.pen.attrs |= DIM;
      else if (v === 3) this.pen.attrs |= ITALIC;
      else if (v === 4) this.pen.attrs |= UNDERLINE;
      else if (v === 7) this.pen.attrs |= INVERSE;
      else if (v === 9) this.pen.attrs |= STRIKE;
      else if (v === 22) this.pen.attrs &= ~(BOLD | DIM);
      else if (v === 23) this.pen.attrs &= ~ITALIC;
      else if (v === 24) this.pen.attrs &= ~UNDERLINE;
      else if (v === 27) this.pen.attrs &= ~INVERSE;
      else if (v === 29) this.pen.attrs &= ~STRIKE;
      else if (v >= 30 && v <= 37) this.pen.fg = palette256(v - 30);
      else if (v >= 90 && v <= 97) this.pen.fg = palette256(v - 90 + 8);
      else if (v === 39) this.pen.fg = null;
      else if (v >= 40 && v <= 47) this.pen.bg = palette256(v - 40);
      else if (v >= 100 && v <= 107) this.pen.bg = palette256(v - 100 + 8);
      else if (v === 49) this.pen.bg = null;
      else if (v === 38 || v === 48 || v === 58) {
        const mode = parseInt(ps[i + 1], 10);
        let rgb = null;
        if (mode === 5) { rgb = palette256(parseInt(ps[i + 2], 10) || 0); i += 2; }
        else if (mode === 2) { rgb = [ps[i + 2], ps[i + 3], ps[i + 4]].map((x) => parseInt(x, 10) || 0); i += 4; }
        if (v === 38) this.pen.fg = rgb; else if (v === 48) this.pen.bg = rgb;
      }
    }
  }

  oscDone() {
    const [code, ...rest] = this.osc.split(";");
    if (code === "0" || code === "2") this.title = rest.join(";");
  }

  /**
   * Kitty graphics (croft's inline images and file icons): a transmit-and-
   * display (`a=T`) of PNG data (`f=100`) placed at the cursor over `c`
   * columns and `r` rows. Chunked transfers (`m=1`) accumulate until the
   * last chunk. Deletes (`a=d`) clear the placements.
   */
  apcDone() {
    const s = this.apc;
    if (!s.startsWith("G")) return;
    const semi = s.indexOf(";");
    const head = semi < 0 ? s.slice(1) : s.slice(1, semi);
    const data = semi < 0 ? "" : s.slice(semi + 1);
    const kv = Object.fromEntries(head.split(",").filter(Boolean).map((p) => p.split("=")));
    if (kv.a === "d") { this.images = []; this.markAll(); return; }
    if (this.pendingImage) {
      this.pendingImage.data += data;
      if (kv.m !== "1") { this.placeImage(this.pendingImage); this.pendingImage = null; }
      return;
    }
    const img = { kv, data, x: Math.min(this.cx, this.cols - 1), y: this.cy };
    if (kv.m === "1") { this.pendingImage = img; return; }
    this.placeImage(img);
  }

  placeImage(img) {
    const cols = parseInt(img.kv.c || "1", 10);
    const rows = parseInt(img.kv.r || "1", 10);
    // A newer image over the same cells replaces the older one.
    this.images = this.images.filter((o) => !(o.x === img.x && o.y === img.y));
    this.images.push({ x: img.x, y: img.y, cols, rows, format: img.kv.f || "100", data: img.data });
    for (let y = img.y; y < Math.min(this.rows, img.y + rows); y++) this.dirty.add(y);
  }

  reset() {
    this.pen = { fg: null, bg: null, attrs: 0 };
    this.cx = 0; this.cy = 0; this.top = 0; this.bottom = this.rows - 1;
    this.primary = this.makeGrid(); this.alt = this.makeGrid(); this.grid = this.primary;
    this.images = [];
    this.markAll();
  }

  resize(cols, rows) {
    const fit = (g) => Array.from({ length: rows }, (_, y) =>
      Array.from({ length: cols }, (_, x) => (g[y] && g[y][x]) || blank()));
    this.primary = fit(this.primary);
    this.alt = fit(this.alt);
    this.grid = this.grid === this.alt ? this.alt : this.primary;
    this.cols = cols; this.rows = rows;
    this.top = 0; this.bottom = rows - 1;
    this.cx = Math.min(this.cx, cols - 1); this.cy = Math.min(this.cy, rows - 1);
    this.markAll();
  }

  /** A row as plain text, for tests. */
  rowText(y) {
    return this.grid[y].map((c) => c.ch).join("");
  }
}

// Keys. Ordinary keys use xterm's legacy sequences; Cmd (Meta) chords, which
// croft binds heavily and a legacy sequence cannot carry, go as CSI u.
const LEGACY = {
  Enter: "\r", Tab: "\t", Backspace: "\x7f", Escape: "\x1b",
  ArrowUp: "\x1b[A", ArrowDown: "\x1b[B", ArrowRight: "\x1b[C", ArrowLeft: "\x1b[D",
  Home: "\x1b[H", End: "\x1b[F", PageUp: "\x1b[5~", PageDown: "\x1b[6~",
  Insert: "\x1b[2~", Delete: "\x1b[3~",
  F1: "\x1bOP", F2: "\x1bOQ", F3: "\x1bOR", F4: "\x1bOS", F5: "\x1b[15~", F6: "\x1b[17~",
  F7: "\x1b[18~", F8: "\x1b[19~", F9: "\x1b[20~", F10: "\x1b[21~", F11: "\x1b[23~", F12: "\x1b[24~",
};
const CSIU = { Enter: 13, Tab: 9, Backspace: 127, Escape: 27 };

export function encodeKey(e) {
  const mods = 1 + (e.shiftKey ? 1 : 0) + (e.altKey ? 2 : 0) + (e.ctrlKey ? 4 : 0) + (e.metaKey ? 8 : 0);
  if (e.metaKey) {
    const code = e.key.length === 1 ? e.key.toLowerCase().codePointAt(0) : CSIU[e.key];
    if (code !== undefined) return `\x1b[${code};${mods}u`;
  }
  if (LEGACY[e.key] !== undefined) {
    const seq = LEGACY[e.key];
    if (mods > 1 && seq.startsWith("\x1b[") && /[A-DHF]$/.test(seq)) return `\x1b[1;${mods}${seq.slice(-1)}`;
    if (e.key === "Tab" && e.shiftKey) return "\x1b[Z";
    return seq;
  }
  if (e.key.length === 1) {
    if (e.ctrlKey && /^[a-z@\[\\\]^_]$/i.test(e.key)) return String.fromCharCode(e.key.toUpperCase().charCodeAt(0) & 0x1f);
    return (e.altKey ? "\x1b" : "") + e.key;
  }
  return null;
}

