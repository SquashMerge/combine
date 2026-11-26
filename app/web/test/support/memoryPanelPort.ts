import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { PanelPort, Unsubscribe } from '../../src/ipc/port';
import type { HostId } from '../../src/model/ids';
import {
  type PanelContent,
  PanelFetchError,
  PanelFetchQueue,
  type PanelsChanged,
  type PanelsEvent,
  type RenderReport,
} from '../../src/model/panels';

type Listener = (event: PanelsEvent) => void;

export interface RawSend {
  hostId: HostId;
  space: string;
  panel: string;
  resolve: (content: PanelContent) => void;
  reject: (error: PanelFetchError) => void;
}

/** One `close_panel` or `pin_panel` call, left unanswered until the test settles it. */
export interface PanelAction {
  kind: 'close' | 'pin';
  hostId: HostId;
  space: string;
  panel: string;
  resolve: () => void;
  reject: (error: PanelFetchError) => void;
}

/** A panel port over a real PanelFetchQueue, with a raw `get_panel` send the test can answer. */
export class MemoryPanelPort implements PanelPort {
  readonly sends: RawSend[] = [];
  readonly actions: PanelAction[] = [];
  readonly reports: { hostId: HostId; report: RenderReport }[] = [];
  reportResult = true;
  /** When set, `onPanelsEvent` registers its listener only once this resolves, as Tauri's async `listen` does. */
  listenGate: Promise<void> | null = null;
  /** The daemon drops a connection with more unflushed panel replies than this. */
  daemonLimit = 16;
  tripped = false;
  outstanding = 0;
  maxOutstanding = 0;
  private readonly listeners = new Set<Listener>();
  private readonly queue: PanelFetchQueue;
  private answers: ((panel: string) => PanelContent | undefined) | null = null;

  constructor(limit?: number) {
    this.queue = new PanelFetchQueue((hostId, space, panel) => this.rawSend(hostId, space, panel), limit);
  }

  autoAnswer(answers: Record<string, PanelContent> | ((panel: string) => PanelContent | undefined)): void {
    this.answers = typeof answers === 'function' ? answers : (panel) => answers[panel];
  }

  onPanelsEvent(listener: Listener): Promise<Unsubscribe> {
    const register = (): Unsubscribe => {
      this.listeners.add(listener);
      return () => {
        this.listeners.delete(listener);
      };
    };
    return this.listenGate === null ? Promise.resolve(register()) : this.listenGate.then(register);
  }

  fetchPanel(hostId: HostId, space: string, panel: string, urgent: boolean): Promise<PanelContent> {
    return this.queue.fetch(hostId, space, panel, urgent);
  }

  closePanel(hostId: HostId, space: string, panel: string): Promise<void> {
    return this.act('close', hostId, space, panel);
  }

  pinPanel(hostId: HostId, space: string, panel: string): Promise<void> {
    return this.act('pin', hostId, space, panel);
  }

  reportRender(hostId: HostId, report: RenderReport): Promise<boolean> {
    this.reports.push({ hostId, report });
    return Promise.resolve(this.reportResult);
  }

  emit(event: PanelsEvent): void {
    if (event.kind === 'disconnected') {
      this.queue.disconnected(event.hostId as HostId);
      for (const send of this.sends.filter((s) => s.hostId === event.hostId)) {
        send.reject(new PanelFetchError(null, 'The host disconnected.'));
      }
    }
    for (const listener of [...this.listeners]) listener(event);
  }

  listenerCount(): number {
    return this.listeners.size;
  }

  private act(kind: PanelAction['kind'], hostId: HostId, space: string, panel: string): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.actions.push({ kind, hostId, space, panel, resolve, reject });
    });
  }

  private rawSend(hostId: HostId, space: string, panel: string): Promise<PanelContent> {
    return new Promise<PanelContent>((resolve, reject) => {
      let settled = false;
      const settle = (): boolean => {
        if (settled) return false;
        settled = true;
        this.outstanding -= 1;
        return true;
      };
      const send: RawSend = {
        hostId,
        space,
        panel,
        resolve: (content) => {
          if (settle()) resolve(content);
        },
        reject: (error) => {
          if (settle()) reject(error);
        },
      };
      this.sends.push(send);
      this.outstanding += 1;
      this.maxOutstanding = Math.max(this.maxOutstanding, this.outstanding);
      if (this.outstanding > this.daemonLimit) {
        this.tripped = true;
        this.emit({ kind: 'disconnected', hostId });
        return;
      }
      const answer = this.answers?.(panel);
      if (answer !== undefined) queueMicrotask(() => send.resolve(answer));
    });
  }
}

export type PanelsFixture = Record<'list' | 'push' | 'loading' | 'panel' | 'closed' | 'pinned' | 'error' | 'disconnected', Record<string, unknown>>;

export function loadPanelsEventFixture(): PanelsFixture {
  const file = resolve(import.meta.dirname, '..', 'fixtures', 'panels-event.json');
  return JSON.parse(readFileSync(file, 'utf8')) as PanelsFixture;
}

export type { PanelsChanged };
