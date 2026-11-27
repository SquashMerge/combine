import type { BackendView, TerminalSummary } from './hosts';
import type { HostId, TermId } from './ids';

/** The tab that shows the space's active terminal. A layout holds at most one. */
export const ACTIVE_TERMINAL_TAB = '.active-terminal';

const PINNED_PREFIX = '.terminal/';
const MAX_TAB_ID_BYTES = 256;
const MAX_INSTANCE_BYTES = 128;
const CONTROL = /\p{Cc}/u;

export interface ParsedTerminalTab {
  instance: string;
  terminal: TermId;
}

/** Why a pinned terminal tab can't show its terminal. */
export type GoneWhy = 'unlisted' | 'other-instance' | 'no-instance';

export type TerminalTabEntry =
  | { kind: 'follow-active'; terminal: TerminalSummary | null }
  | { kind: 'unknown' }
  | { kind: 'pending'; terminal: TermId }
  | { kind: 'gone'; why: GoneWhy; terminal: TermId }
  | { kind: 'pinned'; terminal: TerminalSummary };

export interface TerminalTabsContext {
  hostId: HostId;
  hostName: string;
  /** `null` until the host has named its backend. */
  backend: BackendView | null;
  /** Every terminal the host lists. */
  terminals: ReadonlyMap<TermId, TerminalSummary>;
  /** The selected space's terminals, in list order. */
  spaceTerminals: readonly TerminalSummary[];
  activeTerminal: TermId | null;
}

function validInstance(instance: string): boolean {
  return (
    instance.length > 0 &&
    !instance.includes('/') &&
    !CONTROL.test(instance) &&
    new TextEncoder().encode(instance).length <= MAX_INSTANCE_BYTES
  );
}

/** The ID of the tab pinned to a terminal, or null when no valid ID can be made. */
export function terminalTabId(instance: string, termId: TermId): string | null {
  if (!validInstance(instance) || termId === '') return null;
  const id = `${PINNED_PREFIX}${instance}/${termId}`;
  if (CONTROL.test(id) || new TextEncoder().encode(id).length > MAX_TAB_ID_BYTES) return null;
  return id;
}

export function parseTerminalTabId(id: string): ParsedTerminalTab | null {
  if (!id.startsWith(PINNED_PREFIX)) return null;
  const rest = id.slice(PINNED_PREFIX.length);
  const slash = rest.indexOf('/');
  if (slash <= 0 || slash === rest.length - 1) return null;
  const instance = rest.slice(0, slash);
  const terminal = rest.slice(slash + 1) as TermId;
  if (terminalTabId(instance, terminal) !== id) return null;
  return { instance, terminal };
}

/** A tab ID that starts with `.` is a terminal tab, whatever the panel list says. */
export function isTerminalTab(id: string): boolean {
  return id.startsWith('.');
}

/** The entry of a terminal tab, or null for any other ID. */
export function terminalTabEntry(id: string, ctx: TerminalTabsContext): TerminalTabEntry | null {
  if (!isTerminalTab(id)) return null;
  if (id === ACTIVE_TERMINAL_TAB) {
    const { activeTerminal } = ctx;
    return {
      kind: 'follow-active',
      terminal: activeTerminal === null ? null : (ctx.terminals.get(activeTerminal) ?? null),
    };
  }
  const parsed = parseTerminalTabId(id);
  if (parsed === null) return { kind: 'unknown' };
  const { backend } = ctx;
  if (backend === null) return { kind: 'pending', terminal: parsed.terminal };
  if (backend.instance === null) return { kind: 'gone', why: 'no-instance', terminal: parsed.terminal };
  if (backend.instance !== parsed.instance) {
    return { kind: 'gone', why: 'other-instance', terminal: parsed.terminal };
  }
  const terminal = ctx.terminals.get(parsed.terminal);
  if (terminal === undefined) return { kind: 'gone', why: 'unlisted', terminal: parsed.terminal };
  return { kind: 'pinned', terminal };
}

export function terminalTabLabel(id: string, entry: TerminalTabEntry): string {
  switch (entry.kind) {
    case 'follow-active':
      return entry.terminal === null ? 'Active terminal' : `Active: ${entry.terminal.name}`;
    case 'pinned':
      return entry.terminal.name;
    case 'pending':
    case 'gone':
      return entry.terminal;
    case 'unknown':
      return id;
  }
}

/** The `data-panel-entry` value of a terminal tab. */
export function terminalTabKind(entry: TerminalTabEntry): string {
  switch (entry.kind) {
    case 'follow-active':
      return 'active-terminal';
    case 'pinned':
      return 'terminal';
    case 'gone':
      return 'terminal-gone';
    case 'pending':
      return 'terminal-pending';
    case 'unknown':
      return 'missing';
  }
}
