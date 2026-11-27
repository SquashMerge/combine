import type { HostsPort, Unsubscribe } from '../../src/ipc/port';
import type { BackendView, ConnectionState, HostsSnapshot } from '../../src/model/hosts';
import { decimalU64FromBigInt, decimalU64ToBigInt } from '../../src/model/ids';

type Listener = (snapshot: HostsSnapshot) => void;

export class MemoryHostsPort implements HostsPort {
  readonly retryCalls: string[] = [];
  private current: HostsSnapshot;
  private readonly listeners = new Set<Listener>();

  constructor(initial: HostsSnapshot) {
    this.current = initial;
  }

  getSnapshot(): Promise<HostsSnapshot> {
    return Promise.resolve(this.current);
  }

  onSnapshot(listener: Listener): Promise<Unsubscribe> {
    this.listeners.add(listener);
    return Promise.resolve(() => {
      this.listeners.delete(listener);
    });
  }

  retryHost(name: string): Promise<void> {
    this.retryCalls.push(name);
    return Promise.resolve();
  }

  push(next: HostsSnapshot): void {
    this.current = next;
    for (const listener of [...this.listeners]) listener(next);
  }

  setConnection(name: string, state: ConnectionState): void {
    const revision = decimalU64FromBigInt(decimalU64ToBigInt(this.current.revision) + 1n);
    this.push({
      ...this.current,
      revision,
      hosts: this.current.hosts.map((host) =>
        host.name === name ? { ...host, connection: state } : host,
      ),
    });
  }

  setBackend(name: string, backend: BackendView | null): void {
    const revision = decimalU64FromBigInt(decimalU64ToBigInt(this.current.revision) + 1n);
    this.push({
      ...this.current,
      revision,
      hosts: this.current.hosts.map((host) => (host.name === name ? { ...host, backend } : host)),
    });
  }

  listenerCount(): number {
    return this.listeners.size;
  }
}
