import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { XtermAdapter, XtermCallbacks } from '../../src/renderers/terminal/adapter';
import { installTerminalDom } from '../support/terminalDom';

const webgl = vi.hoisted(() => ({
  events: [] as string[],
  failOnLoad: false,
  contextLoss: [] as Array<() => void>,
}));

vi.mock('@xterm/addon-webgl', () => ({
  WebglAddon: class {
    activate(): void {
      if (webgl.failOnLoad) throw new Error('webgl unavailable');
      webgl.events.push('webgl:activate');
    }
    dispose(): void {
      const live = document.querySelector('.terminal-view__xterm .xterm') !== null;
      webgl.events.push(live ? 'webgl:dispose:terminal-live' : 'webgl:dispose');
    }
    onContextLoss(listener: () => void): { dispose(): void } {
      webgl.contextLoss.push(listener);
      return { dispose() {} };
    }
  },
}));

const { createXtermAdapter } = await import('../../src/renderers/terminal/xtermAdapter');

const decoder = new TextDecoder();

interface Harness {
  adapter: XtermAdapter;
  host: HTMLElement;
  inputs: string[];
  replies: string[];
  focusReports: string[];
  focus: ReturnType<typeof vi.fn>;
  term: () => HTMLTextAreaElement;
}

function make(): Harness {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const inputs: string[] = [];
  const replies: string[] = [];
  const focusReports: string[] = [];
  const focus = vi.fn();
  const callbacks: XtermCallbacks = {
    onInput: (b) => inputs.push(decoder.decode(b)),
    onAutomaticReply: (b) => replies.push(decoder.decode(b)),
    onFocusReport: (b) => focusReports.push(decoder.decode(b)),
    onFocus: focus,
    onCellSize: () => {},
  };
  const adapter = createXtermAdapter(host, { cols: 80, rows: 24 }, callbacks);
  return {
    adapter,
    host,
    inputs,
    replies,
    focusReports,
    focus,
    term: () => host.querySelector('textarea') as HTMLTextAreaElement,
  };
}

function write(adapter: XtermAdapter, text: string): Promise<void> {
  return new Promise((resolve) => adapter.write(new TextEncoder().encode(text), resolve));
}

describe('createXtermAdapter', () => {
  beforeAll(() => {
    installTerminalDom();
  });

  beforeEach(() => {
    webgl.events.length = 0;
    webgl.failOnLoad = false;
    webgl.contextLoss.length = 0;
    document.body.innerHTML = '';
  });

  it('loads the Unicode 11 addon and uses Unicode 11 widths', async () => {
    const h = make();
    await write(h.adapter, '\u{1F600}\x1b[?6n');
    await Promise.resolve();
    expect(h.replies).toEqual(['\x1b[?1;3R']);
    h.adapter.dispose();
  });

  it('loads the WebGL addon after open', () => {
    const h = make();
    expect(webgl.events).toEqual(['webgl:activate']);
    expect(h.adapter.renderer).toBe('webgl');
    expect(h.host.querySelector('.terminal-view__xterm .xterm')).not.toBeNull();
    h.adapter.dispose();
  });

  it('falls back to the DOM renderer when the WebGL addon fails to load', () => {
    webgl.failOnLoad = true;
    const h = make();
    expect(h.adapter.renderer).toBe('dom');
    h.adapter.dispose();
  });

  it('disposes the WebGL addon and keeps the DOM renderer on context loss', () => {
    const h = make();
    expect(h.adapter.renderer).toBe('webgl');
    webgl.contextLoss[0]?.();
    expect(webgl.events).toContain('webgl:dispose:terminal-live');
    expect(h.adapter.renderer).toBe('dom');
    h.adapter.dispose();
  });

  it('disposes the addon before the terminal', () => {
    const h = make();
    h.adapter.dispose();
    expect(webgl.events).toEqual(['webgl:activate', 'webgl:dispose:terminal-live']);
    expect(h.host.querySelector('.terminal-view__xterm')).toBeNull();
  });

  it('exposes no clear method', () => {
    const h = make();
    expect(Object.keys(h.adapter).sort()).toEqual(
      ['dispose', 'focus', 'hasFocus', 'renderer', 'resize', 'write'].sort(),
    );
    h.adapter.dispose();
  });

  it('routes a DECXCPR reply to onAutomaticReply, not onInput', async () => {
    const h = make();
    await write(h.adapter, '\x1b[?6n');
    await Promise.resolve();
    expect(h.replies).toEqual(['\x1b[?1;1R']);
    expect(h.inputs).toEqual([]);
    h.adapter.dispose();
  });

  it('routes typed Enter to onInput', () => {
    const h = make();
    h.term().dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true }),
    );
    expect(h.inputs).toEqual(['\r']);
    expect(h.replies).toEqual([]);
    h.adapter.dispose();
  });

  it('routes focus in and out reports to onFocusReport', async () => {
    const h = make();
    await write(h.adapter, '\x1b[?1004h');
    h.focusReports.length = 0;
    h.term().dispatchEvent(new FocusEvent('focus'));
    h.term().dispatchEvent(new FocusEvent('blur'));
    expect(h.focusReports).toEqual(['\x1b[I', '\x1b[O']);
    expect(h.inputs).toEqual([]);
    h.adapter.dispose();
  });

  it('does not consume Cmd key combinations', () => {
    const h = make();
    h.term().dispatchEvent(
      new KeyboardEvent('keydown', { key: 'k', code: 'KeyK', keyCode: 75, metaKey: true, bubbles: true }),
    );
    expect(h.inputs).toEqual([]);
    h.adapter.dispose();
  });

  it('calls done after the write is parsed', async () => {
    const h = make();
    const order: string[] = [];
    const finished = new Promise<void>((resolve) =>
      h.adapter.write(new TextEncoder().encode('\x1b[?6n'), () => {
        order.push(`done:${h.replies.length}`);
        resolve();
      }),
    );
    order.push('queued');
    await finished;
    expect(order[0]).toBe('queued');
    expect(order[1]).toBe('done:1');
    h.adapter.dispose();
  });
});
