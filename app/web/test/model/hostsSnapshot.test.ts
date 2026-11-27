import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { isConnected, parseHostsSnapshot } from '../../src/model/hosts';
import { ModelError } from '../../src/model/ids';

const fixture: unknown = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '..', 'fixtures', 'hosts-snapshot.json'), 'utf8'),
);

function clone(): any {
  return JSON.parse(JSON.stringify(fixture));
}

describe('parseHostsSnapshot', () => {
  it('parses the shared fixture losslessly', () => {
    expect(JSON.parse(JSON.stringify(parseHostsSnapshot(fixture)))).toEqual(fixture);
  });

  it('ignores unknown fields', () => {
    const json = clone();
    json.extra = 1;
    json.hosts[0].extra = 2;
    json.hosts[0].connection.extra = 3;
    expect(parseHostsSnapshot(json)).toEqual(parseHostsSnapshot(fixture));
  });

  it('reads the backend, null before the host has named one', () => {
    expect(parseHostsSnapshot(fixture).hosts[0]?.backend).toEqual({
      id: 'herdr-core',
      instance: '4bcff4a58f4d97d505396e5a43a807e6',
    });
    expect(parseHostsSnapshot(fixture).hosts[1]?.backend).toBeNull();
  });

  it('accepts a backend with no instance', () => {
    const json = clone();
    json.hosts[0].backend.instance = null;
    expect(parseHostsSnapshot(json).hosts[0]?.backend).toEqual({ id: 'herdr-core', instance: null });
  });

  it('reports which hosts are connected', () => {
    expect(parseHostsSnapshot(fixture).hosts.map(isConnected)).toEqual([
      true,
      false,
      false,
      false,
      false,
    ]);
  });

  it.each([
    ['numeric revision', (j: any) => (j.revision = 1)],
    ['missing config', (j: any) => delete j.config],
    ['unknown connection kind', (j: any) => (j.hosts[0].connection.kind = 'asleep')],
    ['non-integer retryInMs', (j: any) => (j.hosts[1].connection.retryInMs = 1.5)],
    ['missing detail', (j: any) => delete j.hosts[1].connection.detail],
    ['unknown terminal state', (j: any) => (j.hosts[0].spaces[0].terminals[0].state = 'busy')],
    ['empty terminal id', (j: any) => (j.hosts[0].spaces[0].terminals[0].id = '')],
    ['uppercase host id', (j: any) => (j.hosts[0].hostId = j.hosts[0].hostId.toUpperCase())],
    [
      'spaces without a host id',
      (j: any) => (j.hosts[2].spaces = [j.hosts[1].spaces[0]]),
    ],
    [
      'space keyed by another host',
      (j: any) => (j.hosts[0].spaces[0].key.hostId = j.hosts[1].hostId),
    ],
    ['missing backend', (j: any) => delete j.hosts[0].backend],
    ['missing backend on a host with none yet', (j: any) => delete j.hosts[1].backend],
    ['non-object backend', (j: any) => (j.hosts[0].backend = 'herdr-core')],
    ['backend without an instance', (j: any) => delete j.hosts[0].backend.instance],
    ['backend without an id', (j: any) => delete j.hosts[0].backend.id],
    ['non-string backend instance', (j: any) => (j.hosts[0].backend.instance = 7)],
    ['non-u32 retryInMs', (j: any) => (j.hosts[1].connection.retryInMs = 4294967296)],
    ['negative attempt', (j: any) => (j.hosts[3].connection.attempt = -1)],
  ])('rejects %s', (_name, mutate) => {
    const json = clone();
    mutate(json);
    expect(() => parseHostsSnapshot(json)).toThrow(ModelError);
  });

  it('rejects two hosts sharing a host id', () => {
    const json = clone();
    json.hosts[3].hostId = json.hosts[0].hostId;
    expect(() => parseHostsSnapshot(json)).toThrow(/two hosts share/);
  });

  it('rejects non-objects', () => {
    expect(() => parseHostsSnapshot(null)).toThrow(ModelError);
    expect(() => parseHostsSnapshot([])).toThrow(ModelError);
  });
});
