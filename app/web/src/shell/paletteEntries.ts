import type { HostsSnapshot } from '../model/hosts';
import type { TermId } from '../model/ids';
import type { PanelSummary } from '../model/panels';
import { ACTIVE_TERMINAL_TAB } from '../model/terminalTabs';
import { type SpaceKey, folderName, spaceKeyId } from '../model/space';
import type { Searchable } from './fuzzy';
import { describeConnection } from './connectionText';
import { sortSpaces } from './order';

export type PaletteAction =
  | { kind: 'switch-space'; space: SpaceKey }
  | { kind: 'focus-terminal'; space: SpaceKey; terminalId: TermId }
  | { kind: 'retry-host'; hostName: string }
  | { kind: 'show-panel'; space: SpaceKey; panel: string; tile: string | null }
  | { kind: 'close-panel'; space: SpaceKey; panel: string }
  | { kind: 'pin-panel'; space: SpaceKey; panel: string }
  | { kind: 'show-active-terminal'; space: SpaceKey; tile: string };

export interface PaletteEntry extends Searchable {
  id: string;
  action: PaletteAction;
}

export interface PaletteHandlers {
  onSelectSpace: (space: SpaceKey) => void;
  onSelectTerminal: (space: SpaceKey, terminalId: TermId) => void;
  onRetryHost: (hostName: string) => void;
  onShowPanel?: (space: SpaceKey, panel: string, tile: string | null) => void;
  onClosePanel?: (space: SpaceKey, panel: string) => void;
  onPinPanel?: (space: SpaceKey, panel: string) => void;
  onShowActiveTerminal?: (space: SpaceKey, tile: string) => void;
}

/** The watched space's panels, as the shell knows them; `editable` is false while the layout is held. */
export interface PalettePanels {
  space: SpaceKey;
  hostName: string;
  panels: readonly PanelSummary[];
  /** Panel ID to the ID of the tile holding it. */
  tiles: ReadonlyMap<string, string>;
  /** The focused tile's shown tab; `label` is the tab's name when it isn't a listed panel's title. */
  focused: { tile: string; panel: string; label?: string } | null;
  /** The focused tile, even when it shows nothing. */
  focusedTile?: string | null;
  editable: boolean;
}

const GROUP_PANEL_ACTION = 0;
const GROUP_SPACE = 1;
const GROUP_TERMINAL = 2;
const GROUP_PANEL = 3;
const GROUP_HOST = 4;

function panelActionEntries(panels: PalettePanels): PaletteEntry[] {
  const { focused, space } = panels;
  if (!panels.editable) return [];
  const key = spaceKeyId(space);
  const folder = folderName(space.path);
  const entries: PaletteEntry[] = [];
  if (focused !== null) {
    const summary = panels.panels.find((panel) => panel.id === focused.panel);
    const detail = `${summary?.title ?? focused.label ?? focused.panel} in ${folder}`;
    entries.push({
      id: `close-panel:${key}:${focused.panel}`,
      group: GROUP_PANEL_ACTION,
      label: 'Close panel',
      detail,
      action: { kind: 'close-panel', space, panel: focused.panel },
    });
    if (summary?.lifetime === 'scratch') {
      entries.push({
        id: `pin-panel:${key}:${focused.panel}`,
        group: GROUP_PANEL_ACTION,
        label: 'Pin panel',
        detail,
        action: { kind: 'pin-panel', space, panel: focused.panel },
      });
    }
  }
  const focusedTile = panels.focusedTile ?? focused?.tile ?? null;
  if (focusedTile !== null && focused?.panel !== ACTIVE_TERMINAL_TAB) {
    entries.push({
      id: `show-active-terminal:${key}:${focusedTile}`,
      group: GROUP_PANEL_ACTION,
      label: 'Show the active terminal here',
      detail: `in ${folder}`,
      action: { kind: 'show-active-terminal', space, tile: focusedTile },
    });
  }
  return entries;
}

function panelEntries(panels: PalettePanels): PaletteEntry[] {
  const { space } = panels;
  const key = spaceKeyId(space);
  const entries: PaletteEntry[] = [];
  for (const summary of panels.panels) {
    if (summary.state === 'removed') continue;
    const tile = panels.tiles.get(summary.id) ?? null;
    if (tile === null && !panels.editable) continue;
    const state = summary.state.replaceAll('_', ' ');
    entries.push({
      id: `panel:${key}:${summary.id}`,
      group: GROUP_PANEL,
      label: summary.title ?? summary.id,
      detail:
        `${panels.hostName} ${folderName(space.path)} ${summary.renderer} ${state}` +
        (tile === null ? ', not in a tile' : ''),
      action: { kind: 'show-panel', space, panel: summary.id, tile },
    });
  }
  return entries;
}

export function buildPaletteEntries(
  snapshot: HostsSnapshot,
  panels: PalettePanels | null = null,
): PaletteEntry[] {
  const entries: PaletteEntry[] = panels === null ? [] : panelActionEntries(panels);
  for (const host of snapshot.hosts) {
    for (const space of sortSpaces(host.spaces)) {
      entries.push({
        id: `space:${spaceKeyId(space.key)}`,
        group: GROUP_SPACE,
        label: folderName(space.key.path),
        detail: `${host.name} ${space.key.path}`,
        action: { kind: 'switch-space', space: space.key },
      });
    }
  }
  for (const host of snapshot.hosts) {
    for (const space of sortSpaces(host.spaces)) {
      for (const terminal of space.terminals) {
        entries.push({
          id: `terminal:${spaceKeyId(space.key)}:${terminal.id}`,
          group: GROUP_TERMINAL,
          label: terminal.name,
          detail: `${host.name} ${folderName(space.key.path)} ${terminal.command}`,
          action: { kind: 'focus-terminal', space: space.key, terminalId: terminal.id },
        });
      }
    }
  }
  if (panels !== null) entries.push(...panelEntries(panels));
  for (const host of snapshot.hosts) {
    if (host.connection.kind !== 'failed') continue;
    entries.push({
      id: `retry:${host.name}`,
      group: GROUP_HOST,
      label: `Retry ${host.name}`,
      detail: describeConnection(host.connection),
      action: { kind: 'retry-host', hostName: host.name },
    });
  }
  return entries;
}

export function runPaletteAction(action: PaletteAction, handlers: PaletteHandlers): void {
  switch (action.kind) {
    case 'switch-space':
      handlers.onSelectSpace(action.space);
      return;
    case 'focus-terminal':
      handlers.onSelectTerminal(action.space, action.terminalId);
      return;
    case 'retry-host':
      handlers.onRetryHost(action.hostName);
      return;
    case 'show-panel':
      handlers.onShowPanel?.(action.space, action.panel, action.tile);
      return;
    case 'close-panel':
      handlers.onClosePanel?.(action.space, action.panel);
      return;
    case 'pin-panel':
      handlers.onPinPanel?.(action.space, action.panel);
      return;
    case 'show-active-terminal':
      handlers.onShowActiveTerminal?.(action.space, action.tile);
      return;
  }
}
