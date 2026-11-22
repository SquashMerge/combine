import {
  type DecimalU64,
  ModelError,
  type TermId,
  compareDecimalU64,
  parseDecimalU64,
  parseTermId,
} from './ids';

export interface LastSpace {
  spaceId: string;
  selectedAtMs: DecimalU64;
}

export interface ClientSpaceState {
  spaceId: string;
  focusedTile: string | null;
  activeTerminal: TermId | null;
  activeTabs: Record<string, string>;
  groupState: unknown;
}

export interface ClientState {
  version: number;
  lastSpace: LastSpace | null;
  spaces: Record<string, ClientSpaceState>;
  error: string | null;
}

export interface LayoutTile {
  panels: readonly string[];
  active: string | null;
}

export type LayoutTiles = Readonly<Record<string, LayoutTile>>;

export interface RestoredSpaceState {
  focusedTile: string | null;
  activeTerminal: TermId | null;
  activeTabs: Record<string, string>;
}

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

export function emptySpaceState(spaceId: string): ClientSpaceState {
  return { spaceId, focusedTile: null, activeTerminal: null, activeTabs: {}, groupState: {} };
}

export function defaultClientState(error: string | null = null): ClientState {
  return { version: 1, lastSpace: null, spaces: {}, error };
}

function parseSpaceState(value: unknown, what: string): ClientSpaceState {
  const r = record(value, what);
  const tabs = record(r['activeTabs'], `${what}.activeTabs`);
  const activeTabs: Record<string, string> = {};
  for (const [tile, panel] of Object.entries(tabs)) {
    activeTabs[tile] = str(panel, `${what}.activeTabs.${tile}`);
  }
  const terminal = r['activeTerminal'];
  return {
    spaceId: str(r['spaceId'], `${what}.spaceId`),
    focusedTile: nullableStr(r['focusedTile'], `${what}.focusedTile`),
    activeTerminal: terminal === null ? null : parseTermId(terminal),
    activeTabs,
    groupState: r['groupState'],
  };
}

export function parseClientState(json: unknown): ClientState {
  const r = record(json, 'client state');
  const last = r['lastSpace'];
  const lastRecord = last === null ? null : record(last, 'client state.lastSpace');
  const spaces: Record<string, ClientSpaceState> = {};
  for (const [path, state] of Object.entries(record(r['spaces'], 'client state.spaces'))) {
    spaces[path] = parseSpaceState(state, `client state.spaces.${path}`);
  }
  const version = r['version'];
  if (typeof version !== 'number' || !Number.isInteger(version)) {
    throw new ModelError('client state.version must be an integer');
  }
  return {
    version,
    lastSpace:
      lastRecord === null
        ? null
        : {
            spaceId: str(lastRecord['spaceId'], 'client state.lastSpace.spaceId'),
            selectedAtMs: parseDecimalU64(lastRecord['selectedAtMs']),
          },
    spaces,
    error: nullableStr(r['error'], 'client state.error'),
  };
}

export function newestLastSpace<T extends { lastSpace: LastSpace | null }>(
  candidates: readonly T[],
): T | null {
  let best: T | null = null;
  for (const candidate of candidates) {
    if (candidate.lastSpace === null) continue;
    if (
      best === null ||
      best.lastSpace === null ||
      compareDecimalU64(candidate.lastSpace.selectedAtMs, best.lastSpace.selectedAtMs) > 0
    ) {
      best = candidate;
    }
  }
  return best;
}

export function pruneActiveTabs(
  tabs: Readonly<Record<string, string>>,
  layout: LayoutTiles,
): Record<string, string> {
  const kept: Record<string, string> = {};
  for (const [tile, panel] of Object.entries(tabs)) {
    const found = Object.hasOwn(layout, tile) ? layout[tile] : undefined;
    if (found?.panels.includes(panel)) kept[tile] = panel;
  }
  return kept;
}

/**
 * Applies a stored space state to what is on screen now. Values that no longer exist are dropped
 * and nothing is written back. A stored active tab wins over the tile's own `active`.
 */
export function restoreSpaceState(
  state: ClientSpaceState | undefined,
  layout: LayoutTiles,
  terminalIds: ReadonlySet<string>,
): RestoredSpaceState {
  const activeTabs: Record<string, string> = {};
  for (const [tile, { active }] of Object.entries(layout)) {
    if (active !== null) activeTabs[tile] = active;
  }
  if (state === undefined) return { focusedTile: null, activeTerminal: null, activeTabs };
  Object.assign(activeTabs, pruneActiveTabs(state.activeTabs, layout));
  return {
    focusedTile:
      state.focusedTile !== null && Object.hasOwn(layout, state.focusedTile)
        ? state.focusedTile
        : null,
    activeTerminal:
      state.activeTerminal !== null && terminalIds.has(state.activeTerminal)
        ? state.activeTerminal
        : null,
    activeTabs,
  };
}
