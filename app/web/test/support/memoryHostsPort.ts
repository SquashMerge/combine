import type { HostsPort, Unsubscribe } from '../../src/ipc/port';
import type { BackendView, ConnectionState, HostsSnapshot } from '../../src/model/hosts';
import { decimalU64FromBigInt, decimalU64ToBigInt } from '../../src/model/ids';

type Listener = (snapshot: HostsSnapshot) => void;

/** One `onSnapshot` call. Like Tauri's `listen`, each call registers separately, even with the same function. */
interface Subscription {
  listener: Listener;
  onInvalid: ((cause: unknown) => void) | undefined;
}

export class MemoryHostsPort implements HostsPort {
  readonly retryCalls: string[] = [];
  private current: HostsSnapshot;
  private readonly subscriptions = new Set<Subscription>();

  constructor(initial: HostsSnapshot) {
    this.current = initial;
  }

  getSnapshot(): Promise<HostsSnapshot> {
    return Promise.resolve(this.current);
  }

  onSnapshot(listener: Listener, onInvalid?: (cause: unknown) => void): Promise<Unsubscribe> {
    const subscription: Subscription = { listener, onInvalid };
    this.subscriptions.add(subscription);
    return Promise.resolve(() => {
      this.subscriptions.delete(subscription);
    });
  }

  retryHost(name: string): Promise<void> {
    this.retryCalls.push(name);
    return Promise.resolve();
  }

  push(next: HostsSnapshot): void {
    this.current = next;
    for (const { listener } of [...this.subscriptions]) listener(next);
  }

  /** Delivers a `hosts-changed` payload the model refused, as the Tauri port reports one. */
  pushInvalid(cause: unknown): void {
    for (const { onInvalid } of [...this.subscriptions]) onInvalid?.(cause);
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
    return this.subscriptions.size;
  }
}
