// Tests for croft web's terminal emulator; `node --test` runs them (the
// Rust test `web::page::tests::the_page_emulator_passes_its_tests` does).
import test from "node:test";
import assert from "node:assert/strict";
import { Term, charWidth, palette256, ATTR, encodeKey } from "./term.js";

const enc = (s) => new TextEncoder().encode(s);

test("absolute moves and SGR truecolor land where croft paints", () => {
  const t = new Term(20, 5);
  t.write(enc("\x1b[3;5H\x1b[1;38;2;10;20;30;48;5;196mhi\x1b[0m!"));
  assert.equal(t.rowText(2).slice(4, 7), "hi!");
  const h = t.grid[2][4];
  assert.deepEqual(h.fg, [10, 20, 30]);
  assert.deepEqual(h.bg, palette256(196));
  assert.ok(h.attrs & ATTR.BOLD);
  assert.equal(t.grid[2][6].fg, null, "SGR 0 reset the pen");
});

test("erases clear to the end of line and screen", () => {
  const t = new Term(10, 3);
  t.write(enc("abcdefghij\r\nklmnopqrst\r\nuvwxyzABCD"));
  t.write(enc("\x1b[2;4H\x1b[K"));
  assert.equal(t.rowText(1), "klm       ");
  t.write(enc("\x1b[1;3H\x1b[1K"));
  assert.equal(t.rowText(0), "   defghij");
  t.write(enc("\x1b[2J"));
  assert.equal(t.rowText(2).trim(), "");
});

test("a line feed at the bottom of a scroll region scrolls only the region", () => {
  const t = new Term(5, 4);
  t.write(enc("top\r\nr1\r\nr2\r\nbot"));
  t.write(enc("\x1b[2;3r\x1b[3;1H\nnew"));
  assert.equal(t.rowText(0).trim(), "top");
  assert.equal(t.rowText(1).trim(), "r2");
  assert.equal(t.rowText(2).trim(), "new");
  assert.equal(t.rowText(3).trim(), "bot");
});

test("the alternate screen is separate and restores the primary", () => {
  const t = new Term(8, 2);
  t.write(enc("shell"));
  t.write(enc("\x1b[?1049h\x1b[Hcroft"));
  assert.equal(t.rowText(0).trim(), "croft");
  t.write(enc("\x1b[?1049l"));
  assert.equal(t.rowText(0).trim(), "shell");
});

test("UTF-8 split across writes, wide glyphs and wrapping", () => {
  const t = new Term(4, 2);
  const bytes = enc("é中");
  t.write(bytes.slice(0, 1));
  t.write(bytes.slice(1, 3));
  t.write(bytes.slice(3));
  assert.equal(t.grid[0][0].ch, "é");
  assert.equal(t.grid[0][1].ch, "中");
  assert.equal(t.grid[0][1].w, 2);
  assert.equal(t.grid[0][2].w, 0, "the wide glyph's right half");
  t.write(enc("xyz"));
  assert.equal(t.grid[0][3].ch, "x");
  assert.equal(t.rowText(1).slice(0, 2), "yz", "wrapped at the edge");
  assert.equal(charWidth(0x301), 0);
});

test("cursor reports and device attributes are answered", () => {
  const t = new Term(10, 5);
  t.write(enc("\x1b[4;7H\x1b[6n\x1b[c"));
  assert.deepEqual(t.takeReplies(), ["\x1b[4;7R", "\x1b[?62;22c"]);
  assert.deepEqual(t.takeReplies(), []);
});

test("kitty images are placed at the cursor, chunked or whole, and deleted", () => {
  const t = new Term(10, 5);
  t.write(enc("\x1b[2;3H\x1b_Ga=T,f=100,c=2,r=1,m=1;AAAA\x1b\\\x1b_Gm=0;BBBB\x1b\\"));
  assert.equal(t.images.length, 1);
  assert.deepEqual([t.images[0].x, t.images[0].y, t.images[0].cols], [2, 1, 2]);
  assert.equal(t.images[0].data, "AAAABBBB");
  assert.equal(t.rowText(1).trim(), "", "graphics payloads are never printed");
  t.write(enc("\x1b_Ga=d\x1b\\"));
  assert.equal(t.images.length, 0);
});

test("unknown sequences are swallowed, not printed", () => {
  const t = new Term(10, 1);
  t.write(enc("\x1b]0;title\x07\x1bP+q\x1b\\\x1b[>4;1m\x1b(Bok"));
  assert.equal(t.rowText(0).trim(), "ok");
  assert.equal(t.title, "title");
});

test("keys encode as croft reads them", () => {
  const k = (key, m = {}) => encodeKey({ key, shiftKey: false, altKey: false, ctrlKey: false, metaKey: false, ...m });
  assert.equal(k("a"), "a");
  assert.equal(k("Enter"), "\r");
  assert.equal(k("ArrowUp"), "\x1b[A");
  assert.equal(k("ArrowUp", { shiftKey: true }), "\x1b[1;2A");
  assert.equal(k("c", { ctrlKey: true }), "\x03");
  assert.equal(k("x", { altKey: true }), "\x1bx");
  assert.equal(k("Tab", { shiftKey: true }), "\x1b[Z");
  // Cmd chords, which croft binds and legacy sequences cannot carry.
  assert.equal(k("p", { metaKey: true }), "\x1b[112;9u");
  assert.equal(k("F", { metaKey: true, shiftKey: true }), "\x1b[102;10u");
  assert.equal(k("Shift"), null, "a bare modifier sends nothing");
});

test("a resize keeps whichever screen is showing", () => {
  const t = new Term(10, 3);
  t.write(enc("shell\x1b[?1049h\x1b[Hcroft"));
  t.resize(12, 4);
  assert.equal(t.rowText(0).trim(), "croft", "still on the alternate screen after a resize");
  assert.equal(t.grid, t.alt);
  t.write(enc("\x1b[?1049l"));
  assert.equal(t.rowText(0).trim(), "shell");
});
