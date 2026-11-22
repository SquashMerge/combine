import { webcrypto } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { HOSTS_EVENT, HOSTS_SNAPSHOT_COMMAND, RETRY_HOST_COMMAND } from '../../src/ipc/names';
import { createTauriHostsPort } from '../../src/ipc/tauri';
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
