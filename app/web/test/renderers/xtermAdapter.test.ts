import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import type { XtermAdapter, XtermCallbacks } from '../../src/renderers/terminal/adapter';
import { installTerminalDom } from '../support/terminalDom';

const webgl = vi.hoisted(() => ({
  events: [] as string[],
  failOnLoad: false,
  failOnConstruct: false,
  contextLoss: [] as Array<() => void>,
}));

const terminals = vi.hoisted(
  () =>
    [] as Array<{
      _core: {
        coreService: { triggerBinaryEvent(data: string): void };
        _charSizeService: { measure(): void };
      };
      options: { fontFamily?: string };
    }>,
);

vi.mock('@xterm/xterm', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@xterm/xterm')>();
  return {
    ...actual,
    Terminal: class extends actual.Terminal {
      constructor(options?: ConstructorParameters<typeof actual.Terminal>[0]) {
        super(options);
        terminals.push(this as unknown as (typeof terminals)[number]);
      }
    },
  };
});

vi.mock('@xterm/addon-webgl', () => ({
  WebglAddon: class {
    constructor() {
      if (webgl.failOnConstruct) throw new Error('webgl constructor failed');
    }
    activate(): void {
      if (webgl.failOnLoad) throw new Error('webgl unavailable');
      const opened = document.querySelector('.terminal-view__xterm .xterm') !== null;
      webgl.events.push(opened ? 'webgl:activate:after-open' : 'webgl:activate:before-open');
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
  binary: number[][];
  cells: Array<{ width: number; height: number }>;
  replies: string[];
  focusReports: string[];
  focus: ReturnType<typeof vi.fn>;
  term: () => HTMLTextAreaElement;
}

function make(): Harness {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const inputs: string[] = [];
  const binary: number[][] = [];
  const cells: Array<{ width: number; height: number }> = [];
  const replies: string[] = [];
  const focusReports: string[] = [];
  const focus = vi.fn();
  const callbacks: XtermCallbacks = {
    onInput: (b) => {
      inputs.push(decoder.decode(b));
      binary.push([...b]);
    },
    onAutomaticReply: (b) => replies.push(decoder.decode(b)),
    onFocusReport: (b) => focusReports.push(decoder.decode(b)),
    onFocus: focus,
    onCellSize: (c) => cells.push(c),
  };
  const adapter = createXtermAdapter(host, { cols: 80, rows: 24 }, callbacks);
  return {
    adapter,
    host,
    inputs,
    binary,
    cells,
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
    webgl.failOnConstruct = false;
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
    expect(webgl.events).toEqual(['webgl:activate:after-open']);
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

  it('falls back to the DOM renderer when the WebGL addon constructor throws', () => {
    webgl.failOnConstruct = true;
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
    expect(webgl.events).toEqual(['webgl:activate:after-open', 'webgl:dispose:terminal-live']);
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
    for (const [key, code, keyCode] of [
      ['Enter', 'Enter', 13],
      ['Backspace', 'Backspace', 8],
    ] as const) {
      const event = new KeyboardEvent('keydown', {
        key,
        code,
        keyCode,
        metaKey: true,
        bubbles: true,
        cancelable: true,
      });
      h.term().dispatchEvent(event);
      expect(event.defaultPrevented).toBe(false);
    }
    expect(h.inputs).toEqual([]);
    h.adapter.dispose();
  });

  it('routes onBinary data to onInput as bytes', () => {
    const h = make();
    terminals[terminals.length - 1]?._core.coreService.triggerBinaryEvent('\xff\x01\x80');
    expect(h.binary).toEqual([[0xff, 0x01, 0x80]]);
    h.adapter.dispose();
  });

  it('calls onFocus on focusin', () => {
    const h = make();
    h.term().dispatchEvent(new FocusEvent('focusin', { bubbles: true }));
    expect(h.focus).toHaveBeenCalledTimes(1);
    h.adapter.dispose();
  });

  it('reports a measured cell size only when it is non-zero and changed', async () => {
    const h = make();
    const screen = h.host.querySelector('.xterm-screen') as HTMLElement;
    const rect = { width: 0, height: 0 };
    screen.getBoundingClientRect = () => rect as DOMRect;
    const rendered = async () => {
      await write(h.adapter, 'x');
      await new Promise((resolve) => setTimeout(resolve, 50));
    };
    await rendered();
    expect(h.cells).toEqual([]);
    rect.width = 640;
    rect.height = 480;
    await rendered();
    await rendered();
    expect(h.cells).toEqual([{ width: 8, height: 20 }]);
    h.adapter.dispose();
  });

  describe('a terminal face that loads after the terminal opened', () => {
    function fakeFontSet(ready: boolean) {
      const pending: Array<(faces: FontFace[]) => void> = [];
      const fonts = {
        check: vi.fn(() => ready),
        load: vi.fn((_spec: string) => new Promise<FontFace[]>((resolve) => pending.push(resolve))),
      };
      Object.defineProperty(document, 'fonts', { configurable: true, value: fonts });
      return { fonts, settle: (faces: FontFace[]) => pending.forEach((resolve) => resolve(faces)) };
    }

    function measured() {
      const terminal = terminals.at(-1);
      if (!terminal) throw new Error('no terminal');
      const families: string[] = [];
      const measure = vi.spyOn(terminal._core._charSizeService, 'measure').mockImplementation(() => {
        families.push(String(terminal.options.fontFamily));
      });
      return { terminal, measure, families };
    }

    afterEach(() => {
      Reflect.deleteProperty(document, 'fonts');
    });

    it('measures the cell again once the face arrives, and keeps the configured family', async () => {
      const { fonts, settle } = fakeFontSet(false);
      const h = make();
      const { terminal, families } = measured();
      expect(fonts.load.mock.calls.map(([spec]) => spec)).toEqual([
        "400 13px 'JetBrains Mono'",
        "700 13px 'JetBrains Mono'",
      ]);
      expect(families).toEqual([]);
      settle([{} as FontFace]);
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(families).toEqual([
        '"JetBrains Mono", Menlo, monospace',
        "'JetBrains Mono', Menlo, monospace",
      ]);
      expect(terminal.options.fontFamily).toBe("'JetBrains Mono', Menlo, monospace");
      h.adapter.dispose();
    });

    it('does nothing when the face was ready at open, missing, or arrives after dispose', async () => {
      fakeFontSet(true);
      const ready = make();
      expect((document.fonts as unknown as { load: ReturnType<typeof vi.fn> }).load).not.toHaveBeenCalled();
      ready.adapter.dispose();

      const missing = fakeFontSet(false);
      const h = make();
      const { measure } = measured();
      missing.settle([]);
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(measure).not.toHaveBeenCalled();
      h.adapter.dispose();

      const late = fakeFontSet(false);
      const gone = make();
      const goneMeasure = measured().measure;
      gone.adapter.dispose();
      late.settle([{} as FontFace]);
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(goneMeasure).not.toHaveBeenCalled();
    });
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
