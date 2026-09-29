// Tests for croft web's terminal model (src/web/page/term.js, #342).
// Run: node scripts/tests/web_term_test.js  (test_web_page.py runs it).
'use strict';
const assert = require('assert');
const path = require('path');
const T = require(path.join(__dirname, '..', '..', 'src', 'web', 'page', 'term.js'));

const tests = [];
const test = (name, fn) => tests.push([name, fn]);

function term(cols = 20, rows = 5) {
  const replies = [];
  const t = new T.Terminal(cols, rows, (s) => replies.push(s));
  t.replies = replies;
  return t;
}

test('printable text lands at the cursor and advances it', () => {
  const t = term();
  t.write('hello');
  assert.strictEqual(t.rowText(0).trimEnd(), 'hello');
  assert.deepStrictEqual([t.screen.x, t.screen.y], [5, 0]);
});

test('CUP addresses rows and columns from one', () => {
  const t = term();
  t.write('\x1b[3;4Hx');
  assert.strictEqual(t.screen.lines[2][3].ch, 'x');
  t.write('\x1b[H');
  assert.deepStrictEqual([t.screen.x, t.screen.y], [0, 0]);
});

test('bytes split across writes decode as one character', () => {
  const t = term();
  const bytes = Buffer.from('é\u{ea7f}', 'utf8');
  for (const b of bytes) t.write(Uint8Array.of(b));
  assert.strictEqual(t.rowText(0).slice(0, 2), 'é\u{ea7f}');
});

test('an escape sequence split across writes still parses', () => {
  const t = term();
  t.write('\x1b[3');
  t.write('8;2;1;2;3mA');
  assert.strictEqual(t.screen.lines[0][0].fg, T.TRUE | 0x010203);
});

test('SGR sets 16, 256 and truecolor, attributes, and resets', () => {
  const t = term();
  t.write('\x1b[31;44;1;4ma\x1b[38;5;200;48;2;10;20;30mb\x1b[0mc\x1b[92;7md');
  const [a, b, c, d] = t.screen.lines[0];
  assert.strictEqual(a.fg, 1);
  assert.strictEqual(a.bg, 4);
  assert.strictEqual(a.fl, T.BOLD | T.UNDERLINE);
  assert.strictEqual(b.fg, 200);
  assert.strictEqual(b.bg, T.TRUE | (10 << 16) | (20 << 8) | 30);
  assert.strictEqual(c.fg, T.DEFAULT);
  assert.strictEqual(c.fl, 0);
  assert.strictEqual(d.fg, 10);
  assert.strictEqual(d.fl, T.INVERSE);
});

test('the colon form of truecolor and styled underline are understood', () => {
  const t = term();
  t.write('\x1b[38:2::1:2:3;4:3mx\x1b[4:0my');
  assert.strictEqual(t.screen.lines[0][0].fg, T.TRUE | 0x010203);
  assert.ok(t.screen.lines[0][0].fl & T.UNDERLINE);
  assert.ok(!(t.screen.lines[0][1].fl & T.UNDERLINE));
});

test('erase in line and display blank with the current background', () => {
  const t = term(10, 3);
  t.write('abcdefghij\x1b[1;4H\x1b[44m\x1b[K');
  assert.strictEqual(t.rowText(0), 'abc       ');
  assert.strictEqual(t.screen.lines[0][5].bg, 4);
  t.write('\x1b[2J');
  assert.strictEqual(t.rowText(0).trim(), '');
});

test('the last column waits to wrap until the next character', () => {
  const t = term(5, 3);
  t.write('abcde');
  assert.deepStrictEqual([t.screen.x, t.screen.y], [4, 0]);
  t.write('f');
  assert.strictEqual(t.rowText(1)[0], 'f');
});

test('a line feed at the bottom of the scroll region scrolls it', () => {
  const t = term(5, 4);
  t.write('\x1b[2;3r');
  t.write('\x1b[2;1Hone\r\ntwo\r\nthree');
  assert.strictEqual(t.rowText(1).trim(), 'two');
  assert.strictEqual(t.rowText(2).trim(), 'three');
  assert.strictEqual(t.rowText(3).trim(), '');
});

test('insert and delete characters and lines shift the right cells', () => {
  const t = term(6, 3);
  t.write('abcdef\x1b[1;2H\x1b[2@');
  assert.strictEqual(t.rowText(0), 'a  bcd');
  t.write('\x1b[1;1H\x1b[3P');
  assert.strictEqual(t.rowText(0), 'bcd   ');
  t.write('\x1b[2;1Hxx\x1b[1;1H\x1b[L');
  assert.strictEqual(t.rowText(0).trim(), '');
  assert.strictEqual(t.rowText(1).trim(), 'bcd');
  assert.strictEqual(t.rowText(2).trim(), 'xx');
});

test('wide characters take two cells and are not split by the margin', () => {
  const t = term(5, 2);
  t.write('abcd中');
  assert.strictEqual(t.rowText(0), 'abcd ');
  assert.strictEqual(t.screen.lines[1][0].ch, '中');
  assert.strictEqual(t.screen.lines[1][0].w, 2);
  assert.strictEqual(t.screen.lines[1][1].w, 0);
});

test('a combining mark joins the character before it', () => {
  const t = term();
  t.write('éx');
  assert.strictEqual(t.screen.lines[0][0].ch, 'é');
  assert.strictEqual(t.screen.lines[0][1].ch, 'x');
});

test('the alternate screen is separate and restores the cursor', () => {
  const t = term();
  t.write('main\x1b[?1049h');
  assert.ok(t.altActive);
  assert.strictEqual(t.rowText(0).trim(), '');
  t.write('alt\x1b[?1049l');
  assert.ok(!t.altActive);
  assert.strictEqual(t.rowText(0).trim(), 'main');
  assert.strictEqual(t.screen.x, 4);
});

test('private modes croft sets are tracked', () => {
  const t = term();
  t.write('\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[?25l\x1b[5 q');
  assert.strictEqual(t.modes.mouse, 1003);
  assert.ok(t.modes.mouseSgr && t.modes.bracketedPaste);
  assert.ok(!t.modes.cursorVisible);
  assert.strictEqual(t.cursorShape, 'bar');
  t.write('\x1b[?1003l\x1b[?1002l\x1b[?1000l');
  assert.strictEqual(t.modes.mouse, 0);
});

test('queries croft sends are answered', () => {
  const t = term(80, 24);
  t.write('\x1b[3;7H\x1b[6n\x1b[c\x1b[?u\x1b[18t');
  assert.deepStrictEqual(t.replies, ['\x1b[3;7R', '\x1b[?62;22c', '\x1b[?0u', '\x1b[8;24;80t']);
});

test('kitty keyboard flags push, pop and set', () => {
  const t = term();
  t.write('\x1b[>3u');
  assert.strictEqual(t.kittyKeyboard(), 3);
  t.write('\x1b[<u');
  assert.strictEqual(t.kittyKeyboard(), 0);
  // croft re-asserts with the SET form on every resize of a persistent
  // session; it must not stack.
  t.write('\x1b[=3;1u\x1b[=3;1u');
  assert.strictEqual(t.kittyKeyboard(), 3);
  assert.strictEqual(t.kittyFlags.length, 1);
});

test('keys encode as croft expects, with and without kitty flags', () => {
  const t = term();
  const k = (key, mods = {}) => t.encodeKey(Object.assign({ key }, mods));
  assert.strictEqual(k('a'), 'a');
  assert.strictEqual(k('Enter'), '\r');
  assert.strictEqual(k('Backspace'), '\x7f');
  assert.strictEqual(k('ArrowUp'), '\x1b[A');
  assert.strictEqual(k('ArrowUp', { shiftKey: true }), '\x1b[1;2A');
  assert.strictEqual(k('F5'), '\x1b[15~');
  assert.strictEqual(k('F1'), '\x1bOP');
  assert.strictEqual(k('c', { ctrlKey: true }), '\x03');
  assert.strictEqual(k('x', { altKey: true }), '\x1bx');
  assert.strictEqual(k('k', { metaKey: true }), null, 'no legacy encoding for Cmd');
  assert.strictEqual(k('Shift'), null);
  t.write('\x1b[?1h');
  assert.strictEqual(k('ArrowUp'), '\x1bOA');
  t.write('\x1b[=3;1u');
  assert.strictEqual(k('k', { metaKey: true }), '\x1b[107;9u', 'Cmd+K');
  assert.strictEqual(k('K', { metaKey: true, shiftKey: true }), '\x1b[107;10u', 'Cmd+Shift+K');
  assert.strictEqual(k('c', { ctrlKey: true }), '\x1b[99;5u');
  assert.strictEqual(k('Escape'), '\x1b[27u');
  assert.strictEqual(k('a'), 'a', 'plain text stays text');
});

test('mouse reports follow the mode croft enabled', () => {
  const t = term();
  assert.strictEqual(t.encodeMouse(0, 4, 2, false, false), null, 'no mouse mode');
  t.write('\x1b[?1000h\x1b[?1006h');
  assert.strictEqual(t.encodeMouse(0, 4, 2, false, false), '\x1b[<0;5;3M');
  assert.strictEqual(t.encodeMouse(0, 4, 2, true, false), '\x1b[<0;5;3m');
  assert.strictEqual(t.encodeMouse(0, 4, 2, false, true), null, '1000 reports no motion');
  assert.strictEqual(t.encodeMouse(64, 0, 0, false, false), '\x1b[<64;1;1M');
  t.write('\x1b[?1003h');
  assert.strictEqual(t.encodeMouse(3, 1, 1, false, true), '\x1b[<35;2;2M');
});

test('paste is bracketed when croft asked for it', () => {
  const t = term();
  assert.strictEqual(t.encodePaste('a\nb'), 'a\rb');
  t.write('\x1b[?2004h');
  assert.strictEqual(t.encodePaste('x'), '\x1b[200~x\x1b[201~');
});

test('an iTerm2 inline image is placed at the cursor', () => {
  const t = term();
  const images = [];
  t.onImage = (img) => images.push(img);
  t.write('\x1b[2;3H\x1b]1337;File=inline=1;size=3;width=4;height=2;preserveAspectRatio=0:QUJD\x07');
  assert.strictEqual(images.length, 1);
  assert.deepStrictEqual(
    [images[0].x, images[0].y, images[0].cols, images[0].rows, images[0].data, images[0].preserveAspect],
    [2, 1, 4, 2, 'QUJD', false]
  );
  assert.deepStrictEqual([t.screen.x, t.screen.y], [2, 1], 'the cursor stays');
});

test('a chunked kitty image is reassembled and deletes are reported', () => {
  const t = term();
  const images = [];
  const deletes = [];
  t.onImage = (img) => images.push(img);
  t.onImageDelete = (d) => deletes.push(d);
  t.write('\x1b[3;5H\x1b_Gf=100,a=T,c=6,r=3,C=1,q=2,i=7,p=1,m=1;QUJD\x1b\\\x1b_Gm=0;REVG\x1b\\');
  assert.strictEqual(images.length, 1);
  assert.deepStrictEqual(
    [images[0].x, images[0].y, images[0].cols, images[0].rows, images[0].data, images[0].key],
    [4, 2, 6, 3, 'QUJDREVG', 'kitty:7:1']
  );
  t.write('\x1b_Ga=d,d=i,i=7,q=2\x1b\\\x1b_Ga=d,d=a,q=2\x1b\\');
  assert.deepStrictEqual(deletes.slice(-2), [{ kittyId: '7' }, { kitty: true }]);
});

test('writing a cell reports it so images under it can go', () => {
  const t = term();
  const deletes = [];
  t.onImageDelete = (d) => deletes.push(d);
  t.write('\x1b[2;2Hx');
  assert.deepStrictEqual(deletes, [{ x: 1, y: 1 }]);
});

test('OSC titles and clipboard writes are handed to the page', () => {
  const t = term();
  let title = null;
  let clip = null;
  t.onTitle = (s) => { title = s; };
  t.onClipboard = (s) => { clip = s; };
  t.write('\x1b]0;croft — main.rs\x07\x1b]52;c;aGk=\x1b\\');
  assert.strictEqual(title, 'croft — main.rs');
  assert.strictEqual(clip, 'aGk=');
});

test('a resize keeps the cursor row and clamps the cursor', () => {
  const t = term(10, 4);
  t.write('abc\x1b[1;10H');
  t.resize(5, 2);
  assert.strictEqual(t.rowText(0), 'abc  ', 'rows below the cursor go first');
  assert.deepStrictEqual([t.screen.x, t.screen.y], [4, 0]);
  const u = term(10, 4);
  u.write('top\x1b[4;1Hbottom');
  u.resize(10, 2);
  assert.strictEqual(u.rowText(1).trim(), 'bottom', 'then rows above it');
  assert.strictEqual(u.screen.y, 1);
});

test('unknown sequences and DCS strings are swallowed', () => {
  const t = term();
  t.write('\x1bP+q544e\x1b\\\x1b[?999h\x1b(Bok');
  assert.strictEqual(t.rowText(0).trimEnd(), 'ok');
});

let failed = 0;
for (const [name, fn] of tests) {
  try {
    fn();
    console.log('ok   ' + name);
  } catch (e) {
    failed++;
    console.log('FAIL ' + name + '\n     ' + (e && e.message));
  }
}
console.log(`${tests.length - failed}/${tests.length} passed`);
process.exit(failed ? 1 : 0);
