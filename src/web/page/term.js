// croft web (#342): the terminal model behind the page.
//
// A VT parser and screen grid for the output croft itself writes: cursor
// addressing, SGR colour (16, 256 and truecolor) and attributes, erase and
// insert/delete, scroll regions, the alternate screen, the private modes
// croft sets (mouse, bracketed paste, cursor), kitty keyboard flags, and the
// two inline image protocols croft emits (iTerm2 OSC 1337 and Kitty APC G).
// Queries croft sends (DA1, cursor position, kitty flags, window size) are
// answered through `reply`.
//
// No DOM here: app.js paints the grid and feeds input. Keeping the model
// DOM-free is what lets scripts/tests/web_term_test.js run it under node.
(function (root) {
  'use strict';

  // Colours are packed ints: DEFAULT, a palette index (0..255), or
  // TRUE | 0xRRGGBB.
  var DEFAULT = -1;
  var TRUE = 0x1000000;

  var BOLD = 1, DIM = 2, ITALIC = 4, UNDERLINE = 8, BLINK = 16,
    INVERSE = 32, HIDDEN = 64, STRIKE = 128;

  function blankCell(bg) {
    return { ch: ' ', w: 1, fg: DEFAULT, bg: bg === undefined ? DEFAULT : bg, fl: 0 };
  }

  function blankLine(cols, bg) {
    var line = new Array(cols);
    for (var i = 0; i < cols; i++) line[i] = blankCell(bg);
    return line;
  }

  // Display width of a code point: 0 for combining marks and joiners, 2 for
  // East Asian wide and most emoji, 1 otherwise. Private Use Area icons are
  // 1, as croft lays them out.
  var WIDE = [
    [0x1100, 0x115f], [0x231a, 0x231b], [0x2329, 0x232a], [0x23e9, 0x23ec],
    [0x23f0, 0x23f0], [0x23f3, 0x23f3], [0x25fd, 0x25fe], [0x2614, 0x2615],
    [0x2648, 0x2653], [0x267f, 0x267f], [0x2693, 0x2693], [0x26a1, 0x26a1],
    [0x26aa, 0x26ab], [0x26bd, 0x26be], [0x26c4, 0x26c5], [0x26ce, 0x26ce],
    [0x26d4, 0x26d4], [0x26ea, 0x26ea], [0x26f2, 0x26f3], [0x26f5, 0x26f5],
    [0x26fa, 0x26fa], [0x26fd, 0x26fd], [0x2705, 0x2705], [0x270a, 0x270b],
    [0x2728, 0x2728], [0x274c, 0x274c], [0x274e, 0x274e], [0x2753, 0x2755],
    [0x2757, 0x2757], [0x2795, 0x2797], [0x27b0, 0x27b0], [0x27bf, 0x27bf],
    [0x2b1b, 0x2b1c], [0x2b50, 0x2b50], [0x2b55, 0x2b55], [0x2e80, 0x303e],
    [0x3041, 0x33ff], [0x3400, 0x4dbf], [0x4e00, 0x9fff], [0xa000, 0xa4cf],
    [0xa960, 0xa97f], [0xac00, 0xd7a3], [0xf900, 0xfaff], [0xfe10, 0xfe19],
    [0xfe30, 0xfe6f], [0xff00, 0xff60], [0xffe0, 0xffe6], [0x1f004, 0x1f004],
    [0x1f0cf, 0x1f0cf], [0x1f18e, 0x1f18e], [0x1f191, 0x1f19a],
    [0x1f200, 0x1f251], [0x1f300, 0x1f64f], [0x1f680, 0x1f6ff],
    [0x1f7e0, 0x1f7eb], [0x1f90c, 0x1f9ff], [0x1fa70, 0x1faff],
    [0x20000, 0x3fffd]
  ];

  function charWidth(cp) {
    if (cp < 0x300) return 1;
    if ((cp >= 0x300 && cp <= 0x36f) || (cp >= 0x200b && cp <= 0x200f) ||
      (cp >= 0x20d0 && cp <= 0x20ff) || (cp >= 0xfe00 && cp <= 0xfe0f) ||
      (cp >= 0xfe20 && cp <= 0xfe2f) || cp === 0x2060 ||
      (cp >= 0xe0100 && cp <= 0xe01ef)) return 0;
    var lo = 0, hi = WIDE.length - 1;
    while (lo <= hi) {
      var mid = (lo + hi) >> 1;
      if (cp < WIDE[mid][0]) hi = mid - 1;
      else if (cp > WIDE[mid][1]) lo = mid + 1;
      else return 2;
    }
    return 1;
  }

  function Terminal(cols, rows, reply) {
    this.reply = reply || function () {};
    this.onTitle = function () {};
    this.onClipboard = function () {};
    this.onImage = function () {};
    this.onImageDelete = function () {};
    this.onBell = function () {};
    this.cellPx = { w: 8, h: 16 };
    this.utf8 = new TextDecoder('utf-8', { fatal: false });
    this.reset(cols, rows);
  }

  Terminal.prototype.reset = function (cols, rows) {
    this.cols = cols;
    this.rows = rows;
    this.main = this.makeScreen();
    this.alt = this.makeScreen();
    this.screen = this.main;
    this.altActive = false;
    this.state = 'ground';
    this.params = '';
    this.intermediate = '';
    this.osc = '';
    this.apc = '';
    this.pending = [];
    this.pen = { fg: DEFAULT, bg: DEFAULT, fl: 0 };
    this.modes = {
      cursorVisible: true, appCursor: false, autowrap: true, origin: false,
      mouse: 0, mouseSgr: false, bracketedPaste: false, focus: false
    };
    this.cursorShape = 'block';
    this.kittyFlags = [0];
    this.kittyChunks = null;
    this.dirty = new Array(rows).fill(true);
  };

  Terminal.prototype.makeScreen = function () {
    var lines = [];
    for (var i = 0; i < this.rows; i++) lines.push(blankLine(this.cols));
    return { lines: lines, x: 0, y: 0, wrapPending: false, top: 0, bottom: this.rows - 1, saved: null };
  };

  Terminal.prototype.kittyKeyboard = function () {
    return this.kittyFlags[this.kittyFlags.length - 1];
  };

  Terminal.prototype.resize = function (cols, rows) {
    var self = this;
    [this.main, this.alt].forEach(function (s) {
      while (s.lines.length < rows) s.lines.push(blankLine(cols));
      // Shrinking drops the rows below the cursor first, then from the top
      // so the cursor's row survives.
      var excess = s.lines.length - rows;
      if (excess > 0) {
        var below = Math.min(excess, s.lines.length - 1 - s.y);
        s.lines.splice(s.lines.length - below, below);
        var above = excess - below;
        if (above > 0) { s.lines.splice(0, above); s.y -= above; }
      }
      s.lines = s.lines.map(function (line) {
        if (line.length > cols) return line.slice(0, cols);
        while (line.length < cols) line.push(blankCell());
        return line;
      });
      s.x = Math.min(s.x, cols - 1);
      s.y = Math.min(s.y, rows - 1);
      s.top = 0;
      s.bottom = rows - 1;
      s.wrapPending = false;
    });
    this.cols = cols;
    this.rows = rows;
    this.dirty = new Array(rows).fill(true);
    self.onImageDelete(null);
  };

  Terminal.prototype.touch = function (y) {
    if (y >= 0 && y < this.rows) this.dirty[y] = true;
  };

  Terminal.prototype.touchAll = function () {
    for (var i = 0; i < this.rows; i++) this.dirty[i] = true;
  };

  // Feed bytes (Uint8Array) or a string from the session.
  Terminal.prototype.write = function (data) {
    var text = typeof data === 'string' ? data : this.utf8.decode(data, { stream: true });
    for (var i = 0; i < text.length; i++) {
      var code = text.codePointAt(i);
      if (code > 0xffff) i++;
      this.step(code);
    }
  };

  Terminal.prototype.step = function (c) {
    switch (this.state) {
      case 'ground':
        if (c === 0x1b) { this.state = 'esc'; this.intermediate = ''; return; }
        if (c < 0x20 || c === 0x7f) { this.control(c); return; }
        this.print(c);
        return;
      case 'esc':
        if (c === 0x5b) { this.state = 'csi'; this.params = ''; this.intermediate = ''; return; }
        if (c === 0x5d) { this.state = 'osc'; this.osc = ''; return; }
        if (c === 0x5f) { this.state = 'apc'; this.apc = ''; return; }
        if (c === 0x50 || c === 0x58 || c === 0x5e) { this.state = 'string'; return; }
        if (c >= 0x20 && c <= 0x2f) { this.intermediate += String.fromCharCode(c); return; }
        this.state = 'ground';
        this.escape(String.fromCharCode(c));
        return;
      case 'csi':
        if (c >= 0x30 && c <= 0x3f) { this.params += String.fromCharCode(c); return; }
        if (c >= 0x20 && c <= 0x2f) { this.intermediate += String.fromCharCode(c); return; }
        this.state = 'ground';
        if (c >= 0x40 && c <= 0x7e) this.csi(String.fromCharCode(c));
        return;
      case 'osc':
        if (c === 0x07) { this.state = 'ground'; this.oscDone(); return; }
        if (c === 0x1b) { this.state = 'oscEsc'; return; }
        this.osc += String.fromCodePoint(c);
        return;
      case 'oscEsc':
        this.state = 'ground';
        if (c === 0x5c) { this.oscDone(); return; }
        this.oscDone();
        this.step(0x1b);
        this.step(c);
        return;
      case 'apc':
        if (c === 0x1b) { this.state = 'apcEsc'; return; }
        if (c === 0x07) { this.state = 'ground'; this.apcDone(); return; }
        this.apc += String.fromCharCode(c);
        return;
      case 'apcEsc':
        this.state = 'ground';
        this.apcDone();
        if (c !== 0x5c) { this.step(0x1b); this.step(c); }
        return;
      case 'string':
        if (c === 0x1b) { this.state = 'stringEsc'; return; }
        if (c === 0x07) { this.state = 'ground'; }
        return;
      case 'stringEsc':
        this.state = c === 0x5c ? 'ground' : 'string';
        return;
    }
  };

  Terminal.prototype.control = function (c) {
    var s = this.screen;
    switch (c) {
      case 0x07: this.onBell(); break;
      case 0x08: if (s.x > 0) s.x--; s.wrapPending = false; break;
      case 0x09: s.x = Math.min(this.cols - 1, (Math.floor(s.x / 8) + 1) * 8); s.wrapPending = false; break;
      case 0x0a: case 0x0b: case 0x0c: this.lineFeed(); break;
      case 0x0d: s.x = 0; s.wrapPending = false; break;
    }
  };

  Terminal.prototype.lineFeed = function () {
    var s = this.screen;
    s.wrapPending = false;
    if (s.y === s.bottom) this.scrollUp(1);
    else if (s.y < this.rows - 1) s.y++;
  };

  Terminal.prototype.print = function (cp) {
    var s = this.screen;
    var w = charWidth(cp);
    var ch = String.fromCodePoint(cp);
    if (w === 0) {
      // Combining mark: joins the cell before the cursor.
      var px = s.wrapPending ? s.x : s.x - 1;
      if (px >= 0) {
        var prev = s.lines[s.y][px];
        if (prev.w === 0 && px > 0) prev = s.lines[s.y][px - 1];
        prev.ch += ch;
        this.touch(s.y);
      }
      return;
    }
    if (s.wrapPending) {
      s.wrapPending = false;
      if (this.modes.autowrap) { s.x = 0; this.lineFeed(); }
    }
    if (w === 2 && s.x === this.cols - 1) {
      if (this.modes.autowrap) {
        this.putCell(s.y, s.x, ' ', 1);
        s.x = 0;
        this.lineFeed();
      } else {
        return;
      }
    }
    this.putCell(s.y, s.x, ch, w);
    if (w === 2) this.putCell(s.y, s.x + 1, '', 0);
    if (s.x + w >= this.cols) {
      s.x = this.cols - 1;
      s.wrapPending = true;
    } else {
      s.x += w;
    }
  };

  Terminal.prototype.putCell = function (y, x, ch, w) {
    var line = this.screen.lines[y];
    // Overwriting half of a wide character blanks the other half.
    if (line[x].w === 2 && x + 1 < this.cols && w !== 0) line[x + 1] = blankCell(this.pen.bg);
    if (line[x].w === 0 && x > 0 && w !== 0) line[x - 1] = blankCell(this.pen.bg);
    line[x] = { ch: ch, w: w, fg: this.pen.fg, bg: this.pen.bg, fl: this.pen.fl };
    this.touch(y);
    this.onImageDelete({ x: x, y: y });
  };

  Terminal.prototype.escape = function (f) {
    var s = this.screen;
    if (this.intermediate) return;
    switch (f) {
      case '7': this.saveCursor(); break;
      case '8': this.restoreCursor(); break;
      case 'D': this.lineFeed(); break;
      case 'E': s.x = 0; this.lineFeed(); break;
      case 'M':
        s.wrapPending = false;
        if (s.y === s.top) this.scrollDown(1);
        else if (s.y > 0) s.y--;
        break;
      case 'c': this.reset(this.cols, this.rows); this.onImageDelete(null); break;
    }
  };

  Terminal.prototype.saveCursor = function () {
    var s = this.screen;
    s.saved = { x: s.x, y: s.y, pen: Object.assign({}, this.pen), wrapPending: s.wrapPending };
  };

  Terminal.prototype.restoreCursor = function () {
    var s = this.screen;
    if (!s.saved) { s.x = 0; s.y = 0; return; }
    s.x = Math.min(s.saved.x, this.cols - 1);
    s.y = Math.min(s.saved.y, this.rows - 1);
    s.wrapPending = s.saved.wrapPending;
    this.pen = Object.assign({}, s.saved.pen);
  };

  Terminal.prototype.scrollUp = function (n) {
    var s = this.screen;
    for (var i = 0; i < n; i++) {
      s.lines.splice(s.top, 1);
      s.lines.splice(s.bottom, 0, blankLine(this.cols, this.pen.bg));
    }
    for (var y = s.top; y <= s.bottom; y++) this.touch(y);
    this.onImageDelete({ top: s.top, bottom: s.bottom });
  };

  Terminal.prototype.scrollDown = function (n) {
    var s = this.screen;
    for (var i = 0; i < n; i++) {
      s.lines.splice(s.bottom, 1);
      s.lines.splice(s.top, 0, blankLine(this.cols, this.pen.bg));
    }
    for (var y = s.top; y <= s.bottom; y++) this.touch(y);
    this.onImageDelete({ top: s.top, bottom: s.bottom });
  };

  Terminal.prototype.eraseCells = function (y, from, to) {
    var line = this.screen.lines[y];
    for (var x = Math.max(0, from); x < Math.min(this.cols, to); x++) line[x] = blankCell(this.pen.bg);
    this.touch(y);
  };

  Terminal.prototype.csi = function (final) {
    var s = this.screen;
    var raw = this.params;
    var priv = '';
    if (raw && '?<=>'.indexOf(raw[0]) >= 0) { priv = raw[0]; raw = raw.slice(1); }
    var sub = raw.split(';');
    var p = sub.map(function (v) { var n = parseInt(v.split(':')[0], 10); return isNaN(n) ? 0 : n; });
    var p0 = p[0] || 0;
    var n1 = p0 || 1;
    var inter = this.intermediate;

    if (inter === ' ' && final === 'q') {
      this.cursorShape = p0 === 3 || p0 === 4 ? 'underline' : p0 === 5 || p0 === 6 ? 'bar' : 'block';
      this.touch(s.y);
      return;
    }
    if (inter) return;
    if (priv === '?' && (final === 'h' || final === 'l')) { this.privateMode(p, final === 'h'); return; }
    if (priv === '?' && final === 'u') { this.reply('\x1b[?' + this.kittyKeyboard() + 'u'); return; }
    if (priv === '>' && final === 'u') { this.kittyFlags.push(p0); return; }
    if (priv === '<' && final === 'u') {
      for (var k = 0; k < n1 && this.kittyFlags.length > 1; k++) this.kittyFlags.pop();
      return;
    }
    if (priv === '=' && final === 'u') {
      var mode = p[1] || 1;
      var top = this.kittyFlags.length - 1;
      var cur = this.kittyFlags[top];
      this.kittyFlags[top] = mode === 1 ? p0 : mode === 2 ? (cur | p0) : (cur & ~p0);
      return;
    }
    if (priv === '>' && final === 'q') { this.reply('\x1bP>|croft-web\x1b\\'); return; }
    if (priv === '>' && final === 'c') { this.reply('\x1b[>0;10;1c'); return; }
    if (priv) return;

    s.wrapPending = false;
    switch (final) {
      case 'A': s.y = Math.max(s.y < s.top ? 0 : s.top, s.y - n1); break;
      case 'B': s.y = Math.min(s.y > s.bottom ? this.rows - 1 : s.bottom, s.y + n1); break;
      case 'C': s.x = Math.min(this.cols - 1, s.x + n1); break;
      case 'D': s.x = Math.max(0, s.x - n1); break;
      case 'E': s.x = 0; s.y = Math.min(this.rows - 1, s.y + n1); break;
      case 'F': s.x = 0; s.y = Math.max(0, s.y - n1); break;
      case 'G': case '`': s.x = Math.min(this.cols - 1, n1 - 1); break;
      case 'd': s.y = Math.min(this.rows - 1, n1 - 1); break;
      case 'H': case 'f':
        s.y = Math.min(this.rows - 1, (p[0] || 1) - 1);
        s.x = Math.min(this.cols - 1, (p[1] || 1) - 1);
        break;
      case 'J': this.eraseDisplay(p0); break;
      case 'K':
        if (p0 === 0) this.eraseCells(s.y, s.x, this.cols);
        else if (p0 === 1) this.eraseCells(s.y, 0, s.x + 1);
        else this.eraseCells(s.y, 0, this.cols);
        break;
      case 'X': this.eraseCells(s.y, s.x, s.x + n1); break;
      case '@': {
        var line = s.lines[s.y];
        for (var i = 0; i < n1; i++) { line.splice(s.x, 0, blankCell(this.pen.bg)); line.pop(); }
        this.touch(s.y);
        break;
      }
      case 'P': {
        var l2 = s.lines[s.y];
        for (var j = 0; j < n1; j++) { l2.splice(s.x, 1); l2.push(blankCell(this.pen.bg)); }
        this.touch(s.y);
        break;
      }
      case 'L': this.insertLines(n1); break;
      case 'M': this.deleteLines(n1); break;
      case 'S': this.scrollUp(n1); break;
      case 'T': this.scrollDown(n1); break;
      case 'r':
        s.top = Math.max(0, (p[0] || 1) - 1);
        s.bottom = Math.min(this.rows - 1, (p[1] || this.rows) - 1);
        if (s.top >= s.bottom) { s.top = 0; s.bottom = this.rows - 1; }
        s.x = 0; s.y = 0;
        break;
      case 's': this.saveCursor(); break;
      case 'u': this.restoreCursor(); break;
      case 'm': this.sgr(sub); break;
      case 'c': if (p0 === 0) this.reply('\x1b[?62;22c'); break;
      case 'n':
        if (p0 === 6) this.reply('\x1b[' + (s.y + 1) + ';' + (s.x + 1) + 'R');
        else if (p0 === 5) this.reply('\x1b[0n');
        break;
      case 't':
        if (p0 === 14) this.reply('\x1b[4;' + this.rows * this.cellPx.h + ';' + this.cols * this.cellPx.w + 't');
        else if (p0 === 16) this.reply('\x1b[6;' + this.cellPx.h + ';' + this.cellPx.w + 't');
        else if (p0 === 18) this.reply('\x1b[8;' + this.rows + ';' + this.cols + 't');
        break;
    }
    this.touch(s.y);
  };

  Terminal.prototype.eraseDisplay = function (mode) {
    var s = this.screen;
    var y;
    if (mode === 0) {
      this.eraseCells(s.y, s.x, this.cols);
      for (y = s.y + 1; y < this.rows; y++) this.eraseCells(y, 0, this.cols);
    } else if (mode === 1) {
      this.eraseCells(s.y, 0, s.x + 1);
      for (y = 0; y < s.y; y++) this.eraseCells(y, 0, this.cols);
    } else {
      for (y = 0; y < this.rows; y++) this.eraseCells(y, 0, this.cols);
      if (mode === 2 || mode === 3) this.onImageDelete(null);
    }
  };

  Terminal.prototype.insertLines = function (n) {
    var s = this.screen;
    if (s.y < s.top || s.y > s.bottom) return;
    for (var i = 0; i < n; i++) {
      s.lines.splice(s.bottom, 1);
      s.lines.splice(s.y, 0, blankLine(this.cols, this.pen.bg));
    }
    for (var y = s.y; y <= s.bottom; y++) this.touch(y);
    s.x = 0;
  };

  Terminal.prototype.deleteLines = function (n) {
    var s = this.screen;
    if (s.y < s.top || s.y > s.bottom) return;
    for (var i = 0; i < n; i++) {
      s.lines.splice(s.y, 1);
      s.lines.splice(s.bottom, 0, blankLine(this.cols, this.pen.bg));
    }
    for (var y = s.y; y <= s.bottom; y++) this.touch(y);
    s.x = 0;
  };

  Terminal.prototype.privateMode = function (p, on) {
    var m = this.modes;
    for (var i = 0; i < p.length; i++) {
      switch (p[i]) {
        case 1: m.appCursor = on; break;
        case 6: m.origin = on; break;
        case 7: m.autowrap = on; break;
        case 25: m.cursorVisible = on; this.touch(this.screen.y); break;
        case 9: case 1000: case 1002: case 1003:
          m.mouse = on ? p[i] : (m.mouse === p[i] ? 0 : m.mouse);
          break;
        case 1004: m.focus = on; break;
        case 1006: m.mouseSgr = on; break;
        case 2004: m.bracketedPaste = on; break;
        case 47: case 1047: case 1049: this.setAlt(on, p[i] === 1049); break;
      }
    }
  };

  Terminal.prototype.setAlt = function (on, saveCursor) {
    if (on === this.altActive) return;
    if (on) {
      if (saveCursor) this.saveCursor();
      this.alt = this.makeScreen();
      this.screen = this.alt;
    } else {
      this.screen = this.main;
      if (saveCursor) this.restoreCursor();
    }
    this.altActive = on;
    this.touchAll();
    this.onImageDelete(null);
  };

  function extendedColor(parts, i) {
    // parts[i] is 38/48/58; returns [colour, next index].
    var kind = parseInt(parts[i + 1], 10);
    if (kind === 5) return [parseInt(parts[i + 2], 10) & 255, i + 3];
    if (kind === 2) {
      var r = parseInt(parts[i + 2], 10) || 0, g = parseInt(parts[i + 3], 10) || 0,
        b = parseInt(parts[i + 4], 10) || 0;
      return [TRUE | ((r & 255) << 16) | ((g & 255) << 8) | (b & 255), i + 5];
    }
    return [DEFAULT, i + 2];
  }

  Terminal.prototype.sgr = function (sub) {
    var pen = this.pen;
    if (sub.length === 1 && sub[0] === '') sub = ['0'];
    for (var i = 0; i < sub.length; i++) {
      var item = sub[i];
      if (item.indexOf(':') >= 0) {
        // Colon form: 38:2::r:g:b, 38:5:n, 4:3 (curly underline).
        var parts = item.split(':');
        var head = parseInt(parts[0], 10);
        if (head === 4) {
          if (parts[1] === '0') pen.fl &= ~UNDERLINE; else pen.fl |= UNDERLINE;
        } else if (head === 38 || head === 48) {
          var kind = parseInt(parts[1], 10);
          var colour = DEFAULT;
          if (kind === 5) colour = parseInt(parts[2], 10) & 255;
          else if (kind === 2) {
            var off = parts.length >= 6 ? 3 : 2;
            colour = TRUE | ((parseInt(parts[off], 10) & 255) << 16) |
              ((parseInt(parts[off + 1], 10) & 255) << 8) | (parseInt(parts[off + 2], 10) & 255);
          }
          if (head === 38) pen.fg = colour; else pen.bg = colour;
        }
        continue;
      }
      var n = parseInt(item, 10) || 0;
      if (n === 38 || n === 48 || n === 58) {
        var ext = extendedColor(sub, i);
        if (n === 38) pen.fg = ext[0]; else if (n === 48) pen.bg = ext[0];
        i = ext[1] - 1;
        continue;
      }
      if (n === 0) { pen.fg = DEFAULT; pen.bg = DEFAULT; pen.fl = 0; }
      else if (n === 1) pen.fl |= BOLD;
      else if (n === 2) pen.fl |= DIM;
      else if (n === 3) pen.fl |= ITALIC;
      else if (n === 4) pen.fl |= UNDERLINE;
      else if (n === 5 || n === 6) pen.fl |= BLINK;
      else if (n === 7) pen.fl |= INVERSE;
      else if (n === 8) pen.fl |= HIDDEN;
      else if (n === 9) pen.fl |= STRIKE;
      else if (n === 21 || n === 24) pen.fl &= ~UNDERLINE;
      else if (n === 22) pen.fl &= ~(BOLD | DIM);
      else if (n === 23) pen.fl &= ~ITALIC;
      else if (n === 25) pen.fl &= ~BLINK;
      else if (n === 27) pen.fl &= ~INVERSE;
      else if (n === 28) pen.fl &= ~HIDDEN;
      else if (n === 29) pen.fl &= ~STRIKE;
      else if (n >= 30 && n <= 37) pen.fg = n - 30;
      else if (n === 39) pen.fg = DEFAULT;
      else if (n >= 40 && n <= 47) pen.bg = n - 40;
      else if (n === 49) pen.bg = DEFAULT;
      else if (n >= 90 && n <= 97) pen.fg = n - 90 + 8;
      else if (n >= 100 && n <= 107) pen.bg = n - 100 + 8;
    }
  };

  Terminal.prototype.oscDone = function () {
    var text = this.osc;
    var semi = text.indexOf(';');
    var code = semi < 0 ? text : text.slice(0, semi);
    var rest = semi < 0 ? '' : text.slice(semi + 1);
    if (code === '0' || code === '2') this.onTitle(rest);
    else if (code === '52') {
      var data = rest.slice(rest.indexOf(';') + 1);
      if (data !== '?') this.onClipboard(data);
    } else if (code === '10' && rest === '?') this.reply('\x1b]10;rgb:d4d4/d4d4/d4d4\x1b\\');
    else if (code === '11' && rest === '?') this.reply('\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\');
    else if (code === '1337' && rest.indexOf('File=') === 0) this.itermImage(rest.slice(5));
  };

  // OSC 1337;File=k=v;...:base64 — one PNG, placed at the cursor.
  Terminal.prototype.itermImage = function (body) {
    var colon = body.indexOf(':');
    if (colon < 0) return;
    var args = {};
    body.slice(0, colon).split(';').forEach(function (kv) {
      var eq = kv.indexOf('=');
      if (eq > 0) args[kv.slice(0, eq)] = kv.slice(eq + 1);
    });
    if (args.inline !== '1') return;
    var s = this.screen;
    this.onImage({
      key: 'iterm:' + s.x + ',' + s.y,
      x: s.x, y: s.y,
      cols: parseInt(args.width, 10) || 1,
      rows: parseInt(args.height, 10) || 1,
      preserveAspect: args.preserveAspectRatio !== '0',
      data: body.slice(colon + 1)
    });
  };

  // APC G: Kitty graphics. croft sends `a=T` PNG transmits chunked with
  // `m=1`, and `a=d` deletes.
  Terminal.prototype.apcDone = function () {
    var body = this.apc;
    if (body[0] !== 'G') return;
    body = body.slice(1);
    var semi = body.indexOf(';');
    var head = semi < 0 ? body : body.slice(0, semi);
    var payload = semi < 0 ? '' : body.slice(semi + 1);
    var args = {};
    head.split(',').forEach(function (kv) {
      var eq = kv.indexOf('=');
      if (eq > 0) args[kv.slice(0, eq)] = kv.slice(eq + 1);
    });
    if (args.a === 'd') {
      var what = args.d || 'a';
      if (what === 'i' || what === 'I') this.onImageDelete({ kittyId: args.i });
      else this.onImageDelete({ kitty: true });
      return;
    }
    if (this.kittyChunks) {
      this.kittyChunks.data += payload;
      if (args.m !== '1') {
        var done = this.kittyChunks;
        this.kittyChunks = null;
        this.kittyImage(done.args, done.data, done.x, done.y);
      }
      return;
    }
    if (args.a !== 'T' || (args.f && args.f !== '100')) return;
    var s = this.screen;
    if (args.m === '1') {
      this.kittyChunks = { args: args, data: payload, x: s.x, y: s.y };
      return;
    }
    this.kittyImage(args, payload, s.x, s.y);
  };

  Terminal.prototype.kittyImage = function (args, data, x, y) {
    this.onImage({
      key: 'kitty:' + (args.i || '') + ':' + (args.p || x + ',' + y),
      kittyId: args.i,
      x: x, y: y,
      cols: parseInt(args.c, 10) || 1,
      rows: parseInt(args.r, 10) || 1,
      preserveAspect: true,
      z: parseInt(args.z, 10) || 0,
      data: data
    });
  };

  // The text of row y, for tests and accessibility.
  Terminal.prototype.rowText = function (y) {
    return this.screen.lines[y].map(function (c) { return c.ch; }).join('');
  };

  // ---- Input encoding ----------------------------------------------------

  var FUNCTIONAL = {
    Escape: 27, Enter: 13, Tab: 9, Backspace: 127, Insert: [2, '~'], Delete: [3, '~'],
    ArrowUp: 'A', ArrowDown: 'B', ArrowRight: 'C', ArrowLeft: 'D', Home: 'H', End: 'F',
    PageUp: [5, '~'], PageDown: [6, '~'],
    F1: 'P', F2: 'Q', F3: 'R', F4: 'S', F5: [15, '~'], F6: [17, '~'], F7: [18, '~'],
    F8: [19, '~'], F9: [20, '~'], F10: [21, '~'], F11: [23, '~'], F12: [24, '~']
  };

  // Kitty modifier bits, plus one as the protocol sends them.
  function modifierParam(ev) {
    return 1 + (ev.shiftKey ? 1 : 0) + (ev.altKey ? 2 : 0) + (ev.ctrlKey ? 4 : 0) + (ev.metaKey ? 8 : 0);
  }

  // The bytes a key press sends, or null when the page should let the
  // browser keep it. `ev` needs key, shiftKey, altKey, ctrlKey, metaKey.
  Terminal.prototype.encodeKey = function (ev) {
    var kitty = this.kittyKeyboard() & 1;
    var mods = modifierParam(ev);
    var f = FUNCTIONAL[ev.key];
    if (f !== undefined) {
      if (typeof f === 'number') {
        if (ev.key === 'Escape' && kitty) return '\x1b[27' + (mods > 1 ? ';' + mods : '') + 'u';
        if (mods > 1 && kitty) return '\x1b[' + f + ';' + mods + 'u';
        if (ev.key === 'Enter') return ev.altKey ? '\x1b\r' : '\r';
        if (ev.key === 'Tab') return ev.shiftKey ? '\x1b[Z' : '\t';
        if (ev.key === 'Backspace') return ev.ctrlKey ? '\x08' : ev.altKey ? '\x1b\x7f' : '\x7f';
        return '\x1b';
      }
      if (typeof f === 'string') {
        var ss3 = f >= 'P' && f <= 'S';
        if (mods > 1) return '\x1b[1;' + mods + f;
        if (ss3 || (this.modes.appCursor && 'ABCDHF'.indexOf(f) >= 0)) return '\x1bO' + f;
        return '\x1b[' + f;
      }
      return '\x1b[' + f[0] + (mods > 1 ? ';' + mods : '') + f[1];
    }
    if ([...ev.key].length !== 1) return null;
    var key = ev.key;
    var cp = key.codePointAt(0);
    if (kitty && (ev.ctrlKey || ev.altKey || ev.metaKey)) {
      // Disambiguated: the unshifted key with every modifier, so Cmd+K
      // reaches croft as CSI 107;9u.
      var base = key.length === 1 && key >= 'A' && key <= 'Z' ? key.toLowerCase().charCodeAt(0) : cp;
      return '\x1b[' + base + ';' + mods + 'u';
    }
    if (ev.metaKey) return null;
    if (ev.ctrlKey) {
      var lower = key.toLowerCase();
      if (lower >= 'a' && lower <= 'z') return (ev.altKey ? '\x1b' : '') + String.fromCharCode(lower.charCodeAt(0) - 96);
      var ctrl = { ' ': '\x00', '@': '\x00', '[': '\x1b', '\\': '\x1c', ']': '\x1d', '^': '\x1e', '_': '\x1f', '/': '\x1f' }[key];
      return ctrl === undefined ? null : ctrl;
    }
    return (ev.altKey ? '\x1b' : '') + key;
  };

  // An SGR (or X10) mouse report. button: 0 left, 1 middle, 2 right, 3
  // release, 64/65 wheel up/down; motion adds 32. x, y are zero-based
  // cells. Returns null when croft has not asked for this kind of event.
  Terminal.prototype.encodeMouse = function (button, x, y, release, motion, ev) {
    var mode = this.modes.mouse;
    if (!mode) return null;
    if (motion && mode === 1000) return null;
    if (motion && mode === 1002 && button === 3) return null;
    var code = button + (motion ? 32 : 0) +
      (ev && ev.shiftKey ? 4 : 0) + (ev && ev.altKey ? 8 : 0) + (ev && ev.ctrlKey ? 16 : 0);
    if (this.modes.mouseSgr) {
      return '\x1b[<' + code + ';' + (x + 1) + ';' + (y + 1) + (release ? 'm' : 'M');
    }
    if (release) code = 3 + (code & ~3);
    return '\x1b[M' + String.fromCharCode(32 + code, 33 + Math.min(x, 222), 33 + Math.min(y, 222));
  };

  Terminal.prototype.encodePaste = function (text) {
    text = text.replace(/\r?\n/g, '\r');
    return this.modes.bracketedPaste ? '\x1b[200~' + text + '\x1b[201~' : text;
  };

  var api = {
    Terminal: Terminal, charWidth: charWidth, DEFAULT: DEFAULT, TRUE: TRUE,
    BOLD: BOLD, DIM: DIM, ITALIC: ITALIC, UNDERLINE: UNDERLINE, INVERSE: INVERSE,
    HIDDEN: HIDDEN, STRIKE: STRIKE
  };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.CroftTerm = api;
})(this);
