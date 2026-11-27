import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { type HostsSnapshot, parseHostsSnapshot } from '../../src/model/hosts';
import { U64_MAX } from '../../src/model/ids';
import { MemoryHostsPort } from '../support/memoryHostsPort';

const fixture = parseHostsSnapshot(
  JSON.parse(
    readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'hosts-snapshot.json'), 'utf8'),
  ),
);

describe('MemoryHostsPort', () => {
  it('bumps the revision past 2^53 without losing precision', () => {
    const port = new MemoryHostsPort({ ...fixture, revision: '9007199254740993' as HostsSnapshot['revision'] });
    const seen: HostsSnapshot[] = [];
    void port.onSnapshot((s) => seen.push(s));
    const name = fixture.hosts[0]?.name ?? '';
    port.setConnection(name, { kind: 'connected' });
    expect(seen[0]?.revision).toBe('9007199254740994');
    expect(seen[0]?.hosts[0]?.connection).toEqual({ kind: 'connected' });
  });

  it('publishes a host\'s backend with a bumped revision', () => {
    const port = new MemoryHostsPort({ ...fixture, revision: '7' as HostsSnapshot['revision'] });
    const seen: HostsSnapshot[] = [];
    void port.onSnapshot((s) => seen.push(s));
    port.setBackend('mini', { id: 'herdr-core', instance: 'abc' });
    expect(seen[0]?.hosts[1]?.backend).toEqual({ id: 'herdr-core', instance: 'abc' });
    expect(seen[0]?.hosts[0]?.backend).toEqual(fixture.hosts[0]?.backend);
    expect(seen[0]?.revision).toBe('8');
  });

  it('refuses to wrap past u64::MAX', () => {
    const port = new MemoryHostsPort({ ...fixture, revision: U64_MAX });
    expect(() => port.setConnection('mini', { kind: 'connected' })).toThrow(/out of range/);
  });

  it('stops notifying after unsubscribe', async () => {
    const port = new MemoryHostsPort(fixture);
    const unsubscribe = await port.onSnapshot(() => undefined);
    expect(port.listenerCount()).toBe(1);
    unsubscribe();
    expect(port.listenerCount()).toBe(0);
  });
});
