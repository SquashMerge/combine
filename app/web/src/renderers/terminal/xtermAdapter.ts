import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebglAddon } from '@xterm/addon-webgl';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import type { CellSize, XtermAdapter, XtermAdapterFactory, XtermCallbacks } from './adapter';
import {
  TERMINAL_FONT,
  TERMINAL_FONT_FACES,
  TERMINAL_HISTORY_LINES,
  TERMINAL_THEME,
} from './settings';

const encoder = new TextEncoder();

function isFocusReport(data: string): boolean {
  return data === '\x1b[I' || data === '\x1b[O';
}

export const createXtermAdapter: XtermAdapterFactory = (host, size, callbacks) => {
  const child = document.createElement('div');
  child.className = 'terminal-view__xterm';
  host.appendChild(child);

  const terminal = new Terminal({
    cols: size.cols,
    rows: size.rows,
    allowProposedApi: true,
    scrollback: TERMINAL_HISTORY_LINES,
    fontFamily: TERMINAL_FONT.family,
    fontSize: TERMINAL_FONT.size,
    theme: { ...TERMINAL_THEME },
    cursorBlink: false,
  });

  const unicode11 = new Unicode11Addon();
  terminal.loadAddon(unicode11);
  terminal.unicode.activeVersion = '11';

  // xterm answers DECXCPR through onData synchronously, right after this handler returns false.
  let capturing = false;
  terminal.parser.registerCsiHandler({ prefix: '?', final: 'n' }, (params) => {
    if (params[0] === 6) {
      capturing = true;
      queueMicrotask(() => {
        capturing = false;
      });
    }
    return false;
  });

  terminal.onData((data) => {
    if (capturing) {
      capturing = false;
      callbacks.onAutomaticReply(encoder.encode(data));
    } else if (isFocusReport(data)) {
      callbacks.onFocusReport(encoder.encode(data));
    } else {
      callbacks.onInput(encoder.encode(data));
    }
  });
  terminal.onBinary((data) => {
    callbacks.onInput(Uint8Array.from(data, (c) => c.charCodeAt(0) & 0xff));
  });
  terminal.attachCustomKeyEventHandler((event) => !event.metaKey);

  terminal.open(child);

  let webgl: WebglAddon | null = null;
  let addon: WebglAddon | undefined;
  try {
    const loading = new WebglAddon();
    addon = loading;
    loading.onContextLoss(() => {
      loading.dispose();
      webgl = null;
    });
    terminal.loadAddon(loading);
    webgl = loading;
  } catch {
    try {
      addon?.dispose();
    } catch {
      // The addon never finished loading; the DOM renderer stays.
    }
  }

  child.addEventListener('focusin', callbacks.onFocus);

  // A terminal opened before its face arrived (the first render goes ahead after 3 s) measured a
  // fallback cell. xterm measures again only when a font option changes, so once the face loads,
  // set an equivalent family and then the real one: each set measures, and a changed cell reaches
  // onCellSize through the next render, which refits the owner and resends the display facts.
  let disposed = false;
  const fonts = typeof document === 'undefined' ? undefined : document.fonts;
  if (fonts && !TERMINAL_FONT_FACES.every((spec) => fonts.check(spec))) {
    // Each face that arrives measures on its own, so a bold face that fails can't stop the
    // regular face from correcting the cell. A face that never arrives changes nothing.
    for (const spec of TERMINAL_FONT_FACES) {
      fonts.load(spec).then(
        (faces) => {
          if (disposed || faces.length === 0) return;
          terminal.options.fontFamily = TERMINAL_FONT.family.replace(/'/g, '"');
          terminal.options.fontFamily = TERMINAL_FONT.family;
        },
        () => {},
      );
    }
  }

  let lastCell: CellSize | null = null;
  terminal.onRender(() => {
    const screen = child.querySelector('.xterm-screen');
    if (!screen) return;
    const rect = screen.getBoundingClientRect();
    const width = rect.width / terminal.cols;
    const height = rect.height / terminal.rows;
    if (!(width > 0) || !(height > 0)) return;
    if (lastCell && lastCell.width === width && lastCell.height === height) return;
    lastCell = { width, height };
    callbacks.onCellSize(lastCell);
  });

  const adapter: XtermAdapter = {
    get renderer() {
      return webgl ? 'webgl' : 'dom';
    },
    write(bytes, done) {
      terminal.write(bytes, done);
    },
    resize(next) {
      terminal.resize(next.cols, next.rows);
    },
    focus() {
      terminal.focus();
    },
    hasFocus() {
      return child.contains(document.activeElement);
    },
    dispose() {
      disposed = true;
      child.removeEventListener('focusin', callbacks.onFocus);
      webgl?.dispose();
      webgl = null;
      unicode11.dispose();
      terminal.dispose();
      child.remove();
    },
  };
  return adapter;
};
