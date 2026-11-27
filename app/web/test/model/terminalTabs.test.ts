import { describe, expect, it } from 'vitest';
import type { BackendView, TerminalSummary } from '../../src/model/hosts';
import type { HostId, TermId } from '../../src/model/ids';
import {
  ACTIVE_TERMINAL_TAB,
  type TerminalTabsContext,
  parseTerminalTabId,
  terminalTabEntry,
  terminalTabId,
  terminalTabKind,
  terminalTabLabel,
} from '../../src/model/terminalTabs';
import { HOST_A, terminal } from '../support/snapshots';

const build = terminal('hc:t8', 'build', 'make', 'running');
const exited = terminal('hc:t9', 'done', 'true', 'exited');
const BACKEND: BackendView = { id: 'herdr-core', instance: 'core-a' };

function context(over: Partial<TerminalTabsContext> = {}): TerminalTabsContext {
  const terminals: TerminalSummary[] = [build, exited];
  return {
    hostId: HOST_A as HostId,
    hostName: 'mac',
    backend: BACKEND,
    terminals: new Map(terminals.map((t) => [t.id, t])),
    spaceTerminals: terminals,
    activeTerminal: build.id,
    ...over,
  };
}

const termId = (id: string) => id as TermId;

describe('terminalTabId', () => {
  it('composes the pinned tab ID', () => {
    expect(terminalTabId('4bcff4a5', termId('hc:t8'))).toBe('.terminal/4bcff4a5/hc:t8');
  });

  it('is null for an invalid backend instance', () => {
    expect(terminalTabId('', termId('t1'))).toBeNull();
    expect(terminalTabId('a/b', termId('t1'))).toBeNull();
    expect(terminalTabId('a\u0007b', termId('t1'))).toBeNull();
    expect(terminalTabId('x'.repeat(129), termId('t1'))).toBeNull();
  });

  it('is null when a control character is in the terminal ID', () => {
    expect(terminalTabId('core', termId('t\n1'))).toBeNull();
    expect(terminalTabId('core', termId('t\u0000'))).toBeNull();
  });

  it('is null over 256 bytes, counting bytes and not characters', () => {
    const edge = 256 - '.terminal/core/'.length;
    expect(terminalTabId('core', termId('a'.repeat(edge)))).not.toBeNull();
    expect(terminalTabId('core', termId('a'.repeat(edge + 1)))).toBeNull();
    // Each é is two bytes but one character.
    expect(terminalTabId('core', termId('é'.repeat(Math.floor(edge / 2))))).not.toBeNull();
    expect(terminalTabId('core', termId('é'.repeat(Math.floor(edge / 2) + 1)))).toBeNull();
  });
});

describe('parseTerminalTabId', () => {
  it('round trips, splitting at the first slash after the instance', () => {
    expect(parseTerminalTabId('.terminal/core/hc:t8')).toEqual({ instance: 'core', terminal: 'hc:t8' });
    expect(parseTerminalTabId('.terminal/core/a/b')).toEqual({ instance: 'core', terminal: 'a/b' });
    const id = terminalTabId('core', termId('t1'));
    expect(parseTerminalTabId(id ?? '')).toEqual({ instance: 'core', terminal: 't1' });
  });

  it.each(['.terminal/', '.terminal/core', '.terminal/core/', '.terminal//t1', '.terminal', 'terminal/core/t1', '.active-terminal', '.terminal/core/t\n1'])(
    'is null for %j',
    (id) => {
      expect(parseTerminalTabId(id)).toBeNull();
    },
  );

  it('is null over 256 bytes', () => {
    expect(parseTerminalTabId(`.terminal/core/${'a'.repeat(300)}`)).toBeNull();
  });
});

describe('terminalTabEntry', () => {
  it('is null for an ID that does not start with a dot', () => {
    expect(terminalTabEntry('notes', context())).toBeNull();
    expect(terminalTabEntry('terminal/core/t1', context())).toBeNull();
  });

  it('follows the active terminal, or none', () => {
    expect(terminalTabEntry(ACTIVE_TERMINAL_TAB, context())).toEqual({ kind: 'follow-active', terminal: build });
    expect(terminalTabEntry(ACTIVE_TERMINAL_TAB, context({ activeTerminal: null }))).toEqual({
      kind: 'follow-active',
      terminal: null,
    });
    expect(terminalTabEntry(ACTIVE_TERMINAL_TAB, context({ activeTerminal: termId('zzz') }))).toEqual({
      kind: 'follow-active',
      terminal: null,
    });
  });

  it('follow-active comes before the backend checks', () => {
    expect(terminalTabEntry(ACTIVE_TERMINAL_TAB, context({ backend: null }))?.kind).toBe('follow-active');
  });

  it('an ID that does not parse is unknown, even before the backend is known', () => {
    expect(terminalTabEntry('.bogus', context({ backend: null }))).toEqual({ kind: 'unknown' });
    expect(terminalTabEntry('.terminal/core', context())).toEqual({ kind: 'unknown' });
  });

  it('is pending until the backend is named', () => {
    expect(terminalTabEntry('.terminal/core-a/hc:t8', context({ backend: null }))).toEqual({
      kind: 'pending',
      terminal: 'hc:t8',
    });
  });

  it('is gone when the host names no backend instance', () => {
    expect(
      terminalTabEntry('.terminal/core-a/hc:t8', context({ backend: { id: 'herdr-core', instance: null } })),
    ).toEqual({ kind: 'gone', why: 'no-instance', terminal: 'hc:t8' });
  });

  it('is gone for another backend instance, even when the terminal ID is listed', () => {
    expect(terminalTabEntry('.terminal/core-b/hc:t8', context())).toEqual({
      kind: 'gone',
      why: 'other-instance',
      terminal: 'hc:t8',
    });
  });

  it('is gone when the terminal is not listed', () => {
    expect(terminalTabEntry('.terminal/core-a/hc:t1', context())).toEqual({
      kind: 'gone',
      why: 'unlisted',
      terminal: 'hc:t1',
    });
  });

  it('is pinned to a listed terminal, running or exited', () => {
    expect(terminalTabEntry('.terminal/core-a/hc:t8', context())).toEqual({ kind: 'pinned', terminal: build });
    expect(terminalTabEntry('.terminal/core-a/hc:t9', context())).toEqual({ kind: 'pinned', terminal: exited });
  });

  it('a_listed_panel_with_a_reserved_id_is_still_a_terminal_tab', () => {
    // The context carries no panel list: a tab ID that starts with a dot never consults one.
    const entry = terminalTabEntry('.terminal/core-a/hc:t8', context());
    expect(entry?.kind).toBe('pinned');
    expect(terminalTabEntry('.something-else', context())?.kind).toBe('unknown');
  });
});

describe('terminalTabLabel and terminalTabKind', () => {
  const label = (id: string, ctx = context()) => {
    const entry = terminalTabEntry(id, ctx);
    if (entry === null) throw new Error('not a terminal tab');
    return [terminalTabLabel(id, entry), terminalTabKind(entry)];
  };

  it('names each kind', () => {
    expect(label(ACTIVE_TERMINAL_TAB)).toEqual(['Active: build', 'active-terminal']);
    expect(label(ACTIVE_TERMINAL_TAB, context({ activeTerminal: null }))).toEqual(['Active terminal', 'active-terminal']);
    expect(label('.terminal/core-a/hc:t8')).toEqual(['build', 'terminal']);
    expect(label('.terminal/core-a/hc:t1')).toEqual(['hc:t1', 'terminal-gone']);
    expect(label('.terminal/core-a/hc:t8', context({ backend: null }))).toEqual(['hc:t8', 'terminal-pending']);
    expect(label('.bogus')).toEqual(['.bogus', 'missing']);
  });
});
