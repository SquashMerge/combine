import type { ClientStatePort } from '../../src/ipc/port';
import type { ClientSpaceState, ClientState } from '../../src/model/clientState';
import type { DecimalU64, HostId } from '../../src/model/ids';
import type { ClientStateWrite } from './memoryClientStatePort';

interface Gate {
  resolve: (state: ClientState) => void;
  reject: (cause: Error) => void;
}

export class DeferredClientStatePort implements ClientStatePort {
  readonly loads: HostId[] = [];
  readonly writes: ClientStateWrite[] = [];
  failWrites: Error | null = null;
  private readonly gates = new Map<HostId, Gate>();

  load(hostId: HostId): Promise<ClientState> {
    this.loads.push(hostId);
    return new Promise((resolve, reject) => {
      this.gates.set(hostId, { resolve, reject });
    });
  }

  resolve(hostId: HostId, state: ClientState): void {
    this.gates.get(hostId)?.resolve(state);
  }

  reject(hostId: HostId, message: string): void {
    this.gates.get(hostId)?.reject(new Error(message));
  }

  setSpaceState(hostId: HostId, path: string, state: ClientSpaceState): Promise<void> {
    this.writes.push({ kind: 'space', hostId, path, state });
    return this.failWrites === null ? Promise.resolve() : Promise.reject(this.failWrites);
  }

  setLastSpace(hostId: HostId, spaceId: string, selectedAtMs: DecimalU64): Promise<void> {
    this.writes.push({ kind: 'last', hostId, spaceId, selectedAtMs });
    return this.failWrites === null ? Promise.resolve() : Promise.reject(this.failWrites);
  }
}
