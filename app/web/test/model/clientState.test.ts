import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  type ClientSpaceState,
  newestLastSpace,
  parseClientState,
  restoreSpaceState,
} from '../../src/model/clientState';
import type { DecimalU64, TermId } from '../../src/model/ids';

const layout = {
  t1: { panels: ['p1', 'p2'], active: 'p1' },
  t2: { panels: ['p3'], active: null },
};

function stored(overrides: Partial<ClientSpaceState> = {}): ClientSpaceState {
  return {
    spaceId: 'space-1',
    focusedTile: 't1',
    activeTerminal: '7' as TermId,
    activeTabs: { t1: 'p2' },
    groupState: {},
    ...overrides,
  };
}

const terminals = new Set(['7', '8']);

describe('parseClientState', () => {
  it('reads the shared fixture without a number round trip', () => {
    const file = resolve(import.meta.dirname, '..', 'fixtures', 'client-state.json');
    const state = parseClientState(JSON.parse(readFileSync(file, 'utf8')));
    expect(state.lastSpace?.selectedAtMs).toBe('18446744073709551615');
    expect(state.spaces['/work/a']?.activeTabs).toEqual({ t1: 'p2' });
    expect(state.error).toBeNull();
  });

  it('accepts an empty activeTerminal, as the state store does', () => {
    const file = resolve(import.meta.dirname, '..', 'fixtures', 'client-state.json');
    const json = JSON.parse(readFileSync(file, 'utf8')) as {
      spaces: Record<string, { activeTerminal: unknown }>;
    };
    json.spaces['/work/a']!.activeTerminal = '';
    expect(parseClientState(json).spaces['/work/a']?.activeTerminal).toBe('');
    json.spaces['/work/a']!.activeTerminal = 7;
    expect(() => parseClientState(json)).toThrow();
  });

  it('rejects a timestamp that is not a decimal string', () => {
    expect(() =>
      parseClientState({ version: 1, lastSpace: { spaceId: 'a', selectedAtMs: 5 }, spaces: {}, error: null }),
    ).toThrow();
  });
});

describe('newestLastSpace', () => {
  it('compares timestamps above 2^53 exactly', () => {
    const at = (ms: string) => ({ lastSpace: { spaceId: 'x', selectedAtMs: ms as DecimalU64 } });
    const a = at('9007199254740992');
    const b = at('9007199254740993');
    expect(newestLastSpace([a, { lastSpace: null }, b])).toBe(b);
    expect(newestLastSpace([{ lastSpace: null }])).toBeNull();
  });
});

describe('restoreSpaceState', () => {
  it('drops tile not in layout', () => {
    const restored = restoreSpaceState(
      stored({ focusedTile: 'gone', activeTabs: { gone: 'p1', t1: 'p2' } }),
      layout,
      terminals,
    );
    expect(restored.focusedTile).toBeNull();
    expect(restored.activeTabs).not.toHaveProperty('gone');
    expect(restored.activeTabs['t1']).toBe('p2');
  });

  it('drops active tab for missing tile or panel', () => {
    const restored = restoreSpaceState(
      stored({ activeTabs: { t1: 'p9', t3: 'p1', t2: 'p3' } }),
      layout,
      terminals,
    );
    expect(restored.activeTabs).toEqual({ t1: 'p1', t2: 'p3' });
  });

  it('drops terminal not on host', () => {
    expect(restoreSpaceState(stored({ activeTerminal: '99' as TermId }), layout, terminals).activeTerminal).toBeNull();
    expect(restoreSpaceState(stored(), layout, terminals).activeTerminal).toBe('7');
  });

  it('active_tabs overrides Tile.active', () => {
    const restored = restoreSpaceState(stored({ activeTabs: { t1: 'p2' } }), layout, terminals);
    expect(restored.activeTabs['t1']).toBe('p2');
    expect(restoreSpaceState(undefined, layout, terminals).activeTabs).toEqual({ t1: 'p1' });
  });

  it('writes nothing back to the stored state', () => {
    const before = stored({ focusedTile: 'gone', activeTabs: { gone: 'p1' } });
    const copy = structuredClone(before);
    restoreSpaceState(before, layout, terminals);
    expect(before).toEqual(copy);
  });
});
