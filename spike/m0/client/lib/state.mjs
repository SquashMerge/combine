// Snapshot and diff of an @xterm/headless terminal's full state, for M0 oracles.
// Public API where it exists; internals (pinned @xterm/headless 6.0.0) for the rest.
import xterm from '@xterm/headless';
import u11 from '@xterm/addon-unicode11';
export const { Terminal } = xterm;
// UNICODE=6 reproduces xterm.js's default widths; the default here is Unicode 11 (wide emoji).
const UNICODE = process.env.M0_UNICODE || '11';

export function newTerm(cols, rows) {
  const t = new Terminal({ cols, rows, allowProposedApi: true, scrollback: 10000 });
  if (UNICODE === '11') {
    t.loadAddon(new u11.Unicode11Addon());
    t.unicode.activeVersion = '11';
  }
  t.m0title = '';
  t.onTitleChange((s) => { t.m0title = s; });
  return t;
}

export function write(t, data) {
  return new Promise((res) => t.write(data, res));
}

function color(mode, value, isDefault) {
  if (isDefault) return 'default';
  // xterm.js keeps SGR 31 (P16) and 38;5;1 (P256) apart; both render palette entry 1.
  if (mode === 'p16' || mode === 'p256') return 'pal:' + value;
  return 'rgb:' + value.toString(16).padStart(6, '0');
}

function cellInfo(c) {
  const fgm = c.isFgRGB() ? 'rgb' : c.isFgPalette() ? (c.getFgColor() < 16 ? 'p16' : 'p256') : 'def';
  const bgm = c.isBgRGB() ? 'rgb' : c.isBgPalette() ? (c.getBgColor() < 16 ? 'p16' : 'p256') : 'def';
  return {
    ch: c.getChars() || ' ',
    w: c.getWidth(),
    fg: color(fgm, c.getFgColor(), c.isFgDefault()),
    bg: color(bgm, c.getBgColor(), c.isBgDefault()),
    attrs: [c.isBold() && 'bold', c.isDim() && 'dim', c.isItalic() && 'italic', c.isUnderline() && 'underline',
      c.isInverse() && 'inverse', c.isBlink() && 'blink', c.isInvisible() && 'invisible',
      c.isStrikethrough() && 'strike', c.isOverline() && 'overline'].filter(Boolean).join(','),
  };
}

function bufferRows(t, buf, rows) {
  const out = [];
  const cell = buf.getNullCell();
  for (let y = 0; y < rows; y++) {
    const line = buf.getLine(buf.viewportY + y);
    if (!line) { out.push({ text: '', cells: '' }); continue; }
    const cells = [];
    for (let x = 0; x < t.cols; x++) {
      line.getCell(x, cell);
      const i = cellInfo(cell);
      // Blank cells with default colours carry no state worth comparing.
      if (i.ch === ' ' && i.fg === 'default' && i.bg === 'default' && !i.attrs) continue;
      cells.push(`${x}:${i.ch}|${i.w}|${i.fg}|${i.bg}|${i.attrs}`);
    }
    out.push({ text: line.translateToString(true), wrapped: line.isWrapped, cells: cells.join(' ') });
  }
  return out;
}

function pen(t) {
  const a = t._core._inputHandler._curAttrData;
  // Reuse the cell decoder on a cell-like view of the pen.
  const fake = {
    getChars: () => '', getWidth: () => 1, isFgRGB: () => a.isFgRGB(), isBgRGB: () => a.isBgRGB(),
    isFgPalette: () => a.isFgPalette(), isBgPalette: () => a.isBgPalette(), getFgColor: () => a.getFgColor(),
    getBgColor: () => a.getBgColor(), isFgDefault: () => a.isFgDefault(), isBgDefault: () => a.isBgDefault(),
    isBold: () => a.isBold(), isDim: () => a.isDim(), isItalic: () => a.isItalic(), isUnderline: () => a.isUnderline(),
    isInverse: () => a.isInverse(), isBlink: () => a.isBlink(), isInvisible: () => a.isInvisible(),
    isStrikethrough: () => a.isStrikethrough(), isOverline: () => a.isOverline(),
  };
  const i = cellInfo(fake);
  return `${i.fg}|${i.bg}|${i.attrs}`;
}

export function snapshot(t) {
  const c = t._core;
  const act = t.buffer.active;
  const nb = c._bufferService.buffers.normal;
  const ab = c._bufferService.buffers.alt;
  const m = t.modes;
  const tabs = Object.keys(c.buffer.tabs).map(Number).sort((a, b) => a - b).join(',');
  return {
    size: `${t.cols}x${t.rows}`,
    activeBuffer: act.type,
    cursor: `${act.cursorX},${act.cursorY}`,
    cursorHidden: c.coreService.isCursorHidden,
    cursorStyle: `${c.coreService.decPrivateModes.cursorStyle ?? 'unset'}/${c.coreService.decPrivateModes.cursorBlink ?? 'unset'}`,
    modes: {
      applicationCursorKeys_DECCKM: m.applicationCursorKeysMode,
      applicationKeypad: m.applicationKeypadMode,
      bracketedPaste_2004: m.bracketedPasteMode,
      mouseTracking: m.mouseTrackingMode,
      mouseEncoding: c.coreMouseService.activeEncoding,
      sendFocus_1004: m.sendFocusMode,
      insertMode_IRM: m.insertMode,
      originMode_DECOM: m.originMode,
      wraparound_DECAWM: m.wraparoundMode,
      reverseWraparound: m.reverseWraparoundMode,
      synchronizedOutput: m.synchronizedOutputMode,
    },
    scrollRegion: `${c.buffer.scrollTop + 1};${c.buffer.scrollBottom + 1}`,
    // viewport-relative; xterm.js keeps an absolute row, and a never-saved cursor (row 0) scrolls off the top
    savedCursor: { normal: `${nb.savedX},${Math.max(0, nb.savedY - nb.ybase)}`, alt: `${ab.savedX},${Math.max(0, ab.savedY - ab.ybase)}` },
    pen: pen(t),
    charset: `g${c._charsetService.glevel}:${c._charsetService.charset ? 'dec-graphics' : 'ascii'}`,
    tabStops: tabs,
    title: t.m0title,
    activeRows: bufferRows(t, act, t.rows),
    normalRows: act.type === 'alternate' ? bufferRows(t, t.buffer.normal, t.rows) : 'same-as-active',
  };
}

// Returns a list of human-readable differences (empty: identical).
export function diff(ref, got, path = '') {
  const out = [];
  if (typeof ref !== 'object' || ref === null || typeof got !== 'object' || got === null) {
    if (JSON.stringify(ref) !== JSON.stringify(got)) out.push(`${path}: reference=${JSON.stringify(ref)} attach=${JSON.stringify(got)}`);
    return out;
  }
  const keys = new Set([...Object.keys(ref), ...Object.keys(got)]);
  for (const k of keys) out.push(...diff(ref[k], got[k], path ? `${path}.${k}` : k));
  return out;
}

export function screenText(t) {
  return bufferRows(t, t.buffer.active, t.rows).map((r) => r.text).join('\n');
}
