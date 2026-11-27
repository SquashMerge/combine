import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { LayoutPort, Unsubscribe } from '../../src/ipc/port';
import type { DecimalU64, HostId } from '../../src/model/ids';
import { decimalU64FromBigInt } from '../../src/model/ids';
import type { LayoutEvent, LayoutOp, LayoutSnapshot } from '../../src/model/layout';

type Listener = (event: LayoutEvent) => void;

export type LayoutCall =
  | { kind: 'watch'; hostId: HostId; space: string }
  | { kind: 'unwatch'; hostId: HostId }
  | {
      kind: 'mutate';
      hostId: HostId;
      space: string;
      baseLayoutRevision: DecimalU64;
      ops: LayoutOp[];
    };

export class MemoryLayoutPort implements LayoutPort {
  readonly calls: LayoutCall[] = [];
  unwatchError: string | null = null;
  watchGate: Promise<void> | null = null;
  /** When set, `onLayoutEvent` registers its listener only once this resolves, as Tauri's async `listen` does. */
  listenGate: Promise<void> | null = null;
  /** Called inside `watchLayout`, before it returns, so a test can reply to the watch at once. */
  onWatch: ((hostId: HostId, space: string, request: DecimalU64) => void) | null = null;
  private readonly listeners = new Set<Listener>();
  private nextRequest = 1n;

  watchLayout(hostId: HostId, space: string): Promise<DecimalU64> {
    this.calls.push({ kind: 'watch', hostId, space });
    const request = this.request();
    this.onWatch?.(hostId, space, request);
    return this.watchGate === null ? Promise.resolve(request) : this.watchGate.then(() => request);
  }

  unwatchLayout(hostId: HostId): Promise<void> {
    this.calls.push({ kind: 'unwatch', hostId });
    return this.unwatchError === null
      ? Promise.resolve()
      : Promise.reject(new Error(this.unwatchError));
  }

  mutateLayout(
    hostId: HostId,
    space: string,
    baseLayoutRevision: DecimalU64,
    ops: LayoutOp[],
  ): Promise<DecimalU64> {
    this.calls.push({ kind: 'mutate', hostId, space, baseLayoutRevision, ops });
    return Promise.resolve(this.request());
  }

  onLayoutEvent(listener: Listener): Promise<Unsubscribe> {
    const register = (): Unsubscribe => {
      this.listeners.add(listener);
      return () => {
        this.listeners.delete(listener);
      };
    };
    return this.listenGate === null ? Promise.resolve(register()) : this.listenGate.then(register);
  }

  emit(event: LayoutEvent): void {
    for (const listener of [...this.listeners]) listener(event);
  }

  push(hostId: HostId, snapshot: LayoutSnapshot, request: DecimalU64 | null = null): void {
    this.emit({ kind: 'snapshot', hostId, request, snapshot });
  }

  listenerCount(): number {
    return this.listeners.size;
  }

  private request(): DecimalU64 {
    const request = decimalU64FromBigInt(this.nextRequest);
    this.nextRequest += 1n;
    return request;
  }
}

/** The layout a fresh space starts with in the daemon: one tile, `main`, holding the follow-active tab. */
export function defaultLayoutSnapshot(space: string, layoutRevision = '1' as DecimalU64): LayoutSnapshot {
  return {
    space,
    layoutRevision,
    root: { type: 'tile', id: 'main', panels: ['.active-terminal'], active: '.active-terminal' },
    readOnly: false,
    error: null,
    dropped: null,
  };
}

export interface LayoutEventFixture {
  snapshot: Record<string, unknown>;
  push: Record<string, unknown>;
  error: Record<string, unknown>;
  disconnected: Record<string, unknown>;
  ops: Record<string, unknown>[];
}

export function loadLayoutEventFixture(): LayoutEventFixture {
  const file = resolve(import.meta.dirname, '..', 'fixtures', 'layout-event.json');
  return JSON.parse(readFileSync(file, 'utf8')) as LayoutEventFixture;
}
