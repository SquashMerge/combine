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
  connectionEvent,
  cursor,
  dataEvent,
  errorEvent,
  gapEvent,
  lostEvent,
  ownershipEvent,
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
        instance: null,
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
    t.port.onOpen = (stream) => stream.emit(connectionEvent(false));
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
    expect(t.stream().commands('resume')).toEqual([{ op: 'resume', cursor: cursor(7, 1, 2), instance: null }]);
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
    expect(t.stream().commands('resume')).toEqual([{ op: 'resume', cursor: cursor(7, 1, 2), instance: null }]);
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
    machine.handle({ kind: 'event', event: connectionEvent(true) });
    for (const event of events) machine.handle({ kind: 'event', event });
    machine.handle({ kind: 'applied', cursor: cursor(7, 1, 0) });
    machine.handle({ kind: 'event', event: connectionEvent(false) });
    machine.handle({ kind: 'event', event: connectionEvent(true) });
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

const decoder = new TextDecoder();

/** Goes live and makes attachment `owner` the owner at `generation`. */
async function goLiveWithOwner(
  t: Rig,
  owner: number | null,
  generation = 1,
  started?: StartedOptions,
): Promise<void> {
  await t.goLive(started);
  t.stream().emit(ownershipEvent(7, generation, owner));
}

describe('terminalController ownership and resize', () => {
  it('the owner sends a debounced resize of floor(container / cell) with its generation', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7, 3);
    t.controller.setContainerSize(400, 200);
    vi.advanceTimersByTime(50);
    t.controller.setContainerSize(805, 349);
    vi.advanceTimersByTime(99);
    expect(t.stream().commands('resize')).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(t.stream().commands('resize')).toEqual([
      { op: 'resize', attachment: '7', size: { cols: 100, rows: 20 }, ownershipGeneration: '3' },
    ]);
    expect(t.fake.instances[0]?.resizes).toEqual([]);
  });

  it('the owner sends no resize when the fit equals the PTY size', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7, 1, { cols: 100, rows: 20 });
    t.controller.setContainerSize(800, 340);
    vi.advanceTimersByTime(500);
    expect(t.stream().commands('resize')).toEqual([]);
  });

  it('a non-owner never sends resize and its view is letterboxed', async () => {
    const t = rig();
    await goLiveWithOwner(t, 8);
    t.controller.setContainerSize(800, 340);
    vi.advanceTimersByTime(500);
    expect(t.stream().commands('resize')).toEqual([]);
    expect(t.controller.getView()).toMatchObject({ ownership: 'other', letterboxed: true });
    t.stream().emit(ownershipEvent(7, 2, null));
    t.controller.setContainerSize(900, 340);
    vi.advanceTimersByTime(500);
    expect(t.stream().commands('resize')).toEqual([]);
    expect(t.controller.getView()).toMatchObject({ ownership: 'none', letterboxed: true });
  });

  it('show sends nothing about ownership', async () => {
    const t = rig();
    await t.goLive();
    vi.advanceTimersByTime(500);
    expect(t.ops().filter((op) => op !== 'attach' && op !== 'ack')).toEqual([]);
    expect(t.controller.getView().ownership).toBe('unknown');
  });

  it('dom focus sends focus for the owner and for a non-owner', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7);
    t.fake.instances[0]?.emitFocus();
    expect(t.stream().commands('focus')).toHaveLength(1);
    t.stream().emit(ownershipEvent(7, 2, 8));
    t.fake.instances[0]?.emitFocus();
    expect(t.stream().commands('focus')).toEqual([
      { op: 'focus', attachment: '7' },
      { op: 'focus', attachment: '7' },
    ]);
  });

  it('non-owner typing sends input and never request_ownership', async () => {
    const t = rig();
    await goLiveWithOwner(t, 8);
    t.fake.instances[0]?.emitInput('ls');
    const inputs = t.stream().commands('input');
    expect(inputs).toHaveLength(1);
    expect(inputs[0]).toMatchObject({ op: 'input', attachment: '7' });
    expect(inputs[0]?.op === 'input' && decoder.decode(inputs[0].bytes)).toBe('ls');
    expect(t.stream().commands('requestOwnership')).toEqual([]);
  });

  it('the owner forwards focus reports as input', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7);
    t.fake.instances[0]?.emitFocusReport('\x1b[I');
    const inputs = t.stream().commands('input');
    expect(inputs).toHaveLength(1);
    expect(inputs[0]?.op === 'input' && decoder.decode(inputs[0].bytes)).toBe('\x1b[I');
  });

  it('a non-owner drops focus reports', async () => {
    const t = rig();
    await goLiveWithOwner(t, 8);
    t.fake.instances[0]?.emitFocusReport('\x1b[I');
    t.fake.instances[0]?.emitFocusReport('\x1b[O');
    expect(t.stream().commands('input')).toEqual([]);
    expect(t.stream().commands('requestOwnership')).toEqual([]);
  });

  it('take input sends request_ownership', async () => {
    const t = rig();
    await goLiveWithOwner(t, 8);
    expect(t.controller.getView().canTakeInput).toBe(true);
    t.controller.takeInput();
    expect(t.stream().commands('requestOwnership')).toEqual([
      { op: 'requestOwnership', attachment: '7', ifVacant: null },
    ]);
    t.stream().emit(ownershipEvent(7, 2, 7));
    expect(t.controller.getView().canTakeInput).toBe(false);
    t.controller.takeInput();
    expect(t.stream().commands('requestOwnership')).toHaveLength(1);
  });

  it("the owner mark follows the daemon's ownership events", async () => {
    const t = rig();
    await t.goLive();
    expect(t.controller.getView()).toMatchObject({ ownership: 'unknown', letterboxed: true });
    t.stream().emit(ownershipEvent(7, 1, null));
    expect(t.controller.getView().ownership).toBe('none');
    t.stream().emit(ownershipEvent(7, 2, 8));
    expect(t.controller.getView().ownership).toBe('other');
    t.stream().emit(ownershipEvent(7, 3, 7));
    expect(t.controller.getView()).toMatchObject({ ownership: 'self', letterboxed: false });
    t.stream().emit(ownershipEvent(9, 4, null));
    expect(t.controller.getView().ownership).toBe('self');
    t.port.setConnected(HOST_ID, false);
    expect(t.controller.getView()).toMatchObject({ ownership: 'unknown', letterboxed: true });
  });

  it('stale_generation is not a failure; the owner re-sends after the next ownership', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7, 1);
    t.controller.setContainerSize(805, 349);
    vi.advanceTimersByTime(100);
    expect(t.stream().commands('resize')).toHaveLength(1);
    t.stream().emit(errorEvent({ code: 'stale_generation', attachment: 7 }));
    t.stream().emit(errorEvent({ code: 'unknown_attachment', attachment: 7 }));
    expect(t.controller.getView().status).toBeNull();
    vi.advanceTimersByTime(500);
    expect(t.stream().commands('resize')).toHaveLength(1);
    t.stream().emit(ownershipEvent(7, 2, 7));
    vi.advanceTimersByTime(100);
    expect(t.stream().commands('resize')).toEqual([
      { op: 'resize', attachment: '7', size: { cols: 100, rows: 20 }, ownershipGeneration: '1' },
      { op: 'resize', attachment: '7', size: { cols: 100, rows: 20 }, ownershipGeneration: '2' },
    ]);
  });

  it('another attachment error sets a status that the next ownership clears', async () => {
    const t = rig();
    await goLiveWithOwner(t, 8);
    t.stream().emit(errorEvent({ code: 'bad_input', attachment: 9 }));
    expect(t.controller.getView().status).toBeNull();
    t.stream().emit(errorEvent({ code: 'bad_input', attachment: 7 }));
    expect(t.controller.getView().status).toEqual({ kind: 'error', code: 'bad_input' });
    t.stream().emit(ownershipEvent(7, 2, 8));
    expect(t.controller.getView().status).toBeNull();
  });

  it('an applied started starts the owner resize timer', () => {
    const machine = new AttachmentMachine(TERM);
    const timerStarts = (effects: ReturnType<AttachmentMachine['handle']>): number =>
      effects.filter((effect) => effect.kind === 'startTimer' && effect.timer === 'resize').length;
    machine.handle({ kind: 'show' });
    machine.handle({ kind: 'event', event: connectionEvent(true) });
    machine.handle({ kind: 'event', event: attachedEvent(1, 7) });
    expect(timerStarts(machine.handle({ kind: 'event', event: ownershipEvent(7, 1, 7) }))).toBe(0);
    machine.handle({ kind: 'event', event: startedEvent(cursor(7, 1, 0)) });
    expect(machine.view().phase).toBe('live');
    expect(timerStarts(machine.handle({ kind: 'applied', cursor: cursor(7, 1, 0) }))).toBe(1);
  });
});

describe('terminalController display facts', () => {
  const cellOf = (width: number, height: number) => displayFrom(TERMINAL_THEME, { width, height });

  it('attach options carry the display from the theme and measured cell', () => {
    const machine = new AttachmentMachine(TERM);
    machine.handle({ kind: 'cellSize', cell: { width: 9.4, height: 18.2 } });
    machine.handle({ kind: 'containerSize', width: 940, height: 364 });
    machine.handle({ kind: 'event', event: connectionEvent(true) });
    const effects = machine.handle({ kind: 'show' });
    expect(effects).toContainEqual({
      kind: 'attach',
      terminal: TERM,
      options: {
        cols: 100,
        rows: 20,
        display: cellOf(9, 18),
        observe: false,
        history: TERMINAL_HISTORY_LINES,
      },
      instance: null,
    });
  });

  it('a cell size change sends one debounced set_display', async () => {
    const t = rig();
    await t.goLive();
    const adapter = t.fake.instances[0];
    adapter?.emitCellSize({ width: 9, height: 18 });
    vi.advanceTimersByTime(100);
    adapter?.emitCellSize({ width: 10, height: 18 });
    vi.advanceTimersByTime(249);
    expect(t.stream().commands('setDisplay')).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(t.stream().commands('setDisplay')).toEqual([
      { op: 'setDisplay', attachment: '7', display: cellOf(10, 18) },
    ]);
    vi.advanceTimersByTime(1000);
    expect(t.stream().commands('setDisplay')).toHaveLength(1);
  });

  it('a late font converges the cell, display facts and owner fit with the container held fixed', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7, 2);
    const adapter = t.fake.instances[0];
    t.controller.setContainerSize(800, 330);
    adapter?.emitCellSize({ width: 8, height: 15 });
    vi.advanceTimersByTime(250);
    expect(t.stream().commands('resize')).toEqual([
      { op: 'resize', attachment: '7', size: { cols: 100, rows: 22 }, ownershipGeneration: '2' },
    ]);
    expect(t.stream().commands('setDisplay')).toEqual([
      { op: 'setDisplay', attachment: '7', display: cellOf(8, 15) },
    ]);
    expect(adapter?.resizes).toEqual([]);

    adapter?.emitCellSize({ width: 8, height: 17 });
    vi.advanceTimersByTime(250);
    expect(t.stream().commands('resize')).toEqual([
      { op: 'resize', attachment: '7', size: { cols: 100, rows: 22 }, ownershipGeneration: '2' },
      { op: 'resize', attachment: '7', size: { cols: 100, rows: 19 }, ownershipGeneration: '2' },
    ]);
    expect(t.stream().commands('setDisplay')).toEqual([
      { op: 'setDisplay', attachment: '7', display: cellOf(8, 15) },
      { op: 'setDisplay', attachment: '7', display: cellOf(8, 17) },
    ]);
    expect(adapter?.resizes).toEqual([]);
  });

  it('set_display is never sent while not attached', async () => {
    const machine = new AttachmentMachine(TERM);
    const sentDisplay = (effects: ReturnType<AttachmentMachine['handle']>): boolean =>
      effects.some((effect) => effect.kind === 'setDisplay');
    machine.handle({ kind: 'show' });
    machine.handle({ kind: 'cellSize', cell: { width: 9, height: 18 } });
    expect(machine.view().phase).toBe('waiting');
    expect(sentDisplay(machine.handle({ kind: 'timer', timer: 'display' }))).toBe(false);
    machine.handle({ kind: 'event', event: connectionEvent(true) });
    expect(machine.view().phase).toBe('attaching');
    machine.handle({ kind: 'cellSize', cell: { width: 10, height: 18 } });
    expect(sentDisplay(machine.handle({ kind: 'timer', timer: 'display' }))).toBe(false);
    const named = machine.handle({ kind: 'event', event: attachedEvent(1, 7) });
    expect(named).toContainEqual({ kind: 'startTimer', timer: 'display', ms: 250 });
    expect(sentDisplay(machine.handle({ kind: 'timer', timer: 'display' }))).toBe(true);

    const t = rig();
    await t.goLive();
    t.fake.instances[0]?.emitCellSize({ width: 9, height: 18 });
    t.port.setConnected(HOST_ID, false);
    vi.advanceTimersByTime(1000);
    expect(t.stream().commands('setDisplay')).toEqual([]);
    t.controller.setVisible(false);
    vi.advanceTimersByTime(1000);
    expect(t.stream().commands('setDisplay')).toEqual([]);
  });

  it('an unchanged display is not re-sent', async () => {
    const t = rig();
    await t.goLive();
    const adapter = t.fake.instances[0];
    adapter?.emitCellSize({ width: 8, height: 17 });
    adapter?.emitCellSize({ width: 8.2, height: 16.8 });
    vi.advanceTimersByTime(1000);
    expect(t.stream().commands('setDisplay')).toEqual([]);
    adapter?.emitCellSize({ width: 9, height: 17 });
    vi.advanceTimersByTime(250);
    adapter?.emitCellSize({ width: 9.2, height: 17 });
    vi.advanceTimersByTime(1000);
    expect(t.stream().commands('setDisplay')).toHaveLength(1);
  });

  it('a display that changed while disconnected is sent once resumed', async () => {
    const t = rig();
    await t.goLive();
    t.fake.instances[0]?.emitCellSize({ width: 9, height: 18 });
    t.port.setConnected(HOST_ID, false);
    t.port.setConnected(HOST_ID, true);
    t.stream().emit(resumedEvent(2, 7, null));
    vi.advanceTimersByTime(250);
    expect(t.stream().commands('setDisplay')).toEqual([
      { op: 'setDisplay', attachment: '7', display: cellOf(9, 18) },
    ]);
  });
});

describe('terminalController automatic replies', () => {
  const REPLY = '\x1b[1;1R';

  async function liveOwnerWithWrite(t: Rig, owner: number): Promise<void> {
    await goLiveWithOwner(t, owner, 4);
    t.stream().emit(dataEvent(cursor(7, 1, 1), 'query'));
  }

  it('the owner forwards a DECXCPR reply with its epoch and generation', async () => {
    const t = rig();
    await liveOwnerWithWrite(t, 7);
    t.fake.instances[0]?.emitAutomaticReply(REPLY);
    const replies = t.stream().commands('automaticReply');
    expect(replies).toHaveLength(1);
    expect(replies[0]).toMatchObject({
      op: 'automaticReply',
      attachment: '7',
      epoch: '1',
      ownershipGeneration: '4',
    });
    expect(replies[0]?.op === 'automaticReply' && decoder.decode(replies[0].bytes)).toBe(REPLY);
    expect(t.stream().commands('input')).toEqual([]);
  });

  it('a non-owner drops DECXCPR replies', async () => {
    const t = rig();
    await liveOwnerWithWrite(t, 8);
    t.fake.instances[0]?.emitAutomaticReply(REPLY);
    expect(t.stream().commands('automaticReply')).toEqual([]);
    expect(t.stream().commands('input')).toEqual([]);
  });

  it('replies produced while applying replayed events are dropped', async () => {
    const t = rig();
    await resumeAfterDrop(t, 1);
    t.stream().emit(resumedEvent(2, 7, cursor(7, 1, 3)));
    t.stream().emit(ownershipEvent(7, 4, 7));
    t.stream().emit(dataEvent(cursor(7, 1, 2), 'two'));
    t.stream().emit(dataEvent(cursor(7, 1, 3), 'three'));
    t.fake.instances[0]?.emitAutomaticReply(REPLY);
    t.fake.instances[0]?.completeWrite();
    t.fake.instances[0]?.emitAutomaticReply(REPLY);
    expect(t.stream().commands('automaticReply')).toEqual([]);
  });

  it('replies after replay_through are forwarded', async () => {
    const t = rig();
    await resumeAfterDrop(t, 1);
    t.stream().emit(resumedEvent(2, 7, cursor(7, 1, 2)));
    t.stream().emit(ownershipEvent(7, 4, 7));
    t.stream().emit(dataEvent(cursor(7, 1, 2), 'two'));
    t.fake.instances[0]?.completeAll();
    t.stream().emit(dataEvent(cursor(7, 1, 3), 'live'));
    t.fake.instances[0]?.emitAutomaticReply(REPLY);
    expect(t.stream().commands('automaticReply')).toMatchObject([
      { op: 'automaticReply', attachment: '7', epoch: '1', ownershipGeneration: '4' },
    ]);
  });
});

describe('terminalController resize guards (test-verifier)', () => {
  it('ownership moving away during the resize debounce sends no resize', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7, 1);
    t.controller.setContainerSize(805, 349);
    vi.advanceTimersByTime(50);
    t.stream().emit(ownershipEvent(7, 2, 8));
    vi.advanceTimersByTime(500);
    expect(t.controller.getView().ownership).toBe('other');
    expect(t.stream().commands('resize')).toEqual([]);
  });

  it('a resize already sent at this generation and size is not sent again', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7, 1);
    t.controller.setContainerSize(805, 349);
    vi.advanceTimersByTime(100);
    expect(t.stream().commands('resize')).toHaveLength(1);
    // Same fit (100x20) from a different container size; the PTY has not answered with Resized.
    t.controller.setContainerSize(807, 351);
    vi.advanceTimersByTime(500);
    expect(t.stream().commands('resize')).toHaveLength(1);
  });

  it('a fresh attachment forgets the sent resize and sends again at the same generation', async () => {
    const t = rig();
    await goLiveWithOwner(t, 7, 1);
    t.controller.setContainerSize(805, 349);
    vi.advanceTimersByTime(100);
    expect(t.stream().commands('resize')).toHaveLength(1);
    t.stream().emit(gapEvent(7, null));
    expect(t.stream().commands('attach')).toHaveLength(2);
    t.stream().emit(attachedEvent(2, 8));
    t.stream().emit(ownershipEvent(8, 1, 8));
    t.stream().emit(startedEvent(cursor(8, 1, 0)));
    await Promise.resolve();
    vi.advanceTimersByTime(16);
    t.controller.setContainerSize(806, 350);
    vi.advanceTimersByTime(500);
    expect(t.stream().commands('resize')).toEqual([
      { op: 'resize', attachment: '7', size: { cols: 100, rows: 20 }, ownershipGeneration: '1' },
      { op: 'resize', attachment: '8', size: { cols: 100, rows: 20 }, ownershipGeneration: '1' },
    ]);
  });
});

describe('terminalController stream failure', () => {
  it('a refused stream shows the failure and retry opens it', async () => {
    const t = rig();
    t.port.failNextOpen('link down');
    await t.controller.start(t.host);
    t.controller.setVisible(true);
    expect(t.controller.getView()).toMatchObject({
      status: { kind: 'streamFailed', message: 'link down' },
      canRetry: true,
    });
    expect(t.port.streams).toEqual([]);
    t.controller.retryStream();
    await flush();
    expect(t.controller.getView()).toMatchObject({ status: null, canRetry: false });
    expect(t.port.streams).toHaveLength(1);
    expect(t.ops()).toEqual(['attach']);
    t.controller.retryStream();
    await flush();
    expect(t.port.streams).toHaveLength(1);
  });

  it('retry while the stream is opening does nothing', async () => {
    const t = rig();
    t.port.failNextOpen('link down');
    await t.controller.start(t.host);
    t.controller.retryStream();
    t.controller.retryStream();
    await flush();
    expect(t.port.streams).toHaveLength(1);
  });

  it('a stopped controller never retries', async () => {
    const t = rig();
    t.port.failNextOpen('link down');
    await t.controller.start(t.host);
    await t.controller.stop();
    t.controller.retryStream();
    await flush();
    expect(t.port.streams).toEqual([]);
  });

  it('events from a refused attempt are dropped', async () => {
    const t = rig();
    t.port.failNextOpen('link down');
    await t.controller.start(t.host);
    t.controller.setVisible(true);
    const refused = t.port.refusedListeners[0];
    if (refused === undefined) throw new Error('no refused attempt');
    t.controller.retryStream();
    await flush();
    expect(t.controller.getView().connected).toBe(true);
    refused(connectionEvent(false));
    refused(attachedEvent(9, 99));
    await flush();
    expect(t.controller.getView().connected).toBe(true);
    expect(t.controller.getView().phase).toBe('attaching');
  });
});

describe('terminalController claim', () => {
  it('a focused panel on a terminal with no owner stores its fit, then requests input', async () => {
    const t = rig();
    await t.controller.start(t.host);
    t.controller.setContainerSize(800, 340);
    t.controller.setFocused(true);
    t.controller.setVisible(true);
    t.stream().emit(attachedEvent(1, 7));
    t.stream().emit(startedEvent(cursor(7, 1, 0)));
    await Promise.resolve();
    t.stream().emit(ownershipEvent(7, 3, null));
    expect(t.stream().sent.map((command) => command.op).filter((op) => op !== 'attach')).toEqual([
      'resize',
      'requestOwnership',
    ]);
    expect(t.stream().commands('resize')).toEqual([
      { op: 'resize', attachment: '7', size: { cols: 100, rows: 20 }, ownershipGeneration: '3' },
    ]);
  });
});
