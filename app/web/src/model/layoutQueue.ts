import { type DecimalU64, compareDecimalU64 } from './ids';
import {
  type LayoutOp,
  type LayoutSnapshot,
  type SplitDirection,
  layoutTiles,
  opTiles,
} from './layout';

export const STALE_NOTICE = "Layout changed elsewhere; your change wasn't applied.";

const STALE_LAYOUT = 'stale_layout';
const SPLIT_RATIO = 0.5;

export type SendMutation = (base: DecimalU64, ops: LayoutOp[]) => Promise<DecimalU64>;

export interface LayoutQueueOptions {
  send: SendMutation;
  onNotice: (message: string) => void;
  newTileId?: () => string;
}

interface Intent {
  ops: LayoutOp[];
  stale: boolean;
}

interface InFlight {
  intent: Intent;
  base: DecimalU64;
  request: DecimalU64 | null;
}

type Reply = { kind: 'ok' } | { kind: 'error'; code: string; message: string };

function defaultNewTileId(): string {
  return `t-${crypto.randomUUID()}`;
}

function errorText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

/**
 * Sends one space's layout intents one at a time. Each is based on the newest snapshot held, and
 * the reply to one releases the next. A `stale_layout` reply is retried once, as soon as a newer
 * snapshot than the base is held.
 */
export class LayoutQueue {
  private readonly send: SendMutation;
  private readonly onNotice: (message: string) => void;
  private readonly newTileId: () => string;
  private held: LayoutSnapshot | null = null;
  private readonly intents: Intent[] = [];
  private flight: InFlight | null = null;
  private waiting: { intent: Intent; base: DecimalU64 } | null = null;
  private readonly early = new Map<DecimalU64, Reply>();
  private generation = 0;

  constructor({ send, onNotice, newTileId = defaultNewTileId }: LayoutQueueOptions) {
    this.send = send;
    this.onNotice = onNotice;
    this.newTileId = newTileId;
  }

  enqueue(ops: LayoutOp[]): void {
    this.intents.push({ ops, stale: false });
    this.pump();
  }

  split(tile: string, dir: SplitDirection, panel: string | null = null): void {
    this.enqueue([
      { op: 'split_tile', tile, dir, newTile: this.newTileId(), ratio: SPLIT_RATIO, panel },
    ]);
  }

  close(tile: string): void {
    this.enqueue([{ op: 'close_tile', tile }]);
  }

  addPanel(panel: string): void {
    this.enqueue([{ op: 'add_panel', panel, tile: null }]);
  }

  /** Puts the panel in the named tile. */
  addPanelTo(panel: string, tile: string): void {
    this.enqueue([{ op: 'add_panel', panel, tile }]);
  }

  /** Splits `tile` to the right and puts the panel in the new tile, as one batch. */
  openBeside(tile: string, panel: string): void {
    const newTile = this.newTileId();
    this.enqueue([
      { op: 'split_tile', tile, dir: 'h', newTile, ratio: SPLIT_RATIO, panel: null },
      { op: 'add_panel', panel, tile: newTile },
    ]);
  }

  /** Adds `next` to the tile, then removes `previous`. */
  replacePanel(previous: string, next: string, tile: string): void {
    this.enqueue([
      { op: 'add_panel', panel: next, tile },
      { op: 'remove_panel', panel: previous },
    ]);
  }

  movePanelTo(panel: string, toTile: string, index: number): void {
    this.enqueue([{ op: 'move_panel', panel, toTile, index }]);
  }

  removePanel(panel: string): void {
    this.enqueue([{ op: 'remove_panel', panel }]);
  }

  onSnapshot(request: DecimalU64 | null, snapshot: LayoutSnapshot): void {
    if (this.held === null || compareDecimalU64(snapshot.layoutRevision, this.held.layoutRevision) >= 0) {
      this.held = snapshot;
    }
    if (request !== null) this.reply(request, { kind: 'ok' });
    const { waiting, held } = this;
    if (waiting !== null && held !== null && compareDecimalU64(held.layoutRevision, waiting.base) > 0) {
      this.waiting = null;
      this.resend(waiting.intent);
    }
    this.pump();
  }

  onError(request: DecimalU64, code: string, message: string): void {
    this.reply(request, { kind: 'error', code, message });
    this.pump();
  }

  onDisconnected(): void {
    this.generation += 1;
    this.held = null;
    this.intents.length = 0;
    this.flight = null;
    this.waiting = null;
    this.early.clear();
  }

  private reply(request: DecimalU64, reply: Reply): void {
    const { flight } = this;
    if (flight === null) return;
    if (flight.request === null) {
      this.early.set(request, reply);
      return;
    }
    if (flight.request !== request) return;
    this.flight = null;
    if (reply.kind === 'ok') return;
    if (reply.code !== STALE_LAYOUT) {
      this.onNotice(reply.message);
      return;
    }
    this.stale(flight);
  }

  private stale({ intent, base }: InFlight): void {
    if (intent.stale) {
      this.onNotice(STALE_NOTICE);
      return;
    }
    intent.stale = true;
    if (this.held !== null && compareDecimalU64(this.held.layoutRevision, base) > 0) {
      this.resend(intent);
    } else {
      this.waiting = { intent, base };
    }
  }

  private pump(): void {
    while (this.flight === null && this.waiting === null && this.held !== null) {
      const intent = this.intents.shift();
      if (intent === undefined) return;
      this.resend(intent);
    }
  }

  private resend(intent: Intent): void {
    const { held } = this;
    if (held === null) return;
    const tiles = layoutTiles(held.root);
    if (!intent.ops.every((op) => opTiles(op).every((tile) => tiles.has(tile)))) {
      this.onNotice(STALE_NOTICE);
      return;
    }
    const flight: InFlight = { intent, base: held.layoutRevision, request: null };
    this.flight = flight;
    const generation = this.generation;
    this.send(flight.base, intent.ops).then(
      (request) => {
        if (generation !== this.generation) return;
        flight.request = request;
        const early = this.early.get(request);
        if (early === undefined) return;
        this.early.delete(request);
        this.reply(request, early);
        this.pump();
      },
      (cause: unknown) => {
        if (generation !== this.generation) return;
        this.flight = null;
        this.intents.length = 0;
        this.onNotice(errorText(cause));
      },
    );
  }
}
