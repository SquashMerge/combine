import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { beforeAll, describe, expect, it } from 'vitest';
import { installTerminalDom } from '../support/terminalDom';

function write(terminal: Terminal, data: string | Uint8Array): Promise<void> {
  return new Promise((resolve) => terminal.write(data, resolve));
}

function firstLine(terminal: Terminal): string {
  return terminal.buffer.active.getLine(0)?.translateToString(true) ?? '';
}

describe('xterm under jsdom', () => {
  beforeAll(() => {
    installTerminalDom();
  });

  it('the real Terminal opens, writes, resizes and disposes under jsdom', async () => {
    const el = document.createElement('div');
    document.body.appendChild(el);
    const terminal = new Terminal({ cols: 80, rows: 24 });
    terminal.open(el);

    await write(terminal, 'hello');
    expect(firstLine(terminal)).toBe('hello');

    terminal.resize(100, 30);
    expect(terminal.cols).toBe(100);
    expect(terminal.rows).toBe(30);

    terminal.dispose();
    el.remove();
  });

  it('decodes UTF-8 split across writes', async () => {
    const el = document.createElement('div');
    document.body.appendChild(el);
    const terminal = new Terminal({ cols: 80, rows: 24 });
    terminal.open(el);

    const bytes = new TextEncoder().encode('é');
    await write(terminal, bytes.slice(0, 1));
    await write(terminal, bytes.slice(1));
    expect(firstLine(terminal)).toBe('é');

    terminal.dispose();
    el.remove();
  });

  it('requires the matchMedia stub', () => {
    const el = document.createElement('div');
    document.body.appendChild(el);
    const saved = window.matchMedia;
    try {
      delete (window as { matchMedia?: unknown }).matchMedia;
      expect(() => new Terminal().open(el)).toThrow(/matchMedia is not a function/);
    } finally {
      window.matchMedia = saved;
    }

    const terminal = new Terminal();
    expect(() => terminal.open(el)).not.toThrow();
    terminal.dispose();
    el.remove();
  });
});
