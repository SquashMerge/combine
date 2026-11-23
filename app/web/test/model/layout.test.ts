import { describe, expect, it } from 'vitest';
import type { DecimalU64 } from '../../src/model/ids';
import {
  type LayoutEvent,
  type LayoutNode,
  type LayoutOp,
  firstTileId,
  layoutTiles,
  opTiles,
  parseLayoutEvent,
} from '../../src/model/layout';
import { loadLayoutEventFixture } from '../support/memoryLayoutPort';

/** Valid opaque tile IDs that name properties every plain object inherits. */
const INHERITED = ['__proto__', 'constructor', 'toString', 'hasOwnProperty'];

const fixture = loadLayoutEventFixture();

describe('parseLayoutEvent', () => {
  it('decodes the shared snapshot fixture without a number round trip', () => {
    const event = parseLayoutEvent(fixture.snapshot);
    expect(event.kind).toBe('snapshot');
    if (event.kind !== 'snapshot') return;
    expect(event.hostId).toBe('host-1');
    expect(event.request).toBe('7');
    expect(event.snapshot.layoutRevision).toBe('9007199254740993');
    expect(event.snapshot.readOnly).toBe(true);
    expect(event.snapshot.error).toBe('layout.json is invalid');
    expect(event.snapshot.dropped).toEqual({ opCount: 2, message: 'dropped' });
    expect(event.snapshot.root).toEqual({
      type: 'split',
      dir: 'h',
      ratio: 0.25,
      a: { type: 'tile', id: 't1', panels: ['p1', 'p2'], active: 'p2' },
      b: { type: 'tile', id: 't2', panels: [], active: null },
    });
  });

  it('decodes a push with no request', () => {
    const event = parseLayoutEvent(fixture.push);
    expect(event.kind === 'snapshot' && event.request).toBeNull();
  });

  it('decodes the error and disconnected fixtures', () => {
    expect(parseLayoutEvent(fixture.error)).toEqual({
      kind: 'error',
      hostId: 'host-1',
      request: '8' as DecimalU64,
      code: 'stale_layout',
      message: 'stale',
    } satisfies LayoutEvent);
    expect(parseLayoutEvent(fixture.disconnected)).toEqual({
      kind: 'disconnected',
      hostId: 'host-1',
    });
  });

  it('rejects an unknown kind', () => {
    expect(() => parseLayoutEvent({ ...fixture.disconnected, kind: 'exploded' })).toThrow(
      /layout event.kind/,
    );
  });

  it('rejects a revision or request that is not a decimal string', () => {
    const snapshot = fixture.snapshot['snapshot'] as Record<string, unknown>;
    expect(() =>
      parseLayoutEvent({ ...fixture.snapshot, snapshot: { ...snapshot, layoutRevision: 9 } }),
    ).toThrow();
    expect(() => parseLayoutEvent({ ...fixture.error, request: 8 })).toThrow();
  });

  it('rejects a malformed tree', () => {
    const snapshot = fixture.snapshot['snapshot'] as Record<string, unknown>;
    const root = snapshot['root'] as Record<string, unknown>;
    const withRoot = (bad: unknown) => ({ ...fixture.snapshot, snapshot: { ...snapshot, root: bad } });
    expect(() => parseLayoutEvent(withRoot({ ...root, ratio: 'wide' }))).toThrow();
    expect(() => parseLayoutEvent(withRoot({ ...root, ratio: null }))).toThrow();
    expect(() => parseLayoutEvent(withRoot({ ...root, dir: 'x' }))).toThrow(/dir/);
    expect(() => parseLayoutEvent(withRoot({ type: 'grid' }))).toThrow(/type/);
    expect(() => parseLayoutEvent(withRoot({ type: 'tile', id: 't', panels: [1], active: null }))).toThrow();
    expect(() => parseLayoutEvent(withRoot({ type: 'tile', id: 't', panels: [] }))).toThrow();
  });

  it('rejects a ratio that is not finite', () => {
    const snapshot = fixture.snapshot['snapshot'] as Record<string, unknown>;
    const root = snapshot['root'] as Record<string, unknown>;
    const bad = { ...fixture.snapshot, snapshot: { ...snapshot, root: { ...root, ratio: Infinity } } };
    expect(() => parseLayoutEvent(bad)).toThrow(/finite/);
  });

  it('rejects a payload that is not an object', () => {
    expect(() => parseLayoutEvent(null)).toThrow();
    expect(() => parseLayoutEvent([])).toThrow();
  });
});

describe('layout helpers', () => {
  const root = (() => {
    const event = parseLayoutEvent(fixture.snapshot);
    if (event.kind !== 'snapshot') throw new Error('fixture is a snapshot');
    return event.snapshot.root;
  })();

  it('lists the tiles of a tree', () => {
    expect(layoutTiles(root)).toEqual(
      new Map([
        ['t1', { panels: ['p1', 'p2'], active: 'p2' }],
        ['t2', { panels: [], active: null }],
      ]),
    );
    expect(firstTileId(root)).toBe('t1');
  });

  it('names the tiles an op refers to', () => {
    const tiles = (fixture.ops as unknown as LayoutOp[]).map(opTiles);
    expect(tiles).toEqual([['t1'], ['t1', 't2'], ['t2'], ['t1'], ['t3']]);
  });
});

describe('layoutTiles with tile IDs that name inherited properties', () => {
  it.each(INHERITED)('lists and finds tile %s as its own entry', (id) => {
    const root: LayoutNode = {
      type: 'split',
      dir: 'h',
      ratio: 0.5,
      a: { type: 'tile', id, panels: ['p1'], active: 'p1' },
      b: { type: 'tile', id: 't2', panels: [], active: null },
    };
    const tiles = layoutTiles(root);
    expect([...tiles.keys()]).toEqual([id, 't2']);
    expect(tiles.get(id)).toEqual({ panels: ['p1'], active: 'p1' });
    expect(firstTileId(root)).toBe(id);
  });

  it('does not report inherited names as tiles', () => {
    const tiles = layoutTiles({ type: 'tile', id: 't1', panels: [], active: null });
    for (const id of INHERITED) expect(tiles.has(id)).toBe(false);
  });

  it('parses a wire snapshot whose tile is __proto__', () => {
    const event = parseLayoutEvent(
      JSON.parse(
        '{"kind":"snapshot","hostId":"h","request":null,"snapshot":{"space":"/s","layoutRevision":"1",' +
          '"root":{"type":"tile","id":"__proto__","panels":[],"active":null},' +
          '"readOnly":false,"error":null,"dropped":null}}',
      ),
    );
    if (event.kind !== 'snapshot') throw new Error('expected a snapshot');
    expect(layoutTiles(event.snapshot.root).get('__proto__')).toEqual({ panels: [], active: null });
  });
});
