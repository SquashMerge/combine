import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { TermId } from '../../src/model/ids';
import { parseTermId } from '../../src/model/ids';
import { AttachmentMachine } from '../../src/panels/attachmentMachine';
import { TerminalController } from '../../src/panels/terminalController';
import { displayFrom } from '../../src/renderers/terminal/geometry';
import { TERMINAL_HISTORY_LINES, TERMINAL_THEME } from '../../src/renderers/terminal/settings';
import { fakeXtermFactory } from '../support/fakeXterm';
import {
  HOST_ID,
  MemoryTerminalPort,
  attachedEvent,
  closedEvent,
  cursor,
  dataEvent,
  errorEvent,
  gapEvent,
  lostEvent,
  resizedEvent,
  resumedEvent,
  resyncedEvent,
  startedEvent,
  type MemoryTerminalStream,
  type StartedOptions,
} from '../support/memoryTerminalPort';

const TERM: TermId = parseTermId('term-1');

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

interface Rig {
  port: MemoryTerminalPort;
  fake: ReturnType<typeof fakeXtermFactory>;
  controller: TerminalController;
  host: HTMLElement;
  stream(): MemoryTerminalStream;
  ops(): string[];
  acks(): string[];
  open(): Promise<void>;
  /** Shows the panel and walks it to a live attachment 7 at epoch 1 with an applied `started`. */
  goLive(started?: StartedOptions): Promise<void>;
}

function rig(): Rig {
  const port = new MemoryTerminalPort();
  const fake = fakeXtermFactory();
  const controller = new TerminalController({
    port,
    hostId: HOST_ID,
    termId: TERM,
    createAdapter: fake.factory,
  });
  const host = document.createElement('div');
  const stream = (): MemoryTerminalStream => {
    const first = port.streams[0];
    if (first === undefined) throw new Error('stream not open');
    return first;
  };
  return {
    port,
    fake,
    controller,
    host,
    stream,
    ops: () => stream().sent.map((command) => command.op),
    acks: () =>
      stream()
        .commands('ack')
        .map((command) => (command.op === 'ack' ? `${command.cursor.epoch}:${command.cursor.seq}` : '')),
    open: () => controller.start(host),
    async goLive(started) {
      await controller.start(host);
      controller.setVisible(true);
      stream().emit(attachedEvent(1, 7));
      stream().emit(startedEvent(cursor(7, 1, 0), started));
      await Promise.resolve();
      vi.advanceTimersByTime(16);
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('terminalController attach and detach', () => {
  it('show while connected sends attach with the fit size and display', async () => {
    const t = rig();
    await t.open();
    t.controller.setContainerSize(800, 340);
    t.controller.setVisible(true);
    expect(t.stream().sent).toEqual([
      {
        op: 'attach',
        terminal: TERM,
        options: {
          cols: 100,
          rows: 20,
          display: displayFrom(TERMINAL_THEME, { width: 8, height: 17 }),
          observe: false,
          history: TERMINAL_HISTORY_LINES,
        },
      },
    ]);
    expect(t.controller.getView().phase).toBe('attaching');
  });

  it('show while disconnected sends nothing until the connection returns', async () => {
    const t = rig();
    t.port.setConnected(HOST_ID, false);
    await t.open();
    t.controller.setVisible(true);
    expect(t.stream().sent).toEqual([]);
    expect(t.controller.getView().phase).toBe('waiting');
    t.port.setConnected(HOST_ID, true);
    expect(t.ops()).toEqual(['attach']);
  });

  it('hide sends detach and disposes the instance', async () => {
    const t = rig();
    await t.goLive();
    t.controller.setVisible(false);
    expect(t.stream().commands('detach')).toEqual([{ op: 'detach', attachment: '7' }]);
    expect(t.fake.instances[0]?.disposed).toBe(true);
    expect(t.controller.getView()).toMatchObject({ phase: 'detaching', hasInstance: false });
  });

  it('hide before attached sends detach once attached names the attachment', async () => {
    const t = rig();
    await t.open();
    t.controller.setVisible(true);
    t.controller.setVisible(false);
    expect(t.ops()).toEqual(['attach']);
    t.stream().emit(attachedEvent(1, 7));
    expect(t.stream().commands('detach')).toEqual([{ op: 'detach', attachment: '7' }]);
  });

  it('an attach error after a hide finishes without detaching', async () => {
    const t = rig();
    await t.open();
    t.controller.setVisible(true);
    t.controller.setVisible(false);
    t.stream().emit(errorEvent({ code: 'unknown_terminal', request: 1 }));
    expect(t.ops()).toEqual(['attach']);
    expect(t.controller.getView().phase).toBe('hidden');
  });

  it('a detach before started finishes on the in-order gap', async () => {
    const t = rig();
    await t.open();
    t.controller.setVisible(true);
    t.stream().emit(attachedEvent(1, 7));
    t.controller.setVisible(false);
    expect(t.ops()).toEqual(['attach', 'detach']);
    t.stream().emit(gapEvent(7, null));
    expect(t.controller.getView().phase).toBe('hidden');
    expect(t.ops()).toEqual(['attach', 'detach']);
  });

  it('a drop while detaching abandons the attachment and attaches fresh if shown again', async () => {
    const t = rig();
    await t.goLive();
    t.controller.setVisible(false);
    t.port.setConnected(HOST_ID, false);
    expect(t.controller.getView().phase).toBe('hidden');
    t.controller.setVisible(true);
    expect(t.controller.getView().phase).toBe('waiting');
    t.port.setConnected(HOST_ID, true);
    expect(t.stream().commands('attach')).toHaveLength(2);
    expect(t.stream().commands('resume')).toHaveLength(0);
    expect(t.controller.getView().phase).toBe('attaching');
  });

  it('hide before attached, drop, show and reconnect attaches once and stays attached', async () => {
    const t = rig();
    await t.open();
    t.controller.setVisible(true);
    t.controller.setVisible(false);
    t.port.setConnected(HOST_ID, false);
    t.controller.setVisible(true);
    t.port.setConnected(HOST_ID, true);
    expect(t.stream().commands('attach')).toHaveLength(2);
    t.stream().emit(attachedEvent(2, 8));
    t.stream().emit(startedEvent(cursor(8, 1, 0)));
    await Promise.resolve();
    expect(t.stream().commands('detach')).toEqual([]);
    expect(t.controller.getView().phase).toBe('live');
    expect(t.fake.instances).toHaveLength(1);
  });

  it('detaching discards events and acks only the final closed', async () => {
    const t = rig();
    await t.goLive();
    t.controller.setVisible(false);
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'late'));
    vi.advanceTimersByTime(100);
    expect(t.fake.instances[0]?.writes).toHaveLength(0);
    expect(t.acks()).toEqual(['1:0']);
    t.stream().emit(closedEvent(cursor(7, 1, 2), 'detached'));
    expect(t.acks()).toEqual(['1:0', '1:2']);
    expect(t.controller.getView().phase).toBe('hidden');
  });

  it('show during detaching attaches after the final event', async () => {
    const t = rig();
    await t.goLive();
    t.controller.setVisible(false);
    t.controller.setVisible(true);
    expect(t.stream().commands('attach')).toHaveLength(1);
    t.stream().emit(closedEvent(cursor(7, 1, 1), 'detached'));
    expect(t.stream().commands('attach')).toHaveLength(2);
    expect(t.controller.getView().phase).toBe('attaching');
  });

  it('a rejected attach waits for the next connection', async () => {
    const t = rig();
    await t.open();
    t.stream().rejectNext('attach', 'not connected');
    t.controller.setVisible(true);
    await flush();
    expect(t.controller.getView()).toMatchObject({ phase: 'waiting', connected: false });
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    expect(t.stream().commands('attach')).toHaveLength(2);
  });

  it('a rejection other than not connected shows send_failed', async () => {
    const t = rig();
    await t.open();
    t.stream().rejectNext('attach', 'boom');
    t.controller.setVisible(true);
    await flush();
    expect(t.controller.getView()).toMatchObject({
      phase: 'ended',
      status: { kind: 'error', code: 'send_failed' },
    });
  });

  it('a late rejection of an earlier attach leaves the newer attach alone', async () => {
    const t = rig();
    await t.open();
    const first = t.stream().holdNext('attach');
    t.controller.setVisible(true);
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    expect(t.stream().commands('attach')).toHaveLength(2);
    first.reject('not connected');
    await flush();
    expect(t.controller.getView()).toMatchObject({ phase: 'attaching', connected: true });
    t.stream().emit(attachedEvent(2, 8));
    t.stream().emit(startedEvent(cursor(8, 1, 0)));
    await Promise.resolve();
    expect(t.controller.getView().phase).toBe('live');
    expect(t.stream().commands('attach')).toHaveLength(2);
    expect(t.stream().commands('detach')).toEqual([]);
  });

  it('stop resolves and records close after the connection drops', async () => {
    const t = rig();
    await t.goLive();
    t.port.setConnected(HOST_ID, false);
    await t.controller.stop();
    expect(t.ops().at(-1)).toBe('close');
  });

  it('stop before start resolves does not reject while disconnected', async () => {
    const t = rig();
    t.port.setConnected(HOST_ID, false);
    const starting = t.controller.start(t.host);
    const stopping = t.controller.stop();
    await expect(starting).resolves.toBeUndefined();
    await expect(stopping).resolves.toBeUndefined();
    expect(t.ops()).toEqual(['close']);
  });

  it('stop sends detach from the webview and closes the stream once hidden', async () => {
    const t = rig();
    await t.goLive();
    const held = t.stream().holdNext('ack');
    let stopped = false;
    const stopping = t.controller.stop().then(() => {
      stopped = true;
    });
    expect(t.stream().commands('detach')).toEqual([{ op: 'detach', attachment: '7' }]);
    t.controller.setVisible(true);
    expect(t.stream().commands('attach')).toHaveLength(1);
    await flush();
    expect(t.ops()).not.toContain('close');
    t.stream().emit(closedEvent(cursor(7, 1, 1), 'detached'));
    await flush();
    expect(t.acks()).toEqual(['1:0', '1:1']);
    expect(t.ops()).not.toContain('close');
    held.resolve();
    await stopping;
    expect(stopped).toBe(true);
    expect(t.ops().slice(-2)).toEqual(['ack', 'close']);
    expect(t.stream().listenerCount()).toBe(0);
  });
});

describe('terminalController apply and ack', () => {
  it('applies events in cursor order, one at a time', async () => {
    const t = rig();
    await t.goLive();
    const adapter = t.fake.instances[0];
    for (const seq of [1, 2, 3]) t.stream().emit(dataEvent(cursor(7, 1, seq), `d${seq}`));
    expect(adapter?.writes).toHaveLength(1);
    adapter?.completeWrite();
    expect(adapter?.writes).toHaveLength(2);
    adapter?.completeWrite();
    adapter?.completeWrite();
    const text = adapter?.writes.map((write) => new TextDecoder().decode(write.bytes));
    expect(text).toEqual(['d1', 'd2', 'd3']);
  });

  it('acks only after the renderer reports the write applied', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'one'));
    vi.advanceTimersByTime(100);
    expect(t.acks()).toEqual(['1:0']);
    t.fake.instances[0]?.completeWrite();
    vi.advanceTimersByTime(16);
    expect(t.acks()).toEqual(['1:0', '1:1']);
  });

  it('coalesces acks to the latest applied cursor every 16 ms', async () => {
    const t = rig();
    await t.goLive();
    for (const seq of [1, 2, 3]) t.stream().emit(dataEvent(cursor(7, 1, seq), 'x'));
    t.fake.instances[0]?.completeAll();
    vi.advanceTimersByTime(15);
    expect(t.acks()).toEqual(['1:0']);
    vi.advanceTimersByTime(1);
    expect(t.acks()).toEqual(['1:0', '1:3']);
  });

  it('a drop while attaching attaches fresh on reconnect, never resumes', async () => {
    const t = rig();
    await t.open();
    t.controller.setVisible(true);
    t.stream().emit(attachedEvent(1, 7));
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    expect(t.ops()).toEqual(['attach', 'attach']);
  });

  it('drops events at or before the last received cursor', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'one'));
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'one'));
    t.stream().emit(dataEvent(cursor(7, 1, 0), 'older'));
    const adapter = t.fake.instances[0];
    adapter?.completeAll();
    expect(adapter?.writes).toHaveLength(1);
  });
});

describe('terminalController epochs and instances', () => {
  it('a new epoch disposes the instance and creates one at the Started size', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(startedEvent(cursor(7, 2, 0), { cols: 100, rows: 30 }));
    await Promise.resolve();
    expect(t.fake.instances).toHaveLength(2);
    expect(t.fake.instances[0]?.disposed).toBe(true);
    expect(t.fake.instances[1]?.size).toEqual({ cols: 100, rows: 30 });
    expect(t.controller.getView().ptySize).toEqual({ cols: 100, rows: 30 });
  });

  it('the old instance is disposed before the new one is created', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(startedEvent(cursor(7, 2, 0)));
    await Promise.resolve();
    expect(t.fake.log).toEqual(['create 0', 'dispose 0', 'create 1']);
  });

  it('an in-order gap keeps the old screen dimmed until the new Started', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(gapEvent(7, null));
    expect(t.controller.getView()).toMatchObject({ phase: 'attaching', stale: true, hasInstance: true });
    expect(t.fake.instances[0]?.disposed).toBe(false);
    expect(t.stream().commands('attach')).toHaveLength(2);
    t.stream().emit(attachedEvent(2, 8));
    t.stream().emit(startedEvent(cursor(8, 1, 0)));
    await Promise.resolve();
    expect(t.controller.getView()).toMatchObject({ phase: 'live', stale: false });
    expect(t.fake.instances).toHaveLength(2);
    expect(t.fake.instances[0]?.disposed).toBe(true);
  });

  it('events arriving while the instance is stale are dropped, not written to it', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(gapEvent(7, null));
    t.stream().emit(attachedEvent(2, 8));
    t.stream().emit(dataEvent(cursor(8, 1, 1), 'early'));
    expect(t.fake.instances[0]?.writes).toHaveLength(0);
    expect(t.controller.getView()).toMatchObject({ phase: 'attaching', stale: true });
  });

  it('events of another epoch are dropped, not applied', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(dataEvent(cursor(7, 2, 1), 'stray'));
    expect(t.fake.instances[0]?.writes).toHaveLength(0);
    expect(t.controller.getView().phase).toBe('live');
  });

  it('callbacks and write completions from a disposed instance are ignored', async () => {
    const t = rig();
    await t.goLive();
    const old = t.fake.instances[0];
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'one'));
    t.controller.setVisible(false);
    old?.emitCellSize({ width: 10, height: 20 });
    old?.completeAll();
    vi.advanceTimersByTime(100);
    expect(t.controller.getView().cell).toEqual({ width: 8, height: 17 });
    expect(t.acks()).toEqual(['1:0']);
  });

  it('a hide before a queued createInstance runs leaves no instance', async () => {
    const t = rig();
    await t.open();
    t.controller.setVisible(true);
    t.stream().emit(attachedEvent(1, 7));
    t.stream().emit(startedEvent(cursor(7, 1, 0)));
    t.controller.setVisible(false);
    await flush();
    expect(t.fake.instances).toHaveLength(0);
    expect(t.controller.getView().hasInstance).toBe(false);
  });
});

describe('terminalController resize', () => {
  it('a container resize never resizes the instance', async () => {
    const t = rig();
    await t.goLive();
    t.controller.setContainerSize(1000, 500);
    vi.advanceTimersByTime(500);
    expect(t.fake.instances[0]?.resizes).toEqual([]);
    expect(t.stream().commands('resize')).toEqual([]);
  });

  it('resized is applied only after earlier writes complete', async () => {
    const t = rig();
    await t.goLive();
    const adapter = t.fake.instances[0];
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'one'));
    t.stream().emit(resizedEvent(cursor(7, 1, 2), 100, 30));
    expect(adapter?.resizes).toEqual([]);
    adapter?.completeWrite();
    expect(adapter?.resizes).toEqual([{ cols: 100, rows: 30 }]);
    expect(t.controller.getView().ptySize).toEqual({ cols: 100, rows: 30 });
    vi.advanceTimersByTime(16);
    expect(t.acks()).toEqual(['1:0', '1:2']);
  });
});

describe('terminalController status and lifetime', () => {
  it('an attach error shows its code', async () => {
    const t = rig();
    await t.open();
    t.controller.setVisible(true);
    t.stream().emit(errorEvent({ code: 'unknown_terminal', request: 1 }));
    expect(t.controller.getView()).toMatchObject({
      phase: 'ended',
      status: { kind: 'error', code: 'unknown_terminal' },
    });
  });

  it('resynced shows output skipped', async () => {
    const t = rig();
    await t.goLive();
    expect(t.controller.getView().outputSkipped).toBe(false);
    t.stream().emit(resyncedEvent(cursor(7, 1, 1)));
    expect(t.controller.getView().outputSkipped).toBe(true);
  });

  it('inexact fidelity and unordered resizes are shown', async () => {
    const t = rig();
    await t.goLive({ fidelity: 'inexact', orderedResizes: false });
    expect(t.controller.getView()).toMatchObject({ fidelity: 'inexact', unorderedResizes: true });
  });

  it('only a visible panel holds an instance', async () => {
    const t = rig();
    await t.open();
    expect(t.fake.instances).toHaveLength(0);
    t.controller.setVisible(true);
    t.stream().emit(attachedEvent(1, 7));
    t.stream().emit(startedEvent(cursor(7, 1, 0)));
    await Promise.resolve();
    expect(t.fake.instances.filter((instance) => !instance.disposed)).toHaveLength(1);
    t.controller.setVisible(false);
    expect(t.fake.instances.filter((instance) => !instance.disposed)).toHaveLength(0);
    expect(t.controller.getView().hasInstance).toBe(false);
  });

  it('events delivered before openStream resolves are handled in order', async () => {
    const t = rig();
    t.port.onOpen = (stream) => stream.emit({ type: 'connection', connected: false });
    const starting = t.controller.start(t.host);
    t.controller.setVisible(true);
    await starting;
    expect(t.ops()).toEqual(['attach']);
    expect(t.controller.getView()).toMatchObject({ phase: 'waiting', connected: false });
  });

  it('notifies listeners only when the view changes and keeps the snapshot stable', async () => {
    const t = rig();
    await t.open();
    const listener = vi.fn();
    t.controller.subscribe(listener);
    const before = t.controller.getView();
    t.port.setConnected(HOST_ID, true);
    expect(listener).not.toHaveBeenCalled();
    expect(t.controller.getView()).toBe(before);
    t.controller.setVisible(true);
    expect(listener).toHaveBeenCalled();
    expect(t.controller.getView()).not.toBe(before);
  });
});

/** Goes live, applies data up to `seq`, drops the connection and brings it back, which resumes. */
async function resumeAfterDrop(t: Rig, seq = 1): Promise<void> {
  await t.goLive();
  for (let n = 1; n <= seq; n++) t.stream().emit(dataEvent(cursor(7, 1, n), `d${n}`));
  t.fake.instances[0]?.completeAll();
  t.port.setConnected(HOST_ID, false);
  t.port.setConnected(HOST_ID, true);
}

describe('terminalController resume', () => {
  it('resumes from the last applied cursor after a drop', async () => {
    const t = rig();
    await resumeAfterDrop(t, 2);
    expect(t.stream().commands('resume')).toEqual([{ op: 'resume', cursor: cursor(7, 1, 2) }]);
    expect(t.stream().commands('attach')).toHaveLength(1);
    expect(t.controller.getView()).toMatchObject({ phase: 'resuming', hasInstance: true, stale: false });
    t.stream().emit(resumedEvent(2, 7, null));
    expect(t.controller.getView().phase).toBe('live');
    expect(t.fake.instances).toHaveLength(1);
  });

  it('resume waits until the apply queue drains', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'one'));
    t.stream().emit(dataEvent(cursor(7, 1, 2), 'two'));
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    expect(t.stream().commands('resume')).toEqual([]);
    expect(t.controller.getView().phase).toBe('waiting');
    t.fake.instances[0]?.completeWrite();
    expect(t.stream().commands('resume')).toEqual([]);
    t.fake.instances[0]?.completeWrite();
    expect(t.stream().commands('resume')).toEqual([{ op: 'resume', cursor: cursor(7, 1, 2) }]);
    expect(t.controller.getView().phase).toBe('resuming');
    expect(t.stream().commands('attach')).toHaveLength(1);
  });

  it('acks replayed events once applied', async () => {
    const t = rig();
    await resumeAfterDrop(t, 1);
    t.stream().emit(resumedEvent(2, 7, cursor(7, 1, 3)));
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'dup'));
    t.stream().emit(dataEvent(cursor(7, 1, 2), 'two'));
    t.stream().emit(dataEvent(cursor(7, 1, 3), 'three'));
    vi.advanceTimersByTime(100);
    expect(t.acks()).toEqual(['1:0']);
    t.fake.instances[0]?.completeAll();
    vi.advanceTimersByTime(16);
    expect(t.acks()).toEqual(['1:0', '1:3']);
    expect(t.fake.instances[0]?.writes.map((write) => new TextDecoder().decode(write.bytes))).toEqual([
      'd1',
      'two',
      'three',
    ]);
  });

  it('a rejected resume waits for the next connection and resumes again', async () => {
    const t = rig();
    await t.goLive();
    t.port.setConnected(HOST_ID, false);
    t.stream().rejectNext('resume', 'not connected');
    t.port.setConnected(HOST_ID, true);
    await flush();
    expect(t.stream().commands('resume')).toHaveLength(1);
    expect(t.controller.getView()).toMatchObject({ phase: 'waiting', connected: false });
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    expect(t.stream().commands('resume')).toHaveLength(2);
    expect(t.controller.getView().phase).toBe('resuming');
    expect(t.stream().commands('attach')).toHaveLength(1);
  });

  it('a resume answered with gap attaches fresh', async () => {
    const t = rig();
    await resumeAfterDrop(t);
    t.stream().emit(gapEvent(7, 2));
    expect(t.stream().commands('attach')).toHaveLength(2);
    expect(t.controller.getView()).toMatchObject({ phase: 'attaching', stale: true, hasInstance: true });
    expect(t.fake.instances[0]?.disposed).toBe(false);
    t.stream().emit(attachedEvent(3, 8));
    t.stream().emit(startedEvent(cursor(8, 1, 0)));
    await Promise.resolve();
    expect(t.controller.getView()).toMatchObject({ phase: 'live', stale: false });
    expect(t.fake.instances[0]?.disposed).toBe(true);
  });

  it('a resume answered with an error attaches fresh', async () => {
    const t = rig();
    await resumeAfterDrop(t);
    t.stream().emit(errorEvent({ code: 'unknown_attachment', request: 2 }));
    expect(t.stream().commands('attach')).toHaveLength(2);
    expect(t.controller.getView()).toMatchObject({ phase: 'attaching', stale: true, status: null });
  });

  it('a reply with nothing outstanding is ignored', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(resumedEvent(5, 7, null));
    t.stream().emit(gapEvent(7, 6));
    t.stream().emit(errorEvent({ code: 'unknown_attachment', request: 7 }));
    expect(t.controller.getView()).toMatchObject({ phase: 'live', hasInstance: true });
    expect(t.stream().commands('attach')).toHaveLength(1);
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    t.stream().emit(resumedEvent(8, 7, null));
    t.stream().emit(gapEvent(7, 9));
    expect(t.controller.getView().phase).toBe('live');
    expect(t.stream().commands('attach')).toHaveLength(1);
  });

  it('request ids that differ from the panel count still answer the outstanding request', async () => {
    const t = rig();
    await t.open();
    t.controller.setVisible(true);
    t.stream().emit(attachedEvent(17, 7));
    t.stream().emit(startedEvent(cursor(7, 1, 0)));
    await Promise.resolve();
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'one'));
    t.fake.instances[0]?.completeAll();
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    expect(t.controller.getView().phase).toBe('resuming');
    t.stream().emit(resumedEvent(42, 7, null));
    expect(t.controller.getView().phase).toBe('live');
    expect(t.stream().commands('attach')).toHaveLength(1);
  });

  it('a resume rejected for another reason attaches fresh', async () => {
    const t = rig();
    await t.goLive();
    t.port.setConnected(HOST_ID, false);
    t.stream().rejectNext('resume', 'boom');
    t.port.setConnected(HOST_ID, true);
    await flush();
    expect(t.stream().commands('resume')).toHaveLength(1);
    expect(t.stream().commands('attach')).toHaveLength(2);
    expect(t.controller.getView()).toMatchObject({ phase: 'attaching', stale: true, hasInstance: true });
  });

  it('hide while resuming detaches once resumed names the attachment', async () => {
    const t = rig();
    await resumeAfterDrop(t);
    t.controller.setVisible(false);
    expect(t.stream().commands('detach')).toEqual([]);
    expect(t.controller.getView()).toMatchObject({ phase: 'detaching', hasInstance: false });
    expect(t.fake.instances[0]?.disposed).toBe(true);
    t.stream().emit(resumedEvent(2, 7, null));
    expect(t.stream().commands('detach')).toEqual([{ op: 'detach', attachment: '7' }]);
  });

  it('hide while resuming finishes on gap{request} without detaching, and a later show attaches', async () => {
    const t = rig();
    await resumeAfterDrop(t);
    t.controller.setVisible(false);
    t.stream().emit(gapEvent(7, 2));
    expect(t.controller.getView().phase).toBe('hidden');
    expect(t.stream().commands('detach')).toEqual([]);
    t.controller.setVisible(true);
    expect(t.stream().commands('attach')).toHaveLength(2);
    expect(t.controller.getView().phase).toBe('attaching');
  });

  it('isReplay holds through replay_through and clears once it is applied', () => {
    const machine = new AttachmentMachine(TERM);
    const events = [attachedEvent(1, 7), startedEvent(cursor(7, 1, 0))];
    machine.handle({ kind: 'show' });
    machine.handle({ kind: 'event', event: { type: 'connection', connected: true } });
    for (const event of events) machine.handle({ kind: 'event', event });
    machine.handle({ kind: 'applied', cursor: cursor(7, 1, 0) });
    machine.handle({ kind: 'event', event: { type: 'connection', connected: false } });
    machine.handle({ kind: 'event', event: { type: 'connection', connected: true } });
    expect(machine.view().phase).toBe('resuming');
    expect(machine.isReplay(cursor(7, 1, 1))).toBe(false);
    machine.handle({ kind: 'event', event: resumedEvent(2, 7, cursor(7, 1, 2)) });
    expect(machine.isReplay(cursor(7, 1, 2))).toBe(true);
    expect(machine.isReplay(cursor(7, 1, 3))).toBe(false);
    for (const seq of [1, 2]) {
      machine.handle({ kind: 'event', event: dataEvent(cursor(7, 1, seq), 'x') });
      machine.handle({ kind: 'applied', cursor: cursor(7, 1, seq) });
    }
    expect(machine.isReplay(cursor(7, 1, 2))).toBe(false);
  });
});

describe('terminalController end states', () => {
  it('closed exited shows exited and keeps the screen', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'one'));
    t.fake.instances[0]?.completeAll();
    t.stream().emit(closedEvent(cursor(7, 1, 2), 'exited'));
    expect(t.acks()).toEqual(['1:0', '1:2']);
    vi.advanceTimersByTime(100);
    expect(t.acks()).toEqual(['1:0', '1:2']);
    expect(t.controller.getView()).toMatchObject({
      phase: 'ended',
      status: { kind: 'exited' },
      hasInstance: true,
      canReattach: false,
    });
    expect(t.fake.instances[0]?.disposed).toBe(false);
  });

  it('closed detached outside detaching ends with closed status and is acked', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(closedEvent(cursor(7, 1, 1), 'detached'));
    expect(t.acks()).toEqual(['1:0', '1:1']);
    expect(t.controller.getView()).toMatchObject({ phase: 'ended', status: { kind: 'closed' } });
    expect(t.stream().commands('detach')).toEqual([]);
  });

  it('lost shows lost with reattach and never retries on its own', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(lostEvent(cursor(7, 1, 1)));
    expect(t.acks()).toEqual(['1:0', '1:1']);
    expect(t.controller.getView()).toMatchObject({
      phase: 'ended',
      status: { kind: 'lost' },
      canReattach: true,
    });
    vi.advanceTimersByTime(10_000);
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    await flush();
    expect(t.stream().commands('attach')).toHaveLength(1);
    expect(t.stream().commands('resume')).toEqual([]);
    expect(t.controller.getView().phase).toBe('ended');
  });

  it('reattach does nothing after exited or closed', async () => {
    for (const reason of ['exited', 'detached'] as const) {
      const t = rig();
      await t.goLive();
      t.stream().emit(closedEvent(cursor(7, 1, 1), reason));
      const before = t.controller.getView();
      t.controller.reattach();
      expect(t.controller.getView()).toBe(before);
      expect(t.stream().commands('attach')).toHaveLength(1);
    }
  });

  it('reattach after lost attaches fresh with the old screen dimmed', async () => {
    const t = rig();
    await t.goLive();
    t.stream().emit(lostEvent(cursor(7, 1, 1)));
    t.controller.reattach();
    expect(t.stream().commands('attach')).toHaveLength(2);
    expect(t.controller.getView()).toMatchObject({
      phase: 'attaching',
      stale: true,
      hasInstance: true,
      status: null,
    });
    expect(t.fake.instances[0]?.disposed).toBe(false);
    t.stream().emit(attachedEvent(2, 8));
    t.stream().emit(startedEvent(cursor(8, 1, 0)));
    await Promise.resolve();
    expect(t.controller.getView()).toMatchObject({ phase: 'live', stale: false });
    expect(t.fake.instances[0]?.disposed).toBe(true);
  });
});
