import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  TERMINAL_AUTOMATIC_REPLY_COMMAND,
  TERMINAL_INPUT_COMMAND,
  TERMINAL_OPEN_STREAM_COMMAND,
  TERMINAL_RESIZE_COMMAND,
} from '../../src/ipc/names';
import { createTauriTerminalPort } from '../../src/ipc/tauriTerminal';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { TerminalStreamEvent } from '../../src/model/terminal';

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  channels: [] as { onmessage: (message: unknown) => void }[],
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: mocks.invoke,
  Channel: class {
    onmessage: (message: unknown) => void = () => undefined;
    constructor() {
      mocks.channels.push(this);
    }
  },
}));

const hostId = 'host-1' as HostId;
const u64 = (value: string) => value as DecimalU64;

function frame(attachment: bigint, epoch: bigint, seq: bigint, payload: number[]): Uint8Array {
  const bytes = new Uint8Array(24 + payload.length);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(0, attachment, false);
  view.setBigUint64(8, epoch, false);
  view.setBigUint64(16, seq, false);
  bytes.set(payload, 24);
  return bytes;
}

beforeEach(() => {
  mocks.invoke.mockReset();
  mocks.channels.length = 0;
  mocks.invoke.mockImplementation(async (command: string) =>
    command === TERMINAL_OPEN_STREAM_COMMAND ? '7' : undefined,
  );
});

describe('createTauriTerminalPort', () => {
  it('routes ArrayBuffer messages to data events', async () => {
    const events: TerminalStreamEvent[] = [];
    await createTauriTerminalPort().openStream(hostId, (event) => events.push(event));
    const channel = mocks.channels[0]!;
    const bytes = frame(1n, 2n, 18446744073709551615n, [104, 105]);
    channel.onmessage(bytes.buffer);
    const padded = new Uint8Array(bytes.length + 3);
    padded.set(bytes, 3);
    channel.onmessage(padded.subarray(3));
    expect(events).toHaveLength(2);
    for (const event of events) {
      expect(event).toEqual({
        type: 'data',
        cursor: { attachment: '1', epoch: '2', seq: '18446744073709551615' },
        bytes: new Uint8Array([104, 105]),
      });
    }
  });

  it('parses JSON messages as control events', async () => {
    const events: TerminalStreamEvent[] = [];
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    await createTauriTerminalPort().openStream(hostId, (event) => events.push(event));
    const channel = mocks.channels[0]!;
    channel.onmessage({ type: 'connection', connected: true });
    channel.onmessage({ type: 'nonsense' });
    channel.onmessage(new ArrayBuffer(3));
    expect(events).toEqual([{ type: 'connection', connected: true }]);
    expect(error).toHaveBeenCalledTimes(2);
    error.mockRestore();
  });

  it('sends input as a raw body with stream and attachment headers', async () => {
    const stream = await createTauriTerminalPort().openStream(hostId, () => undefined);
    expect(mocks.invoke).toHaveBeenCalledWith(TERMINAL_OPEN_STREAM_COMMAND, {
      hostId,
      channel: mocks.channels[0],
    });
    const body = new Uint8Array([1, 2, 3]);
    await stream.input(u64('9007199254740993'), body);
    expect(mocks.invoke).toHaveBeenLastCalledWith(TERMINAL_INPUT_COMMAND, body, {
      headers: { 'Combine-Stream': '7', 'Combine-Attachment': '9007199254740993' },
    });
    await stream.automaticReply(u64('9007199254740993'), u64('4'), u64('5'), body);
    expect(mocks.invoke).toHaveBeenLastCalledWith(TERMINAL_AUTOMATIC_REPLY_COMMAND, body, {
      headers: {
        'Combine-Stream': '7',
        'Combine-Attachment': '9007199254740993',
        'Combine-Epoch': '4',
        'Combine-Ownership-Generation': '5',
      },
    });
    await stream.resize(u64('3'), { cols: 80, rows: 24 }, u64('5'));
    expect(mocks.invoke).toHaveBeenLastCalledWith(TERMINAL_RESIZE_COMMAND, {
      stream: '7',
      attachment: '3',
      cols: 80,
      rows: 24,
      ownershipGeneration: '5',
    });
  });

  it('delivers events that arrive before open_stream resolves', async () => {
    const events: TerminalStreamEvent[] = [];
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command !== TERMINAL_OPEN_STREAM_COMMAND) return undefined;
      mocks.channels[0]!.onmessage({ type: 'connection', connected: false });
      return '7';
    });
    await createTauriTerminalPort().openStream(hostId, (event) => events.push(event));
    expect(events).toEqual([{ type: 'connection', connected: false }]);
  });

  it('passes a rejection message through unchanged', async () => {
    const stream = await createTauriTerminalPort().openStream(hostId, () => undefined);
    mocks.invoke.mockRejectedValueOnce('not connected');
    await expect(stream.focus(u64('1'))).rejects.toBe('not connected');
  });
});
