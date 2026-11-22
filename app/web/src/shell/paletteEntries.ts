import type { HostsSnapshot } from '../model/hosts';
import type { TermId } from '../model/ids';
import { type SpaceKey, folderName, spaceKeyId } from '../model/space';
import type { Searchable } from './fuzzy';
import { describeConnection } from './connectionText';
import { sortSpaces } from './order';

export type PaletteAction =
  | { kind: 'switch-space'; space: SpaceKey }
  | { kind: 'focus-terminal'; space: SpaceKey; terminalId: TermId }
  | { kind: 'retry-host'; hostName: string };

export interface PaletteEntry extends Searchable {
  id: string;
  action: PaletteAction;
}

export interface PaletteHandlers {
  onSelectSpace: (space: SpaceKey) => void;
  onSelectTerminal: (space: SpaceKey, terminalId: TermId) => void;
  onRetryHost: (hostName: string) => void;
}

const GROUP_SPACE = 0;
const GROUP_TERMINAL = 1;
const GROUP_HOST = 2;

export function buildPaletteEntries(snapshot: HostsSnapshot): PaletteEntry[] {
  const entries: PaletteEntry[] = [];
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
  }
}
