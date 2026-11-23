import type { SpawnPort } from '../../src/ipc/port';
import type { HostId, TermId } from '../../src/model/ids';
import type { SpawnSource, TemplateList } from '../../src/model/spawn';

export type SpawnCall =
  | { method: 'listTemplates'; hostId: HostId; space: string }
  | { method: 'spawnTerminal'; hostId: HostId; space: string; source: SpawnSource };

interface Deferred<T> {
  resolve: (value: T) => void;
  reject: (error: Error) => void;
}

type Script<T> = { kind: 'ok'; value: T } | { kind: 'error'; error: Error } | { kind: 'defer' };

export const emptyTemplateList = (space = '/work'): TemplateList => ({
  space,
  templates: [],
  skipped: [],
  error: null,
});

export class MemorySpawnPort implements SpawnPort {
  readonly calls: SpawnCall[] = [];
  private listScript: Script<TemplateList> = { kind: 'ok', value: emptyTemplateList() };
  private spawnScript: Script<TermId> = { kind: 'ok', value: 'hc:t1' as TermId };
  private readonly pendingLists: Deferred<TemplateList>[] = [];
  private readonly pendingSpawns: Deferred<TermId>[] = [];

  listCalls(): Extract<SpawnCall, { method: 'listTemplates' }>[] {
    return this.calls.filter((c) => c.method === 'listTemplates');
  }

  spawnCalls(): Extract<SpawnCall, { method: 'spawnTerminal' }>[] {
    return this.calls.filter((c) => c.method === 'spawnTerminal');
  }

  /** Every later `listTemplates` answers with this list. */
  setList(list: TemplateList): void {
    this.listScript = { kind: 'ok', value: list };
  }

  /** Every later `listTemplates` rejects with this message. */
  failList(message: string): void {
    this.listScript = { kind: 'error', error: new Error(message) };
  }

  /** Every later `listTemplates` waits for `resolveList` or `rejectList`. */
  deferList(): void {
    this.listScript = { kind: 'defer' };
  }

  /** Every later `spawnTerminal` answers with this terminal. */
  setSpawned(terminal: TermId): void {
    this.spawnScript = { kind: 'ok', value: terminal };
  }

  /** Every later `spawnTerminal` rejects with this message, or with this error itself. */
  failSpawn(failure: string | Error): void {
    this.spawnScript = {
      kind: 'error',
      error: typeof failure === 'string' ? new Error(failure) : failure,
    };
  }

  /** Every later `spawnTerminal` waits for `resolveSpawn` or `rejectSpawn`. */
  deferSpawn(): void {
    this.spawnScript = { kind: 'defer' };
  }

  pendingListCount(): number {
    return this.pendingLists.length;
  }

  pendingSpawnCount(): number {
    return this.pendingSpawns.length;
  }

  /** Resolves the oldest waiting `listTemplates`, or the one at `index`. */
  resolveList(list: TemplateList, index = 0): void {
    this.take(this.pendingLists, index).resolve(list);
  }

  rejectList(message: string, index = 0): void {
    this.take(this.pendingLists, index).reject(new Error(message));
  }

  resolveSpawn(terminal: TermId, index = 0): void {
    this.take(this.pendingSpawns, index).resolve(terminal);
  }

  rejectSpawn(message: string, index = 0): void {
    this.take(this.pendingSpawns, index).reject(new Error(message));
  }

  listTemplates(hostId: HostId, space: string): Promise<TemplateList> {
    this.calls.push({ method: 'listTemplates', hostId, space });
    return this.answer(this.listScript, this.pendingLists);
  }

  spawnTerminal(hostId: HostId, space: string, source: SpawnSource): Promise<TermId> {
    this.calls.push({ method: 'spawnTerminal', hostId, space, source });
    return this.answer(this.spawnScript, this.pendingSpawns);
  }

  private answer<T>(script: Script<T>, pending: Deferred<T>[]): Promise<T> {
    switch (script.kind) {
      case 'ok':
        return Promise.resolve(script.value);
      case 'error':
        return Promise.reject(script.error);
      case 'defer':
        return new Promise<T>((resolve, reject) => {
          pending.push({ resolve, reject });
        });
    }
  }

  private take<T>(pending: Deferred<T>[], index: number): Deferred<T> {
    const [entry] = pending.splice(index, 1);
    if (entry === undefined) throw new Error(`no waiting call at index ${index}`);
    return entry;
  }
}
