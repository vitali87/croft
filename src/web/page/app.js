// croft web (#342): the page. Paints CroftTerm's grid on a canvas, lays
// inline images over it, and turns keyboard, mouse, wheel, touch and paste
// into the bytes croft reads. The session arrives over the WebSocket the
// listener serves (docs/WEB.md).
(function () {
  'use strict';
  var T = window.CroftTerm;

  // croft-icons holds only the Nerd Font glyphs croft draws; every other
  // character falls through to the monospace fonts after it.
  var FONT = '"croft-icons", "JetBrains Mono", "SF Mono", Menlo, "DejaVu Sans Mono", "Cascadia Mono", Consolas, monospace';
  var FONT_PX = 14;
  var DEFAULT_FG = '#d4d4d4';
  var DEFAULT_BG = '#1e1e1e';

  var PALETTE = [
    '#000000', '#cd3131', '#0dbc79', '#e5e510', '#2472c8', '#bc3fbc', '#11a8cd', '#e5e5e5',
    '#666666', '#f14c4c', '#23d18b', '#f5f543', '#3b8eea', '#d670d6', '#29b8db', '#ffffff'
  ];
  (function () {
    var steps = [0, 95, 135, 175, 215, 255];
    for (var r = 0; r < 6; r++) for (var g = 0; g < 6; g++) for (var b = 0; b < 6; b++) {
      PALETTE.push(rgb(steps[r], steps[g], steps[b]));
    }
    for (var i = 0; i < 24; i++) { var v = 8 + i * 10; PALETTE.push(rgb(v, v, v)); }
  })();

  function rgb(r, g, b) {
    return '#' + ((1 << 24) | (r << 16) | (g << 8) | b).toString(16).slice(1);
  }

  function colour(c, fallback) {
    if (c === T.DEFAULT) return fallback;
    if (c & T.TRUE) return rgb((c >> 16) & 255, (c >> 8) & 255, c & 255);
    return PALETTE[c] || fallback;
  }

  // ---- Token -------------------------------------------------------------
  // The printed URL carries the token in the fragment, which the browser
  // never sends. It moves into memory here and leaves the address bar.
  var token = null;
  var m = /(?:^|[#&])token=([0-9a-fA-F]+)/.exec(location.hash);
  if (m) {
    token = m[1];
    history.replaceState(null, '', location.pathname + location.search);
  }

  var screenEl = document.getElementById('screen');
  var canvas = document.getElementById('grid');
  var imagesEl = document.getElementById('images');
  var input = document.getElementById('input');
  var banner = document.getElementById('banner');
  var ctx = canvas.getContext('2d', { alpha: false });

  var cell = { w: 8, h: 16 };
  var dpr = 1;
  var term = new T.Terminal(80, 24, send);
  var socket = null;
  var lastCursor = null;

  function measure() {
    dpr = window.devicePixelRatio || 1;
    ctx.font = FONT_PX + 'px ' + FONT;
    var w = ctx.measureText('MMMMMMMMMM').width / 10;
    cell = { w: Math.max(1, Math.round(w * dpr) / dpr), h: Math.ceil(FONT_PX * 1.3) };
    term.cellPx = { w: Math.round(cell.w * dpr), h: Math.round(cell.h * dpr) };
  }

  function fit() {
    measure();
    var cols = Math.max(2, Math.floor(screenEl.clientWidth / cell.w));
    var rows = Math.max(2, Math.floor(screenEl.clientHeight / cell.h));
    canvas.width = Math.round(cols * cell.w * dpr);
    canvas.height = Math.round(rows * cell.h * dpr);
    canvas.style.width = cols * cell.w + 'px';
    canvas.style.height = rows * cell.h + 'px';
    if (cols !== term.cols || rows !== term.rows) {
      term.resize(cols, rows);
      control({ t: 'resize', cols: cols, rows: rows });
    }
    term.touchAll();
    placeImages();
  }

  // ---- Painting ----------------------------------------------------------
  function paint() {
    requestAnimationFrame(paint);
    var s = term.screen;
    var cursor = term.modes.cursorVisible ? s.y : -1;
    if (lastCursor !== null) term.touch(lastCursor);
    if (cursor >= 0) term.touch(cursor);
    var any = false;
    for (var y = 0; y < term.rows; y++) {
      if (!term.dirty[y]) continue;
      term.dirty[y] = false;
      paintRow(y);
      any = true;
    }
    if (any && cursor >= 0) paintCursor(s.x, s.y);
    lastCursor = cursor >= 0 ? cursor : null;
  }

  function paintRow(y) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.textBaseline = 'middle';
    var line = term.screen.lines[y];
    var top = y * cell.h;
    for (var x = 0; x < term.cols; x++) {
      var c = line[x];
      if (!c) continue;
      var fg = colour(c.fg, DEFAULT_FG);
      var bg = colour(c.bg, DEFAULT_BG);
      if (c.fl & T.INVERSE) { var t = fg; fg = bg; bg = t; }
      var width = (c.w === 2 ? 2 : 1) * cell.w;
      if (c.w === 0) continue;
      ctx.fillStyle = bg;
      ctx.fillRect(x * cell.w, top, width, cell.h);
      if (c.ch === ' ' || (c.fl & T.HIDDEN)) {
        if (c.fl & (T.UNDERLINE | T.STRIKE)) decorate(c, fg, x, top, width);
        continue;
      }
      ctx.font = (c.fl & T.ITALIC ? 'italic ' : '') + (c.fl & T.BOLD ? 'bold ' : '') + FONT_PX + 'px ' + FONT;
      ctx.fillStyle = fg;
      ctx.globalAlpha = c.fl & T.DIM ? 0.6 : 1;
      ctx.fillText(c.ch, x * cell.w, top + cell.h / 2);
      ctx.globalAlpha = 1;
      decorate(c, fg, x, top, width);
    }
  }

  function decorate(c, fg, x, top, width) {
    ctx.fillStyle = fg;
    if (c.fl & T.UNDERLINE) ctx.fillRect(x * cell.w, top + cell.h - 2, width, 1);
    if (c.fl & T.STRIKE) ctx.fillRect(x * cell.w, top + Math.floor(cell.h / 2), width, 1);
  }

  function paintCursor(x, y) {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = 'rgba(212,212,212,0.85)';
    var px = x * cell.w, py = y * cell.h;
    if (term.cursorShape === 'bar') ctx.fillRect(px, py, 2, cell.h);
    else if (term.cursorShape === 'underline') ctx.fillRect(px, py + cell.h - 2, cell.w, 2);
    else ctx.fillRect(px, py, cell.w, cell.h);
  }

  // ---- Images ------------------------------------------------------------
  var images = {};

  term.onImage = function (img) {
    var el = images[img.key] && images[img.key].el;
    if (!el) {
      el = document.createElement('img');
      el.alt = '';
      el.draggable = false;
      imagesEl.appendChild(el);
    }
    el.src = 'data:image/png;base64,' + img.data;
    el.style.objectFit = img.preserveAspect ? 'contain' : 'fill';
    el.style.zIndex = img.z < 0 ? '0' : '1';
    images[img.key] = { el: el, img: img };
    placeImage(images[img.key]);
  };

  term.onImageDelete = function (what) {
    Object.keys(images).forEach(function (key) {
      var entry = images[key];
      var img = entry.img;
      var gone = what === null ||
        (what.kitty && key.indexOf('kitty:') === 0) ||
        (what.kittyId !== undefined && img.kittyId === what.kittyId) ||
        (what.x !== undefined && what.x >= img.x && what.x < img.x + img.cols &&
          what.y >= img.y && what.y < img.y + img.rows) ||
        (what.top !== undefined && img.y + img.rows > what.top && img.y <= what.bottom);
      if (gone) {
        entry.el.remove();
        delete images[key];
      }
    });
  };

  function placeImage(entry) {
    var s = entry.el.style, img = entry.img;
    s.left = img.x * cell.w + 'px';
    s.top = img.y * cell.h + 'px';
    s.width = img.cols * cell.w + 'px';
    s.height = img.rows * cell.h + 'px';
  }

  function placeImages() {
    Object.keys(images).forEach(function (k) { placeImage(images[k]); });
  }

  // ---- Other things croft asks for ----------------------------------------
  term.onTitle = function (title) { document.title = title || 'croft'; };
  term.onClipboard = function (b64) {
    var text;
    try {
      text = new TextDecoder().decode(Uint8Array.from(atob(b64), function (ch) { return ch.charCodeAt(0); }));
    } catch (e) { return; }
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).catch(function () {});
    } else {
      notice('Copy needs a secure context (HTTPS or localhost); the text was not copied.', 4000);
    }
  };

  // ---- Connection ----------------------------------------------------------
  var encoder = new TextEncoder();

  function send(text) {
    if (socket && socket.readyState === 1) socket.send(encoder.encode(text));
  }

  function control(msg) {
    if (socket && socket.readyState === 1) socket.send(JSON.stringify(msg));
  }

  function notice(text, ms) {
    banner.textContent = text;
    banner.hidden = !text;
    if (ms) setTimeout(function () { if (banner.textContent === text) banner.hidden = true; }, ms);
  }

  function connect() {
    if (!token) {
      notice('No token: open the address croft web printed, including its #token= part.');
      return;
    }
    var url = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/';
    socket = new WebSocket(url, ['croft', 'croft.token.' + token]);
    socket.binaryType = 'arraybuffer';
    socket.onopen = function () {
      notice('');
      control({ t: 'hello', name: 'browser', cols: term.cols, rows: term.rows });
      input.focus();
    };
    socket.onmessage = function (ev) {
      if (typeof ev.data !== 'string') { term.write(new Uint8Array(ev.data)); return; }
      var msg;
      try { msg = JSON.parse(ev.data); } catch (e) { return; }
      if (msg.t === 'exit') notice('The session ended.');
    };
    socket.onclose = function () {
      if (banner.hidden) notice('Disconnected from the session. Reload to reconnect.');
    };
  }

  // ---- Keyboard ------------------------------------------------------------
  // Keys with a modifier, and every non-text key, are encoded from keydown.
  // Plain text arrives through the textarea's input events instead, so IME
  // composition, dead keys and phone keyboards produce what they show.
  var composing = false;

  input.addEventListener('keydown', function (ev) {
    if (ev.isComposing || composing) return;
    var text = ev.key.length === 1 || /^.$/u.test(ev.key);
    if (text && !ev.ctrlKey && !ev.altKey && !ev.metaKey) return;
    var bytes = term.encodeKey(ev);
    if (bytes === null) return;
    ev.preventDefault();
    send(bytes);
  });

  input.addEventListener('compositionstart', function () { composing = true; });
  input.addEventListener('compositionend', function (ev) {
    composing = false;
    if (ev.data) send(ev.data);
    input.value = '';
  });

  input.addEventListener('input', function (ev) {
    if (composing) return;
    if (ev.inputType === 'insertText' && ev.data) send(ev.data);
    else if (ev.inputType === 'insertLineBreak') send('\r');
    else if (ev.inputType === 'deleteContentBackward') send('\x7f');
    input.value = '';
  });

  input.addEventListener('paste', function (ev) {
    ev.preventDefault();
    var text = ev.clipboardData && ev.clipboardData.getData('text/plain');
    if (text) send(term.encodePaste(text));
  });

  input.addEventListener('focus', function () { if (term.modes.focus) send('\x1b[I'); });
  input.addEventListener('blur', function () { if (term.modes.focus) send('\x1b[O'); });

  // ---- Mouse, wheel and touch ----------------------------------------------
  var buttons = { 0: 0, 1: 1, 2: 2 };
  var held = -1;

  function cellAt(ev) {
    var r = canvas.getBoundingClientRect();
    return {
      x: Math.max(0, Math.min(term.cols - 1, Math.floor((ev.clientX - r.left) / cell.w))),
      y: Math.max(0, Math.min(term.rows - 1, Math.floor((ev.clientY - r.top) / cell.h)))
    };
  }

  function mouse(button, ev, release, motion) {
    var at = cellAt(ev);
    var bytes = term.encodeMouse(button, at.x, at.y, release, motion, ev);
    if (bytes !== null) send(bytes);
    return bytes !== null;
  }

  screenEl.addEventListener('mousedown', function (ev) {
    if (buttons[ev.button] === undefined) return;
    held = buttons[ev.button];
    if (mouse(held, ev, false, false)) ev.preventDefault();
    input.focus();
  });
  window.addEventListener('mouseup', function (ev) {
    if (held < 0) return;
    mouse(term.modes.mouseSgr ? held : 3, ev, true, false);
    held = -1;
  });
  screenEl.addEventListener('mousemove', function (ev) {
    mouse(held >= 0 ? held : 3, ev, false, true);
  });
  screenEl.addEventListener('contextmenu', function (ev) {
    if (term.modes.mouse) ev.preventDefault();
  });

  var wheelRest = 0;
  screenEl.addEventListener('wheel', function (ev) {
    ev.preventDefault();
    var lines = ev.deltaMode === 1 ? ev.deltaY : ev.deltaY / cell.h;
    wheelRest += lines;
    while (Math.abs(wheelRest) >= 1) {
      var up = wheelRest < 0;
      if (!mouse(up ? 64 : 65, ev, false, false)) send(up ? '\x1b[A' : '\x1b[B');
      wheelRest += up ? 1 : -1;
    }
  }, { passive: false });

  var touchY = null;
  screenEl.addEventListener('touchstart', function (ev) {
    if (ev.touches.length === 1) touchY = ev.touches[0].clientY;
  }, { passive: true });
  screenEl.addEventListener('touchmove', function (ev) {
    if (touchY === null || ev.touches.length !== 1) return;
    var t = ev.touches[0];
    var moved = (touchY - t.clientY) / cell.h;
    while (Math.abs(moved) >= 1) {
      var up = moved < 0;
      mouse(up ? 64 : 65, t, false, false);
      moved += up ? 1 : -1;
      touchY += (up ? -1 : 1) * cell.h;
    }
    ev.preventDefault();
  }, { passive: false });
  screenEl.addEventListener('touchend', function (ev) {
    if (touchY !== null && ev.changedTouches.length === 1) input.focus();
    touchY = null;
  });

  // ---- Start ---------------------------------------------------------------
  new ResizeObserver(fit).observe(screenEl);
  window.addEventListener('focus', function () { input.focus(); });
  fit();
  requestAnimationFrame(paint);
  // The icon font loads only when something asks for it; ask now, then
  // repaint with it.
  if (document.fonts) {
    document.fonts.load(FONT_PX + 'px "croft-icons"', '\uea83').then(fit, function () {});
    document.fonts.ready.then(fit);
  }
  connect();
})();
