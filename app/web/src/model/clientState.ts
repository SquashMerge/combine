import { type Dict, dictGet, dictWithout, emptyDict } from './dict';
import {
  type DecimalU64,
  ModelError,
  type TermId,
  asRecord,
  compareDecimalU64,
  parseDecimalU64,
} from './ids';

export interface LastSpace {
  spaceId: string;
  selectedAtMs: DecimalU64;
}

export interface ClientSpaceState {
  spaceId: string;
  focusedTile: string | null;
  activeTerminal: TermId | null;
  activeTabs: Dict<string>;
  groupState: unknown;
}

export interface ClientState {
  version: number;
  lastSpace: LastSpace | null;
  spaces: Dict<ClientSpaceState>;
  error: string | null;
}

export interface LayoutTile {
  panels: readonly string[];
  active: string | null;
}

export type LayoutTiles = ReadonlyMap<string, LayoutTile>;

export interface RestoredSpaceState {
  focusedTile: string | null;
  activeTerminal: TermId | null;
  activeTabs: Dict<string>;
}

function str(value: unknown, what: string): string {
  if (typeof value !== 'string') throw new ModelError(`${what} must be a string`);
  return value;
}

function nullableStr(value: unknown, what: string): string | null {
  return value === null ? null : str(value, what);
}

export function emptySpaceState(spaceId: string): ClientSpaceState {
  return { spaceId, focusedTile: null, activeTerminal: null, activeTabs: emptyDict(), groupState: {} };
}

export function defaultClientState(error: string | null = null): ClientState {
  return { version: 1, lastSpace: null, spaces: emptyDict(), error };
}

function parseSpaceState(value: unknown, what: string): ClientSpaceState {
  const r = asRecord(value, what);
  const tabs = asRecord(r['activeTabs'], `${what}.activeTabs`);
  const activeTabs = emptyDict<string>();
  for (const [tile, panel] of Object.entries(tabs)) {
    activeTabs[tile] = str(panel, `${what}.activeTabs.${tile}`);
  }
  const terminal = r['activeTerminal'];
  return {
    spaceId: str(r['spaceId'], `${what}.spaceId`),
    focusedTile: nullableStr(r['focusedTile'], `${what}.focusedTile`),
    activeTerminal: terminal === null ? null : (str(terminal, `${what}.activeTerminal`) as TermId),
    activeTabs,
    groupState: r['groupState'],
  };
}

export function parseClientState(json: unknown): ClientState {
  const r = asRecord(json, 'client state');
  const last = r['lastSpace'];
  const lastRecord = last === null ? null : asRecord(last, 'client state.lastSpace');
  const spaces = emptyDict<ClientSpaceState>();
  for (const [path, state] of Object.entries(asRecord(r['spaces'], 'client state.spaces'))) {
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

export function pruneActiveTabs(tabs: Readonly<Dict<string>>, layout: LayoutTiles): Dict<string> {
  const kept = emptyDict<string>();
  for (const [tile, panel] of Object.entries(tabs)) {
    if (layout.get(tile)?.panels.includes(panel)) kept[tile] = panel;
  }
  return kept;
}

/** The space state without the stored active tab of each listed tile; the same object when none has one. */
export function withoutActiveTabs(
  state: ClientSpaceState,
  tiles: readonly string[],
): ClientSpaceState {
  if (!tiles.some((tile) => dictGet(state.activeTabs, tile) !== undefined)) return state;
  return { ...state, activeTabs: dictWithout(state.activeTabs, tiles) };
}

/** The stored state of the space at `path`, if there is one. */
export function storedSpaceState(
  state: ClientState | undefined,
  path: string,
): ClientSpaceState | undefined {
  return state === undefined ? undefined : dictGet(state.spaces, path);
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
  const activeTabs = emptyDict<string>();
  for (const [tile, { active }] of layout) {
    if (active !== null) activeTabs[tile] = active;
  }
  if (state === undefined) return { focusedTile: null, activeTerminal: null, activeTabs };
  for (const [tile, panel] of Object.entries(pruneActiveTabs(state.activeTabs, layout))) {
    activeTabs[tile] = panel;
  }
  return {
    focusedTile:
      state.focusedTile !== null && layout.has(state.focusedTile) ? state.focusedTile : null,
    activeTerminal:
      state.activeTerminal !== null && terminalIds.has(state.activeTerminal)
        ? state.activeTerminal
        : null,
    activeTabs,
  };
}
