import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import {
  type BackendView,
  type ConnectionState,
  type HostView,
  type HostsSnapshot,
  type SpaceView,
  type TerminalSummary,
  parseHostsSnapshot,
} from '../../src/model/hosts';
import { type DecimalU64, type HostId, type TermId } from '../../src/model/ids';

export const HOST_A = '0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c' as HostId;
export const HOST_B = '6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c' as HostId;

export function loadFixtureSnapshot(): HostsSnapshot {
  const file = resolve(import.meta.dirname, '..', 'fixtures', 'hosts-snapshot.json');
  return parseHostsSnapshot(JSON.parse(readFileSync(file, 'utf8')));
}

export function terminal(id: string, name: string, command = name, state: TerminalSummary['state'] = 'idle'): TerminalSummary {
  return { id: id as TermId, name, command, state };
}

export function space(hostId: HostId, path: string, terminals: TerminalSummary[] = []): SpaceView {
  return { key: { hostId, path }, terminals };
}

export function host(
  name: string,
  hostId: HostId | null,
  connection: ConnectionState,
  spaces: SpaceView[] = [],
  backend?: BackendView | null,
): HostView {
  return backend === undefined
    ? { name, hostId, connection, spaces }
    : { name, hostId, connection, spaces, backend };
}

export function snapshot(hosts: HostView[], revision = '1', error: string | null = null): HostsSnapshot {
  return {
    revision: revision as DecimalU64,
    config: { path: '/cfg/hosts.json', error },
    hosts,
  };
}
