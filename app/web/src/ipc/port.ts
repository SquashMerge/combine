import type { HostsSnapshot } from '../model/hosts';

export type Unsubscribe = () => void;

export interface HostsPort {
  getSnapshot(): Promise<HostsSnapshot>;
  onSnapshot(listener: (snapshot: HostsSnapshot) => void): Promise<Unsubscribe>;
  retryHost(name: string): Promise<void>;
}
