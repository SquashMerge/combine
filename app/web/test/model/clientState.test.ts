import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  type ClientSpaceState,
  type LayoutTiles,
  defaultClientState,
  newestLastSpace,
  parseClientState,
  restoreSpaceState,
  storedSpaceState,
  withoutActiveTabs,
} from '../../src/model/clientState';
import { dictGet } from '../../src/model/dict';
import type { DecimalU64, TermId } from '../../src/model/ids';

const layout: LayoutTiles = new Map([
  ['t1', { panels: ['p1', 'p2'], active: 'p1' }],
  ['t2', { panels: ['p3'], active: null }],
]);

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

  it('restores an empty stored terminal as none', () => {
    expect(restoreSpaceState(stored({ activeTerminal: '' as TermId }), layout, terminals).activeTerminal).toBeNull();
  });

  it('writes nothing back to the stored state', () => {
    const before = stored({ focusedTile: 'gone', activeTabs: { gone: 'p1' } });
    const copy = structuredClone(before);
    restoreSpaceState(before, layout, terminals);
    expect(before).toEqual(copy);
  });
});

describe('client state keyed by IDs that name inherited properties', () => {
  const ids = ['__proto__', 'constructor', 'toString'];
  // An object literal's `__proto__:` sets its prototype instead of a key, so the wire text is
  // written out by hand.
  const text =
    '{"version":1,"lastSpace":null,"error":null,"spaces":{"__proto__":{"spaceId":"__proto__",' +
    '"focusedTile":"__proto__","activeTerminal":null,' +
    '"activeTabs":{"__proto__":"p2","constructor":"p2","toString":"p2"},"groupState":{}}}}';
  const tiles: LayoutTiles = new Map(ids.map((id) => [id, { panels: ['p1', 'p2'], active: 'p1' }]));

  it('parses spaces and active tabs under those keys as own entries', () => {
    const state = parseClientState(JSON.parse(text));
    const space = storedSpaceState(state, '__proto__');
    expect(space?.focusedTile).toBe('__proto__');
    expect(Object.keys(space?.activeTabs ?? {})).toEqual(ids);
    for (const id of ids) expect(dictGet(space?.activeTabs ?? {}, id)).toBe('p2');
    expect(storedSpaceState(state, 'constructor')).toBeUndefined();
  });

  it('restore keeps the focused tile and active tabs under those keys', () => {
    const state = parseClientState(JSON.parse(text));
    const restored = restoreSpaceState(storedSpaceState(state, '__proto__'), tiles, terminals);
    expect(restored.focusedTile).toBe('__proto__');
    expect(Object.keys(restored.activeTabs)).toEqual(ids);
    for (const id of ids) expect(dictGet(restored.activeTabs, id)).toBe('p2');
    expect(JSON.parse(JSON.stringify(restored.activeTabs))).toEqual(
      JSON.parse('{"__proto__":"p2","constructor":"p2","toString":"p2"}'),
    );
  });

  it('restore without stored state takes the tiles own active panels', () => {
    const restored = restoreSpaceState(undefined, tiles, terminals);
    for (const id of ids) expect(dictGet(restored.activeTabs, id)).toBe('p1');
  });

  it('finds nothing stored for an inherited name', () => {
    for (const id of ids) expect(storedSpaceState(defaultClientState(), id)).toBeUndefined();
    const restored = restoreSpaceState(undefined, new Map([['t1', { panels: [], active: null }]]), terminals);
    for (const id of ids) expect(dictGet(restored.activeTabs, id)).toBeUndefined();
  });
});

describe('withoutActiveTabs', () => {
  it('drops the listed tiles overrides and keeps the others', () => {
    const state = stored({ activeTabs: { t1: 'p2', t2: 'p3' } });
    const next = withoutActiveTabs(state, ['t1', 'gone']);
    expect(next).not.toBe(state);
    expect(JSON.parse(JSON.stringify(next.activeTabs))).toEqual({ t2: 'p3' });
    expect(state.activeTabs).toEqual({ t1: 'p2', t2: 'p3' });
  });

  it('returns the same object when no listed tile has an override', () => {
    const state = stored();
    expect(withoutActiveTabs(state, ['t2'])).toBe(state);
    expect(withoutActiveTabs(state, [])).toBe(state);
  });

  it('does not read an inherited property as an override', () => {
    const state = stored({ activeTabs: {} });
    expect(withoutActiveTabs(state, ['constructor', '__proto__'])).toBe(state);
  });
});
