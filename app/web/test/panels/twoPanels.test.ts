import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseTermId, type TermId } from '../../src/model/ids';
import { TerminalController } from '../../src/panels/terminalController';
import { fakeXtermFactory } from '../support/fakeXterm';
import {
  HOST_ID,
  MemoryTerminalPort,
  type MemoryTerminalStream,
  type SentCommand,
} from '../support/memoryTerminalPort';
import { ScriptedDaemon } from '../support/scriptedDaemon';

const TERM: TermId = parseTermId('term-1');
const decoder = new TextDecoder();

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

interface Panel {
  controller: TerminalController;
  fake: ReturnType<typeof fakeXtermFactory>;
  stream: MemoryTerminalStream;
}

interface World {
  port: MemoryTerminalPort;
  daemon: ScriptedDaemon;
  a: Panel;
  b: Panel;
}

async function openPanel(
  port: MemoryTerminalPort,
  size: { width: number; height: number },
): Promise<Panel> {
  const fake = fakeXtermFactory();
  const controller = new TerminalController({
    port,
    hostId: HOST_ID,
    termId: TERM,
    createAdapter: fake.factory,
  });
  await controller.start(document.createElement('div'));
  const stream = port.streams[port.streams.length - 1];
  if (stream === undefined) throw new Error('stream not open');
  controller.setContainerSize(size.width, size.height);
  controller.setVisible(true);
  return { controller, fake, stream };
}

/** Lets every pending write complete and the ack and resize timers run. */
async function settle(...panels: Panel[]): Promise<void> {
  for (let round = 0; round < 3; round++) {
    await flush();
    for (const panel of panels) {
      for (const instance of panel.fake.instances) instance.completeAll();
    }
    vi.advanceTimersByTime(100);
  }
}

/** Two panels on one terminal. A fits 100x20 and B fits 50x10 at the default cell. */
async function twoPanels(screen = 'prompt$ '): Promise<World> {
  const port = new MemoryTerminalPort();
  const daemon = new ScriptedDaemon(port);
  daemon.setScreen(TERM, screen);
  const a = await openPanel(port, { width: 805, height: 349 });
  const b = await openPanel(port, { width: 400, height: 170 });
  await settle(a, b);
  return { port, daemon, a, b };
}

function written(panel: Panel, index = 0): string[] {
  return (panel.fake.instances[index]?.writes ?? []).map((write) => decoder.decode(write.bytes));
}

function typeOn(panel: Panel, text: string): void {
  panel.fake.instances[panel.fake.instances.length - 1]?.emitInput(text);
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('two panels over a scripted daemon', () => {
  it('two panels resume within the grace period without a new instance', async () => {
    const { daemon, port, a, b } = await twoPanels();
    expect(written(a)).toEqual(['prompt$ ']);
    daemon.dropConnection();
    daemon.output(TERM, 'while away');
    expect(a.controller.getView().phase).toBe('waiting');
    daemon.restoreConnection();
    await settle(a, b);
    for (const panel of [a, b]) {
      expect(panel.stream.commands('attach')).toHaveLength(1);
      expect(panel.stream.commands('resume')).toHaveLength(1);
      expect(panel.fake.instances).toHaveLength(1);
      expect(written(panel)).toEqual(['prompt$ ', 'while away']);
      expect(panel.controller.getView()).toMatchObject({ phase: 'live', stale: false });
    }
    expect(port.streams).toHaveLength(2);
  });

  it("the non-owner letterboxes at the owner's PTY size", async () => {
    const { daemon, a, b } = await twoPanels();
    typeOn(a, 'x');
    await settle(a, b);
    expect(daemon.ptySize(TERM)).toEqual({ cols: 100, rows: 20 });
    expect(a.stream.commands('resize')).toHaveLength(1);
    expect(b.stream.commands('resize')).toEqual([]);
    expect(a.controller.getView()).toMatchObject({
      ownership: 'self',
      letterboxed: false,
      ptySize: { cols: 100, rows: 20 },
    });
    expect(b.controller.getView()).toMatchObject({
      ownership: 'other',
      letterboxed: true,
      ptySize: { cols: 100, rows: 20 },
    });
    expect(b.fake.instances[0]?.resizes).toEqual([{ cols: 100, rows: 20 }]);
  });

  it('two panels on one terminal converge on one owner', async () => {
    const { daemon, a, b } = await twoPanels();
    expect(daemon.ownerOf(TERM)).toBeNull();
    expect(a.controller.getView().ownership).toBe('none');
    typeOn(a, 'one');
    await settle(a, b);
    expect(daemon.ownerOf(TERM)).toBe('1');
    expect([a, b].map((panel) => panel.controller.getView().ownership)).toEqual(['self', 'other']);
    typeOn(b, 'two');
    await settle(a, b);
    expect(daemon.ownerOf(TERM)).toBe('2');
    expect(daemon.generation(TERM)).toBe('2');
    expect([a, b].map((panel) => panel.controller.getView().ownership)).toEqual(['other', 'self']);
    expect(daemon.typed(TERM)).toEqual([
      { attachment: '1', text: 'one' },
      { attachment: '2', text: 'two' },
    ]);
    a.controller.takeInput();
    await settle(a, b);
    expect(daemon.ownerOf(TERM)).toBe('1');
    expect([a, b].map((panel) => panel.controller.getView().ownership)).toEqual(['self', 'other']);
    expect(a.stream.commands('requestOwnership')).toHaveLength(1);
    expect(b.stream.commands('requestOwnership')).toEqual([]);
  });

  it('two focused panels that saw the same vacant generation: the first claim wins and the second steals nothing', async () => {
    const { daemon, port, a, b } = await twoPanels();
    expect(daemon.ownerOf(TERM)).toBeNull();
    const handle = port.onCommand;
    if (handle === null) throw new Error('no daemon');
    // Both claims are in flight before the daemon answers either: two clients saw no owner.
    const held: [MemoryTerminalStream, SentCommand][] = [];
    port.onCommand = (stream, command) => {
      if (command.op === 'requestOwnership') held.push([stream, command]);
      else handle(stream, command);
    };
    a.controller.setFocused(true);
    b.controller.setFocused(true);
    await settle(a, b);
    expect(held.map(([, command]) => command)).toEqual([
      { op: 'requestOwnership', attachment: '1', ifVacant: '0' },
      { op: 'requestOwnership', attachment: '2', ifVacant: '0' },
    ]);
    port.onCommand = handle;
    for (const [stream, command] of held) handle(stream, command);
    await settle(a, b);
    expect(daemon.ownerOf(TERM)).toBe('1');
    expect(daemon.generation(TERM)).toBe('1');
    expect(daemon.ptySize(TERM)).toEqual({ cols: 100, rows: 20 });
    expect([a, b].map((panel) => panel.controller.getView().ownership)).toEqual(['self', 'other']);

    b.controller.takeInput();
    await settle(a, b);
    expect(b.stream.commands('requestOwnership').at(-1)).toEqual({
      op: 'requestOwnership',
      attachment: '2',
      ifVacant: null,
    });
    expect(daemon.ownerOf(TERM)).toBe('2');
    expect([a, b].map((panel) => panel.controller.getView().ownership)).toEqual(['other', 'self']);
  });

  it('only the owner forwards DECXCPR, and never from the replay stream', async () => {
    const { daemon, a, b } = await twoPanels();
    typeOn(a, 'x');
    await settle(a, b);
    daemon.query(TERM);
    await flush();
    expect(a.fake.instances[0]?.pendingWrites()).toHaveLength(1);
    expect(b.fake.instances[0]?.pendingWrites()).toHaveLength(1);
    a.fake.instances[0]?.emitAutomaticReply('\x1b[1;1R');
    b.fake.instances[0]?.emitAutomaticReply('\x1b[1;1R');
    expect(a.stream.commands('automaticReply')).toHaveLength(1);
    expect(b.stream.commands('automaticReply')).toEqual([]);
    expect(daemon.automatic(TERM)).toEqual(['\x1b[1;1R']);
    await settle(a, b);

    daemon.dropConnection();
    daemon.query(TERM);
    daemon.restoreConnection();
    await flush();
    expect(a.controller.getView().phase).toBe('live');
    typeOn(a, 'y');
    await flush();
    expect(daemon.ownerOf(TERM)).toBe('1');
    expect(a.controller.getView().ownership).toBe('self');
    expect(a.fake.instances[0]?.pendingWrites()).toHaveLength(1);
    a.fake.instances[0]?.emitAutomaticReply('\x1b[1;1R');
    expect(a.stream.commands('automaticReply')).toHaveLength(1);
    expect(daemon.automatic(TERM)).toEqual(['\x1b[1;1R']);
    await settle(a, b);

    daemon.query(TERM);
    await flush();
    a.fake.instances[0]?.emitAutomaticReply('\x1b[1;1R');
    expect(a.stream.commands('automaticReply')).toHaveLength(2);
    expect(daemon.automatic(TERM)).toEqual(['\x1b[1;1R', '\x1b[1;1R']);
  });

  it('two panels reattach after a daemon restart with the current screen and no stale owner', async () => {
    const { daemon, a, b } = await twoPanels('old screen');
    typeOn(a, 'x');
    await settle(a, b);
    expect(daemon.ownerOf(TERM)).toBe('1');
    daemon.setScreen(TERM, 'new screen');
    daemon.restart(HOST_ID);
    await settle(a, b);
    for (const panel of [a, b]) {
      expect(panel.stream.commands('resume')).toHaveLength(1);
      expect(panel.stream.commands('attach')).toHaveLength(2);
      expect(panel.fake.instances).toHaveLength(2);
      expect(panel.fake.instances[0]?.disposed).toBe(true);
      expect(written(panel, 1)).toEqual(['new screen']);
      expect(panel.controller.getView()).toMatchObject({
        phase: 'live',
        stale: false,
        ownership: 'none',
        letterboxed: true,
      });
    }
    expect(daemon.ownerOf(TERM)).toBeNull();
    typeOn(b, 'z');
    await settle(a, b);
    expect(daemon.ownerOf(TERM)).toBe('4');
    expect([a, b].map((panel) => panel.controller.getView().ownership)).toEqual(['other', 'self']);
  });

  it('only the focused panel claims', async () => {
    const port = new MemoryTerminalPort();
    const daemon = new ScriptedDaemon(port);
    daemon.setScreen(TERM, 'prompt$ ');
    const a = await openPanel(port, { width: 805, height: 349 });
    const b = await openPanel(port, { width: 400, height: 170 });
    b.controller.setFocused(true);
    await settle(a, b);
    expect(daemon.ownerOf(TERM)).toBe('2');
    expect(daemon.ptySize(TERM)).toEqual({ cols: 50, rows: 10 });
    expect(a.stream.commands('requestOwnership')).toEqual([]);
    expect(a.stream.commands('resize')).toEqual([]);
    expect(b.stream.commands('requestOwnership')).toHaveLength(1);
    expect([a, b].map((panel) => panel.controller.getView().ownership)).toEqual(['other', 'self']);
  });
});
