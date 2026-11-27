import { type DecimalU64, ModelError, parseDecimalU64 } from './ids';
import type { LayoutTile, LayoutTiles } from './clientState';
import { type Dict, dictGet } from './dict';

export type SplitDirection = 'h' | 'v';

export type LayoutNode =
  | { type: 'split'; dir: SplitDirection; ratio: number; a: LayoutNode; b: LayoutNode }
  | { type: 'tile'; id: string; panels: string[]; active: string | null };

export interface DroppedNotice {
  opCount: number;
  message: string;
}

export interface LayoutSnapshot {
  space: string;
  layoutRevision: DecimalU64;
  root: LayoutNode;
  readOnly: boolean;
  error: string | null;
  dropped: DroppedNotice | null;
}

export type LayoutOp =
  | { op: 'set_active'; tile: string; panel: string }
  | { op: 'set_ratio'; leaves: string[]; ratio: number }
  | { op: 'move_panel'; panel: string; toTile: string; index: number }
  | {
      op: 'split_tile';
      tile: string;
      dir: SplitDirection;
      newTile: string;
      ratio: number;
      panel: string | null;
    }
  | { op: 'close_tile'; tile: string }
  | { op: 'add_panel'; panel: string; tile: string | null }
  | { op: 'remove_panel'; panel: string };

export type LayoutEvent =
  | { kind: 'snapshot'; hostId: string; request: DecimalU64 | null; snapshot: LayoutSnapshot }
  | { kind: 'error'; hostId: string; request: DecimalU64; code: string; message: string }
  | { kind: 'disconnected'; hostId: string };

function record(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new ModelError(`${what} must be an object`);
  }
  return value as Record<string, unknown>;
}

function str(value: unknown, what: string): string {
  if (typeof value !== 'string') throw new ModelError(`${what} must be a string`);
  return value;
}

function nullableStr(value: unknown, what: string): string | null {
  return value === null ? null : str(value, what);
}

function bool(value: unknown, what: string): boolean {
  if (typeof value !== 'boolean') throw new ModelError(`${what} must be a boolean`);
  return value;
}

function finite(value: unknown, what: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new ModelError(`${what} must be a finite number`);
  }
  return value;
}

function count(value: unknown, what: string): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) {
    throw new ModelError(`${what} must be a non-negative integer`);
  }
  return value;
}

function direction(value: unknown, what: string): SplitDirection {
  if (value !== 'h' && value !== 'v') throw new ModelError(`${what} must be "h" or "v"`);
  return value;
}

function strings(value: unknown, what: string): string[] {
  if (!Array.isArray(value)) throw new ModelError(`${what} must be an array`);
  return value.map((item, i) => str(item, `${what}[${i}]`));
}

export function parseLayoutNode(value: unknown, what = 'layout node'): LayoutNode {
  const r = record(value, what);
  const type = r['type'];
  if (type === 'tile') {
    return {
      type,
      id: str(r['id'], `${what}.id`),
      panels: strings(r['panels'], `${what}.panels`),
      active: nullableStr(r['active'], `${what}.active`),
    };
  }
  if (type === 'split') {
    return {
      type,
      dir: direction(r['dir'], `${what}.dir`),
      ratio: finite(r['ratio'], `${what}.ratio`),
      a: parseLayoutNode(r['a'], `${what}.a`),
      b: parseLayoutNode(r['b'], `${what}.b`),
    };
  }
  throw new ModelError(`${what}.type must be "split" or "tile"`);
}

export function parseLayoutSnapshot(value: unknown): LayoutSnapshot {
  const r = record(value, 'layout snapshot');
  const droppedValue = r['dropped'];
  let dropped: DroppedNotice | null = null;
  if (droppedValue !== null) {
    const d = record(droppedValue, 'layout snapshot.dropped');
    dropped = {
      opCount: count(d['opCount'], 'layout snapshot.dropped.opCount'),
      message: str(d['message'], 'layout snapshot.dropped.message'),
    };
  }
  return {
    space: str(r['space'], 'layout snapshot.space'),
    layoutRevision: parseDecimalU64(r['layoutRevision']),
    root: parseLayoutNode(r['root'], 'layout snapshot.root'),
    readOnly: bool(r['readOnly'], 'layout snapshot.readOnly'),
    error: nullableStr(r['error'], 'layout snapshot.error'),
    dropped,
  };
}

export function parseLayoutEvent(value: unknown): LayoutEvent {
  const r = record(value, 'layout event');
  const hostId = str(r['hostId'], 'layout event.hostId');
  switch (r['kind']) {
    case 'snapshot':
      return {
        kind: 'snapshot',
        hostId,
        request: r['request'] === null ? null : parseDecimalU64(r['request']),
        snapshot: parseLayoutSnapshot(r['snapshot']),
      };
    case 'error':
      return {
        kind: 'error',
        hostId,
        request: parseDecimalU64(r['request']),
        code: str(r['code'], 'layout event.code'),
        message: str(r['message'], 'layout event.message'),
      };
    case 'disconnected':
      return { kind: 'disconnected', hostId };
    default:
      throw new ModelError('layout event.kind must be "snapshot", "error" or "disconnected"');
  }
}

export function layoutTiles(root: LayoutNode): LayoutTiles {
  const tiles = new Map<string, LayoutTile>();
  const visit = (node: LayoutNode): void => {
    if (node.type === 'tile') {
      tiles.set(node.id, { panels: node.panels, active: node.active });
      return;
    }
    visit(node.a);
    visit(node.b);
  };
  visit(root);
  return tiles;
}

export function firstTileId(root: LayoutNode): string {
  return root.type === 'tile' ? root.id : firstTileId(root.a);
}

export function opTiles(op: LayoutOp): string[] {
  switch (op.op) {
    case 'set_active':
    case 'split_tile':
    case 'close_tile':
      return [op.tile];
    case 'set_ratio':
      return op.leaves;
    case 'move_panel':
      return [op.toTile];
    case 'add_panel':
    case 'remove_panel':
      return [];
  }
}

/** The tab a tile shows: the stored choice, else the tile's own active tab, else its first. */
export function shownTab(
  tile: { id: string; panels: readonly string[]; active: string | null },
  activeTabs: Readonly<Dict<string>>,
): string | null {
  return dictGet(activeTabs, tile.id) ?? tile.active ?? tile.panels[0] ?? null;
}

/** The ID of the tile whose panels hold the panel, or null when no tile does. */
export function tileHolding(root: LayoutNode, panel: string): string | null {
  if (root.type === 'tile') return root.panels.includes(panel) ? root.id : null;
  return tileHolding(root.a, panel) ?? tileHolding(root.b, panel);
}

/**
 * The tiles present in both trees whose active panel is not one the tile held before: a panel
 * the daemon put in front of the tile, as opposed to one the user chose from what was there.
 */
export function newlyActiveTiles(prev: LayoutNode, next: LayoutNode): string[] {
  const before = layoutTiles(prev);
  const named: string[] = [];
  for (const [id, tile] of layoutTiles(next)) {
    const old = before.get(id);
    if (old !== undefined && tile.active !== null && !old.panels.includes(tile.active)) {
      named.push(id);
    }
  }
  return named;
}
