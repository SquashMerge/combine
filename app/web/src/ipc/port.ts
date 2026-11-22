import type { ClientSpaceState, ClientState } from '../model/clientState';
import type { HostsSnapshot } from '../model/hosts';
import type { DecimalU64, HostId } from '../model/ids';

export type Unsubscribe = () => void;

export interface HostsPort {
  getSnapshot(): Promise<HostsSnapshot>;
  onSnapshot(listener: (snapshot: HostsSnapshot) => void): Promise<Unsubscribe>;
  retryHost(name: string): Promise<void>;
}

export interface ClientStatePort {
  load(hostId: HostId): Promise<ClientState>;
  setSpaceState(hostId: HostId, path: string, state: ClientSpaceState): Promise<void>;
  setLastSpace(hostId: HostId, spaceId: string, selectedAtMs: DecimalU64): Promise<void>;
}
