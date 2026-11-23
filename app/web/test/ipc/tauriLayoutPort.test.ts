import { webcrypto } from 'node:crypto';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import {
  LAYOUT_EVENT,
  MUTATE_LAYOUT_COMMAND,
  UNWATCH_LAYOUT_COMMAND,
  WATCH_LAYOUT_COMMAND,
} from '../../src/ipc/names';
import { createTauriLayoutPort } from '../../src/ipc/tauri';
import type { DecimalU64, HostId } from '../../src/model/ids';
import { type LayoutEvent, type LayoutOp, parseLayoutEvent } from '../../src/model/layout';
import { loadLayoutEventFixture } from '../support/memoryLayoutPort';

const fixture = loadLayoutEventFixture();
const hostId = 'host-1' as HostId;

beforeAll(() => {
  if (!('crypto' in window)) {
    Object.defineProperty(window, 'crypto', { value: webcrypto });
  }
});

afterEach(() => clearMocks());

describe('createTauriLayoutPort', () => {
  it('sends watch, unwatch and mutate with camelCase arguments and returns the request id', async () => {
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC(
      (cmd, args) => {
        calls.push({ cmd, args });
        return cmd === UNWATCH_LAYOUT_COMMAND ? undefined : '9007199254740993';
      },
      { shouldMockEvents: true },
    );
    const port = createTauriLayoutPort();
    const ops = fixture.ops as unknown as LayoutOp[];
    expect(await port.watchLayout(hostId, '/work')).toBe('9007199254740993');
    await port.unwatchLayout(hostId);
    expect(await port.mutateLayout(hostId, '/work', '18446744073709551615' as DecimalU64, ops)).toBe(
      '9007199254740993',
    );
    expect(calls).toEqual([
      { cmd: WATCH_LAYOUT_COMMAND, args: { hostId, space: '/work' } },
      { cmd: UNWATCH_LAYOUT_COMMAND, args: { hostId } },
      {
        cmd: MUTATE_LAYOUT_COMMAND,
        args: { hostId, space: '/work', baseLayoutRevision: '18446744073709551615', ops },
      },
    ]);
  });

  it('rejects a request id that is not a decimal string', async () => {
    mockIPC(() => 7, { shouldMockEvents: true });
    await expect(createTauriLayoutPort().watchLayout(hostId, '/work')).rejects.toThrow();
  });

  it('surfaces a not connected error', async () => {
    mockIPC(
      () => {
        throw 'not connected';
      },
      { shouldMockEvents: true },
    );
    await expect(createTauriLayoutPort().unwatchLayout(hostId)).rejects.toBe('not connected');
  });

  it('delivers parsed layout events until unsubscribed', async () => {
    mockIPC(() => undefined, { shouldMockEvents: true });
    const received: LayoutEvent[] = [];
    const unsubscribe = await createTauriLayoutPort().onLayoutEvent((event) => received.push(event));
    await emit(LAYOUT_EVENT, fixture.snapshot);
    await emit(LAYOUT_EVENT, fixture.error);
    await emit(LAYOUT_EVENT, fixture.disconnected);
    expect(received).toEqual([
      parseLayoutEvent(fixture.snapshot),
      parseLayoutEvent(fixture.error),
      parseLayoutEvent(fixture.disconnected),
    ]);
    unsubscribe();
    await emit(LAYOUT_EVENT, fixture.disconnected);
    expect(received).toHaveLength(3);
  });
});
