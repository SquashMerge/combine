import type { ClientStatePort } from '../../src/ipc/port';
import {
  type ClientSpaceState,
  type ClientState,
  defaultClientState,
} from '../../src/model/clientState';
import type { DecimalU64, HostId } from '../../src/model/ids';

export type ClientStateWrite =
  | { kind: 'space'; hostId: HostId; path: string; state: ClientSpaceState }
  | { kind: 'last'; hostId: HostId; spaceId: string; selectedAtMs: DecimalU64 };

export class MemoryClientStatePort implements ClientStatePort {
  readonly writes: ClientStateWrite[] = [];
  private readonly states = new Map<HostId, ClientState>();

  constructor(initial: ReadonlyMap<HostId, ClientState> = new Map()) {
    for (const [hostId, state] of initial) this.states.set(hostId, state);
  }

  load(hostId: HostId): Promise<ClientState> {
    return Promise.resolve(this.states.get(hostId) ?? defaultClientState());
  }

  setSpaceState(hostId: HostId, path: string, state: ClientSpaceState): Promise<void> {
    this.writes.push({ kind: 'space', hostId, path, state });
    const current = this.states.get(hostId) ?? defaultClientState();
    this.states.set(hostId, { ...current, spaces: { ...current.spaces, [path]: state } });
    return Promise.resolve();
  }

  setLastSpace(hostId: HostId, spaceId: string, selectedAtMs: DecimalU64): Promise<void> {
    this.writes.push({ kind: 'last', hostId, spaceId, selectedAtMs });
    const current = this.states.get(hostId) ?? defaultClientState();
    this.states.set(hostId, { ...current, lastSpace: { spaceId, selectedAtMs } });
    return Promise.resolve();
  }
}
