import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebglAddon } from '@xterm/addon-webgl';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import type { CellSize, XtermAdapter, XtermAdapterFactory, XtermCallbacks } from './adapter';
import { TERMINAL_FONT, TERMINAL_HISTORY_LINES, TERMINAL_THEME } from './settings';

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
    theme: {
      foreground: TERMINAL_THEME.foreground,
      background: TERMINAL_THEME.background,
      cursor: TERMINAL_THEME.cursor,
    },
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
  const addon = new WebglAddon();
  try {
    addon.onContextLoss(() => {
      addon.dispose();
      webgl = null;
    });
    terminal.loadAddon(addon);
    webgl = addon;
  } catch {
    try {
      addon.dispose();
    } catch {
      // The addon never finished loading; the DOM renderer stays.
    }
  }

  child.addEventListener('focusin', callbacks.onFocus);

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
