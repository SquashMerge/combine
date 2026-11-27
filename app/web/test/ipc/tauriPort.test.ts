import { webcrypto } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import {
  CLIENT_STATE_COMMAND,
  HOSTS_EVENT,
  HOSTS_SNAPSHOT_COMMAND,
  RETRY_HOST_COMMAND,
  SET_CLIENT_SPACE_STATE_COMMAND,
  SET_LAST_SPACE_COMMAND,
} from '../../src/ipc/names';
import { createTauriClientStatePort, createTauriHostsPort } from '../../src/ipc/tauri';
import { type DecimalU64, type HostId, type TermId } from '../../src/model/ids';
import { type HostsSnapshot, parseHostsSnapshot } from '../../src/model/hosts';

const fixture: unknown = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'hosts-snapshot.json'), 'utf8'),
);

beforeAll(() => {
  if (!('crypto' in window)) {
    Object.defineProperty(window, 'crypto', { value: webcrypto });
  }
});

afterEach(() => clearMocks());

describe('createTauriHostsPort', () => {
  it('parses the hosts_snapshot command result', async () => {
    mockIPC((cmd) => (cmd === HOSTS_SNAPSHOT_COMMAND ? fixture : undefined), {
      shouldMockEvents: true,
    });
    expect(await createTauriHostsPort().getSnapshot()).toEqual(parseHostsSnapshot(fixture));
  });

  it('carries each host\'s backend through the parser', async () => {
    mockIPC((cmd) => (cmd === HOSTS_SNAPSHOT_COMMAND ? fixture : undefined), {
      shouldMockEvents: true,
    });
    const hosts = (await createTauriHostsPort().getSnapshot()).hosts;
    expect(hosts[0]?.backend).toEqual({
      id: 'herdr-core',
      instance: '4bcff4a58f4d97d505396e5a43a807e6',
    });
    expect(hosts[1]?.backend).toBeNull();
  });

  it('rejects a malformed hosts_snapshot result', async () => {
    mockIPC(() => ({ revision: 7 }), { shouldMockEvents: true });
    await expect(createTauriHostsPort().getSnapshot()).rejects.toThrow();
  });

  it('sends the host name with retry_host', async () => {
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC(
      (cmd, args) => {
        calls.push({ cmd, args });
      },
      { shouldMockEvents: true },
    );
    await createTauriHostsPort().retryHost('mini');
    expect(calls).toEqual([{ cmd: RETRY_HOST_COMMAND, args: { name: 'mini' } }]);
  });

  it('delivers parsed snapshots until unsubscribed', async () => {
    mockIPC(() => undefined, { shouldMockEvents: true });
    const received: HostsSnapshot[] = [];
    const unsubscribe = await createTauriHostsPort().onSnapshot((s) => received.push(s));

    await emit(HOSTS_EVENT, fixture);
    expect(received).toEqual([parseHostsSnapshot(fixture)]);

    unsubscribe();
    await emit(HOSTS_EVENT, fixture);
    expect(received).toHaveLength(1);
  });

  it('passes event payloads through the model parser', async () => {
    mockIPC(() => undefined, { shouldMockEvents: true });
    const received: HostsSnapshot[] = [];
    await createTauriHostsPort().onSnapshot((s) => received.push(s));

    await emit(HOSTS_EVENT, { ...(fixture as object), unexpected: true });
    expect(received).toEqual([parseHostsSnapshot(fixture)]);
  });
});

describe('createTauriClientStatePort', () => {
  const hostId = '0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c' as HostId;
  const clientFixture: unknown = JSON.parse(
    readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'client-state.json'), 'utf8'),
  );

  it('parses the client_state result for the host', async () => {
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      return clientFixture;
    });
    const state = await createTauriClientStatePort().load(hostId);
    expect(calls).toEqual([{ cmd: CLIENT_STATE_COMMAND, args: { hostId } }]);
    expect(state.lastSpace?.selectedAtMs).toBe('18446744073709551615');
  });

  it('sends the space state and the u64 timestamp as given', async () => {
    const calls: { cmd: string; args: unknown }[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
    });
    const port = createTauriClientStatePort();
    const state = {
      spaceId: '/work/a',
      focusedTile: null,
      activeTerminal: '7' as TermId,
      activeTabs: {},
      groupState: {},
    };
    await port.setSpaceState(hostId, '/work/a', state);
    await port.setLastSpace(hostId, '/work/a', '18446744073709551615' as DecimalU64);
    expect(calls).toEqual([
      { cmd: SET_CLIENT_SPACE_STATE_COMMAND, args: { hostId, path: '/work/a', state } },
      {
        cmd: SET_LAST_SPACE_COMMAND,
        args: { hostId, spaceId: '/work/a', selectedAtMs: '18446744073709551615' },
      },
    ]);
  });
});
